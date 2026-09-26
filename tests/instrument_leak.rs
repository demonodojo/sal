use std::path::PathBuf;
use std::process::Command;

#[test]
fn instrument_nan_exits_nonzero() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("tests/support/nan_test.c");
    let bins = PathBuf::from("/tmp/sal-instrument-bins");
    std::fs::create_dir_all(&bins).unwrap();
    let bin = bins.join("instrument_leak_nan");
    Command::new("clang")
        .arg(&src)
        .arg(root.join("runtime/instrument.c"))
        .arg("-DSAL_INSTRUMENT=1")
        .arg("-o")
        .arg(&bin)
        .arg("-lm")
        .status()
        .expect("clang");
    let out = Command::new(&bin).output().expect("run");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NAN"), "{stderr}");
}
