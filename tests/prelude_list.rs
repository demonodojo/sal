use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use sal_compiler::compile::compile_source;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `compile_source` links to a shared out path; serialize parallel list tests.
static LINK_LOCK: Mutex<()> = Mutex::new(());

fn link_lock() -> std::sync::MutexGuard<'static, ()> {
    LINK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn compile_list(src: &str, instrument: bool) -> PathBuf {
    compile_list_opts(src, instrument, false)
}

fn compile_list_opts(src: &str, instrument: bool, release: bool) -> PathBuf {
    let _guard = link_lock();
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release,
        instrument,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile list program");
    let bin = art.binary.expect("binary");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let copy = PathBuf::from(format!(
        "/tmp/sal-list-run-{}-{}",
        std::process::id(),
        stamp
    ));
    std::fs::copy(&bin, &copy).expect("copy binary");
    copy
}

/// Build a two-element `List[Int]` and return `list_len` (must be 2, not a hardcoded 2).
#[test]
fn list_len_of_two_ints_is_2() {
    let src = r#"
fn main() -> Int ! alloc
    xs = list_new[Int]()
    list_push(xs, 1)
    list_push(xs, 2)
    list_len(xs)
"#;
    let bin = compile_list(src, false);
    let out = Command::new(&bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// In-range `list_get` must not abort; exit code is the element (1 at index 0).
#[test]
fn list_get_index_0_returns_first_element() {
    let src = r#"
fn main() -> Int ! alloc
    xs = list_new[Int]()
    list_push(xs, 1)
    list_push(xs, 2)
    list_get(xs, 0)
"#;
    let bin = compile_list(src, false);
    let out = Command::new(&bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Out-of-range `list_get` aborts with non-zero exit even without `--instrument`.
#[test]
fn list_get_oob_exits_nonzero() {
    let src = r#"
fn main() -> Int ! alloc
    xs = list_new[Int]()
    list_push(xs, 1)
    list_push(xs, 2)
    list_get(xs, 5)
"#;
    let bin = compile_list(src, false);
    let out = Command::new(&bin).output().expect("run");
    assert_ne!(
        out.status.code(),
        Some(0),
        "expected non-zero exit on OOB; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Growing by `push` reuses the buffer with `realloc`: the peak RSS stays near
/// the final 32 MB instead of also keeping every previous buffer (~64 MB).
#[test]
fn list_push_growth_does_not_keep_old_buffers() {
    const N: u64 = 4_000_001; // final buffer: 32 MB; exit = N & 255 = 1
    // Worst case of a copying realloc (old 16 MB + new 32 MB) still fits here;
    // keeping the old buffers peaks at ~64 MB.
    const CAP_KB: u64 = 56_000;
    let src = r#"
fn main() -> Int ! alloc
    xs = List.new[Int]()
    i = 0
    while i < 4000001
        xs.push(i)
        i = i + 1
    xs.len() & 255
"#;
    let bin = compile_list_opts(src, false, true);
    let log = std::env::temp_dir().join(format!("sal-vec-rss-{}", std::process::id()));
    let out = Command::new("/usr/bin/time")
        .args(["-f", "%M", "-o"])
        .arg(&log)
        .arg(&bin)
        .output()
        .expect("time the list program");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    let rss = text
        .lines()
        .rev()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .unwrap_or(0);
    assert_eq!(
        out.status.code(),
        Some((N & 255) as i32),
        "push/len wrong; stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        rss > 0 && rss < CAP_KB,
        "peak RSS {rss} KB for a 32 MB list; old buffers are being kept (cap {CAP_KB} KB)"
    );
}

/// With `--instrument`, illegal `list_get` emits `OOB` and exits non-zero.
#[test]
fn list_get_oob_instrument_emits_oob() {
    let src = r#"
fn main() -> Int ! alloc
    xs = list_new[Int]()
    list_push(xs, 1)
    list_push(xs, 2)
    list_get(xs, 5)
"#;
    let bin = compile_list(src, true);
    let out = Command::new(&bin).output().expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit on OOB");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("OOB"),
        "stderr missing OOB: {stderr}"
    );
}
