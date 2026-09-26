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

fn compile_list(src: &str, instrument: bool) -> PathBuf {
    let _guard = LINK_LOCK.lock().expect("link lock");
    let opts = CompileOptions {
        release: false,
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
    xs = list_push(list_push(list_new(), 1), 2)
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
    xs = list_push(list_push(list_new(), 1), 2)
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
    xs = list_push(list_push(list_new(), 1), 2)
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

/// With `--instrument`, illegal `list_get` emits `OOB` and exits non-zero.
#[test]
fn list_get_oob_instrument_emits_oob() {
    let src = r#"
fn main() -> Int ! alloc
    xs = list_push(list_push(list_new(), 1), 2)
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
