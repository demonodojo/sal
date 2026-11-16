use std::path::PathBuf;
use std::process::Command;

/// Builds `tests/support/<harness>.c` with the runtime under TSan and runs it; it must exit 0
/// with no data race reported. Skipped when clang cannot build a TSan binary.
fn run_tsan_harness(harness: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join(format!("tests/support/{harness}.c"));
    let runtime = root.join("runtime/sal_runtime.c");
    let out = std::env::temp_dir().join(format!("{harness}_{}", std::process::id()));
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
            "-pthread",
        ])
        .status()
        .expect("clang");
    if !status.success() {
        eprintln!("clang TSan build failed (is clang available?)");
        return;
    }
    let run = Command::new(&out)
        .env("TSAN_OPTIONS", "report_thread_leaks=0 halt_on_error=1")
        .output()
        .expect("run harness");
    let _ = std::fs::remove_file(&out);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!stderr.contains("ThreadSanitizer"), "{harness}: {stderr}");
    assert_eq!(run.status.code(), Some(0), "{harness}: {stderr}");
}

#[test]
fn sal_str_threads_tsan_clean() {
    run_tsan_harness("sal_str_threads");
}

/// SPEC «Hilos»: threads, Semaphore, Mutex and Channel, and the tensor place heaps and fused
/// regions that several threads use at once.
#[test]
fn sal_sync_threads_tsan_clean() {
    run_tsan_harness("sal_sync_threads");
}
