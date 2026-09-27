use std::path::PathBuf;
use std::process::Command;

#[test]
fn matmul_kernel_numeric() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("tests/support/matmul_test.c");
    let bin = root.join("target/test-matmul");
    std::fs::create_dir_all(root.join("target")).unwrap();
    let status = Command::new("clang")
        .arg("-I")
        .arg(root.join("runtime"))
        .arg(&src)
        .arg(root.join("runtime/kernels.c"))
        .arg(root.join("runtime/gpu_driver.c"))
        .arg(root.join("tests/support/place_stubs.c"))
        .arg("-o")
        .arg(&bin)
        .arg("-lm")
        .status()
        .expect("clang");
    assert!(status.success());
    let out = Command::new(&bin).output().expect("run");
    assert!(out.status.success());
}
