use std::path::{Path, PathBuf};
use std::process::Command;

fn bins_dir() -> PathBuf {
    let dir = PathBuf::from("/tmp/sal-instrument-bins");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn compile_support(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("tests/support").join(name);
    let bin = bins_dir().join(name.trim_end_matches(".c"));
    let status = Command::new("clang")
        .arg(&src)
        .arg(root.join("runtime/instrument.c"))
        .arg("-DSAL_INSTRUMENT=1")
        .arg("-o")
        .arg(&bin)
        .arg("-lm")
        .status()
        .expect("clang");
    assert!(status.success(), "clang failed for {name}");
    bin
}

fn run_expect_event(bin: &Path, code: &str) {
    let out = Command::new(bin).output().expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit for {code}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(code),
        "stderr missing {code}: {stderr}"
    );
}

#[test]
fn instrument_leak_event() {
    let bin = compile_support("leak_test.c");
    run_expect_event(&bin, "LEAK");
}

#[test]
fn instrument_oob_event() {
    let bin = compile_support("oob_test.c");
    run_expect_event(&bin, "OOB");
}

#[test]
fn instrument_oob_redzone_event() {
    let bin = compile_support("oob_redzone_test.c");
    run_expect_event(&bin, "OOB");
}

#[test]
fn instrument_use_after_free_event() {
    let bin = compile_support("uaf_test.c");
    run_expect_event(&bin, "USE_AFTER_FREE");
}

#[test]
fn instrument_double_free_event() {
    let bin = compile_support("double_free_test.c");
    run_expect_event(&bin, "DOUBLE_FREE");
}

#[test]
fn instrument_bad_place_event() {
    let bin = compile_support("bad_place_test.c");
    run_expect_event(&bin, "BAD_PLACE");
}

#[test]
fn instrument_nan_event() {
    let bin = compile_support("nan_test.c");
    run_expect_event(&bin, "NAN");
}

#[test]
fn instrument_inf_event() {
    let bin = compile_support("inf_test.c");
    run_expect_event(&bin, "INF");
}

#[test]
fn instrument_clean_exits_zero() {
    let bin = compile_support("clean_test.c");
    let out = Command::new(&bin).output().expect("run");
    assert_eq!(out.status.code(), Some(0), "stderr={}", String::from_utf8_lossy(&out.stderr));
}
