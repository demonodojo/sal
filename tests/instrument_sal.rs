use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `compile_file` always links to `target/sal-out/a.out`; serialize + copy so
/// parallel instrument tests do not race on that path (ETXTBSY / wrong binary).
static LINK_LOCK: Mutex<()> = Mutex::new(());

fn instrument_opts() -> CompileOptions {
    CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: true,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    }
}

fn compile_instrumented(file: &Path) -> (String, PathBuf) {
    let _guard = LINK_LOCK.lock().expect("link lock");
    let art = compile_file(file, &instrument_opts()).expect("compile instrumented");
    let bin = art.binary.expect("binary");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let copy = PathBuf::from(format!(
        "/tmp/sal-inst-run-{}-{}",
        std::process::id(),
        stamp
    ));
    std::fs::copy(&bin, &copy).expect("copy binary");
    (art.llvm, copy)
}

/// Instrumented build reserves host tensors and does not free them before
/// `sal_instrument_shutdown`, so stderr must contain LEAK.
#[test]
fn instrument_sal_leak() {
    let file = root().join("examples/cpu_matmul.sal");
    let (llvm, bin) = compile_instrumented(&file);
    assert!(
        llvm.contains("sal_instrument_malloc")
            && llvm.contains("sal_instrument_shutdown"),
        "expected instrument probes:\n{llvm}"
    );
    let out = Command::new(&bin).output().expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit on LEAK");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LEAK"),
        "stderr missing LEAK: {stderr}"
    );

    // Also exercise CLI `--instrument-out` via `sal run`.
    let out_path = PathBuf::from("/tmp/sal-instrument-out-wiring.json");
    let sal = env!("CARGO_BIN_EXE_sal");
    let _guard = LINK_LOCK.lock().expect("link lock");
    let status = Command::new(sal)
        .arg("run")
        .arg(&file)
        .arg("--instrument")
        .arg("--instrument-out")
        .arg(&out_path)
        .current_dir(root())
        .output()
        .expect("sal run");
    drop(_guard);
    assert_ne!(status.status.code(), Some(0));
    let saved = std::fs::read_to_string(&out_path).expect("read instrument-out");
    assert!(
        saved.contains("LEAK") || String::from_utf8_lossy(&status.stderr).contains("LEAK"),
        "instrument-out={saved} stderr={}",
        String::from_utf8_lossy(&status.stderr)
    );
}

/// `index(collection, i)` with `--instrument` must call `sal_instrument_check_index`
/// and exit non-zero with OOB (IR has no List lengths; LLVM uses a small bound).
#[test]
fn instrument_sal_oob() {
    let file = root().join("examples/instrument_oob.sal");
    let (llvm, bin) = compile_instrumented(&file);
    assert!(
        llvm.contains("sal_instrument_check_index"),
        "expected check_index probe:\n{llvm}"
    );
    let out = Command::new(&bin)
        .current_dir(root())
        .output()
        .expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit on OOB");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("OOB"),
        "stderr missing OOB: {stderr}"
    );
}

/// `load` of a .salt with f32 NaN bits must hit `sal_instrument_check_f32` and emit NAN.
#[test]
fn instrument_sal_nan() {
    let file = root().join("examples/instrument_nan.sal");
    let (llvm, bin) = compile_instrumented(&file);
    assert!(
        llvm.contains("sal_instrument_check_f32")
            && llvm.contains("sal_load_f32"),
        "expected load + check_f32:\n{llvm}"
    );
    let out = Command::new(&bin)
        .current_dir(root())
        .output()
        .expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit on NAN");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("NAN"),
        "stderr missing NAN: {stderr}"
    );
}

fn run_strict(src: &str, tag: &str) -> std::process::Output {
    let file = PathBuf::from(format!("/tmp/sal-strict-{}-{tag}.sal", std::process::id()));
    std::fs::write(&file, src).expect("write source");
    let (_llvm, bin) = compile_instrumented(&file);
    Command::new(&bin)
        .env("SAL_INSTRUMENT_STRICT", "1")
        .current_dir(root())
        .output()
        .expect("run")
}

/// Locals the compiler frees at scope end leave no live block, even in strict mode.
#[test]
fn instrument_strict_freed_locals_are_clean() {
    let out = run_strict(
        "enum Shape\n    Dot\n    Rect(Int, Int)\n    Tag(String, Int)\n\nfn main() -> Int ! alloc\n    s = int_to_str(12345) + \"x\"\n    xs = list_new[String]()\n    _a = list_push(xs, int_to_str(7))\n    d = dict_new[String, String]()\n    _b = dict_put(d, \"k\", int_to_str(9))\n    e = Tag(int_to_str(3), 4)\n    n = match e\n        Tag(t, k) => str_len(t) + k\n        Dot => 0\n        Rect(w, h) => w\n    str_len(s) + list_len(xs) + n\n",
        "clean",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
}

/// Pages of an `@arena` call are freed as one block, so strict mode reports no LEAK.
#[test]
fn instrument_strict_arena_pages_are_clean() {
    let out = run_strict(
        "@arena\nfn work(req: String) -> Int ! alloc\n    s = req + \"!\"\n    xs = list_new[Int]()\n    _a = list_push(xs, str_len(s))\n    str_len(s)\n\nfn main() -> Int ! alloc\n    work(\"ab\")\n",
        "arena",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.code().is_some(), "arena crashed: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "arena double free: {stderr}");
}

/// A string handed to a user function that returns it is not freed by the caller
/// (the result has unknown provenance), and strict mode reports it.
#[test]
fn instrument_strict_reports_unfreed_string() {
    let out = run_strict(
        "fn keep(s: String) -> String\n    s\n\nfn main() -> Int ! alloc\n    s = int_to_str(12345)\n    t = keep(s)\n    str_len(t)\n",
        "leak",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("LEAK"), "expected a leak report: {stderr}");
}

/// Lists of enums own their boxes (`list_free` releases them, `list_set` replaces
/// one). `list_get` borrows the element, so the local is not released again.
#[test]
fn instrument_strict_enum_lists_are_clean() {
    for (tag, file) in [
        ("enum_owned", "drops/drop_list_enum_owned.sal"),
        ("enum_nested", "drops/drop_list_enum_nested.sal"),
    ] {
        let src = std::fs::read_to_string(root().join("tests/lang").join(file)).expect("read");
        let out = run_strict(&src, tag);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("LEAK"), "{file}: unexpected leak: {stderr}");
    }
}

/// A value popped from an owned list is a fresh box the local releases. A `list_get` read
/// straight into a `match` borrows the element: the list releases it, and strict mode is clean.
#[test]
fn instrument_strict_list_pop_and_match_temps_are_clean() {
    for (tag, file) in [
        ("pop_enum", "drops/drop_list_pop_enum.sal"),
        ("match_temp", "drops/drop_match_list_get_temp.sal"),
    ] {
        let src = std::fs::read_to_string(root().join("tests/lang").join(file)).expect("read");
        let out = run_strict(&src, tag);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("LEAK"), "{file}: unexpected leak: {stderr}");
    }
}

/// An owned struct list frees its boxes. `list_get` borrows, so a read passed to a function
/// that only looks at a field is not copied and not freed again.
#[test]
fn instrument_strict_struct_list_box_pass_fn_is_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_list_struct_box_pass_fn.sal"))
        .expect("read");
    let out = run_strict(&src, "struct_box_fn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("struct_copy"), "struct_copy leak: {stderr}");
    assert_eq!(out.status.code(), Some(8), "stderr: {stderr}");
}

#[test]
fn instrument_strict_struct_list_boxes_are_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_list_struct_fields_copy.sal"))
        .expect("read");
    let out = run_strict(&src, "struct_list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(8), "stderr: {stderr}");
}

/// A struct popped from an owned struct list is a box the local releases: strict mode is clean.
#[test]
fn instrument_strict_list_pop_struct_is_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_list_pop_struct.sal")).expect("read");
    let out = run_strict(&src, "pop_struct");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
}

/// A discarded `pop` is a fresh value the statement releases. A discarded `get` only borrows.
#[test]
fn instrument_strict_discarded_fresh_values_are_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_discard_fresh.sal")).expect("read");
    let out = run_strict(&src, "discard_fresh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(4), "stderr: {stderr}");
}

/// Pushing a borrowed element copies it. Both lists free their own box: no leak, no double free.
#[test]
fn instrument_strict_list_get_pushed_copies_once() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    ys = list_new[P]()\n    list_push(ys, o)\n    list_len(xs) + list_len(ys)\n";
    let out = run_strict(src, "struct_push");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
}

/// Returning a borrowed element copies it. The caller frees that copy once.
#[test]
fn instrument_strict_list_get_returned_is_freed_once() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn give(xs: List[P]) -> P ! alloc\n    list_get(xs, 0)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    p = give(xs)\n    p.n\n";
    let out = run_strict(src, "struct_ret");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// Reading a field of a borrowed struct does not copy it.
#[test]
fn instrument_strict_list_get_field_read_is_clean() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn read_n(p: P) -> Int ! alloc\n    p.n\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    read_n(o)\n";
    let out = run_strict(src, "struct_read");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// Rebuilding a borrowed struct from its fields shares the pointers. No copy, no leak.
#[test]
fn instrument_strict_list_get_rebuilt_is_clean() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: P) -> P ! alloc\n    P(p.name, p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    q.n\n";
    let out = run_strict(src, "struct_rebuild");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// Pushing the rebuilt alias copies it once. Both lists free their own strings.
#[test]
fn instrument_strict_list_get_rebuilt_then_pushed_is_clean() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: P) -> P ! alloc\n    P(p.name, p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    ys = list_new[P]()\n    list_push(ys, q)\n    list_len(xs) + list_len(ys)\n";
    let out = run_strict(src, "struct_rebuild_push");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
}

/// `o.n = v` on a list element writes that element.
#[test]
fn instrument_strict_field_write_updates_the_list_element() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    o.n = 9\n    p = list_get(xs, 0)\n    p.n\n";
    let out = run_strict(src, "struct_field_write");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(9), "stderr: {stderr}");
}

/// A callee that stores the struct takes it: the caller copies, and that copy is freed once.
#[test]
fn instrument_strict_list_get_taken_is_freed_once() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn keep(p: P) -> Int ! alloc\n    ys = list_new[P]()\n    list_push(ys, p)\n    list_len(ys)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    keep(o)\n";
    let out = run_strict(src, "struct_take");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// A user fn whose every result is a fresh String hands its caller an owned value: the caller
/// releases it, however it is used.
#[test]
fn instrument_strict_user_fn_string_is_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_user_fn_string_owned.sal")).expect("read");
    let out = run_strict(&src, "user_fn_string");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
}
