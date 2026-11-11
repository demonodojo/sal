//! The String primitives of the runtime under ASan + LSan + UBSan
//! (`tests/support/sal_str_immutable.c`): concatenation does not change its operands, the
//! accumulator reuses only its own block, and nothing leaks or is used after free.
use std::path::PathBuf;
use std::process::Command;

#[test]
fn sal_str_primitives_keep_operands_and_memory_clean() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("tests/support/sal_str_immutable.c");
    let runtime = root.join("runtime/sal_runtime.c");
    let out = std::env::temp_dir().join(format!("sal_str_immutable_{}", std::process::id()));
    let status = Command::new("clang")
        .args([
            "-fsanitize=address,undefined",
            "-fno-sanitize-recover=undefined",
            "-g",
            "-O1",
            "-I",
            root.join("runtime").to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            src.to_str().unwrap(),
            runtime.to_str().unwrap(),
            "-lm",
        ])
        .status()
        .expect("clang");
    if !status.success() {
        if std::env::var_os("SAL_REQUIRE_ASAN").is_some() {
            panic!("clang -fsanitize=address failed to build the harness");
        }
        eprintln!("skipping: clang ASan build failed");
        return;
    }
    let run = Command::new(&out)
        .env("ASAN_OPTIONS", "detect_leaks=1")
        .output()
        .expect("run harness");
    let _ = std::fs::remove_file(&out);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(0), "harness failed (exit = failing line):\n{stderr}");
}
