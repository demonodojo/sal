use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_source;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}


fn link_lock() {
    // Each link gets its own `a.out.<pid>.<n>`, so the tests need no lock to run in parallel.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe { std::env::set_var("SAL_OUT_UNIQUE", "1") });
}

fn compile_list(src: &str, instrument: bool) -> PathBuf {
    compile_list_opts(src, instrument, false)
}

fn compile_list_opts(src: &str, instrument: bool, release: bool) -> PathBuf {
    link_lock();
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
    bin
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

/// `list_push_share` of a flat `list_get` must copy slots without freeing the borrow
/// (an interior pointer). Freeing it was `free(): invalid pointer` in stage1.
#[test]
fn list_push_share_of_flat_get_does_not_free_borrow() {
    let src = r#"
struct Pair
    a: Int
    b: String

fn main() -> Int ! alloc
    xs = list_new[Pair]()
    xs.push(Pair(1, "a"))
    xs.push(Pair(2, "b"))
    ys = list_new[Pair]()
    list_push_share(ys, xs.get(0))
    list_push_share(ys, xs.get(1))
    # `ys` aliases the fields; free only its shell so `xs` still owns them.
    list_free_shell(ys)
    xs.get(0).a + xs.get(1).a
"#;
    let bin = compile_list(src, false);
    let out = Command::new(&bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(3),
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A `List[Struct]` keeps its slots inline: `set` replaces the element in place, `get_mut`
/// borrows a pointer into the list, and `pop` hands out an owned copy of the slots.
#[test]
fn list_of_structs_is_flat_and_pop_owns_its_copy() {
    let src = r#"
struct Pair
    a: Int
    b: String

fn main() -> Int ! alloc
    xs = list_new[Pair]()
    xs.push(Pair(1, int_to_str(2)))
    xs.set(0, Pair(7, int_to_str(8)))
    p = xs.get_mut(0)
    p.a = 40
    n = p.a
    q = xs.pop()
    n + q.b.len()
"#;
    let bin = compile_list(src, false);
    let out = Command::new(&bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(41),
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
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
    assert!(stderr.contains("OOB"), "stderr missing OOB: {stderr}");
}
