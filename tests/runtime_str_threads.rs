use std::path::PathBuf;
use std::process::Command;

#[test]
fn sal_str_threads_tsan_clean() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("tests/support/sal_str_threads.c");
    let runtime = root.join("runtime/sal_runtime.c");
    let out = std::env::temp_dir().join(format!("sal_str_threads_{}", std::process::id()));
    let status = Command::new("clang")
        .env("TSAN_OPTIONS", "report_thread_leaks=0")
        .args([
            "-fsanitize=thread",
            "-g",
            "-O1",
            "-I",
            root.join("runtime").to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            src.to_str().unwrap(),
            runtime.to_str().unwrap(),
            "-lpthread",
        ])
        .status()
        .expect("clang");
    if !status.success() {
        eprintln!("clang TSan build failed (is clang available?)");
        return;
    }
    let run = Command::new(&out)
        .env("TSAN_OPTIONS", "report_thread_leaks=0")
        .status()
        .expect("run harness");
    assert_eq!(run.code(), Some(0));
    let _ = std::fs::remove_file(&out);
}
