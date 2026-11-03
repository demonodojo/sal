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
