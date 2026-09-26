use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_source;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Batch axis typed as `?` but resolved by a 1×2 literal → LLVM must pass i64 1,2,2
/// (not three zeros, and not skip the call). Product vs identity sums to 3.
#[test]
fn exec_dyn_batch1_literal_dims() {
    let src = r#"
fn main() -> Int ! alloc
    let a: Tensor[F32, ?, 2] on cpu = tensor[[1.0, 2.0]]
    let b = tensor[[1.0, 0.0], [0.0, 1.0]]
    on cpu
        matmul(a, b)
    0
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile dyn batch-1");
    assert!(
        art.llvm.contains("call void @sal_matmul_f32")
            && art.llvm.contains("i64 1, i64 2, i64 2"),
        "expected concrete dims from literal-resolved `?`:\n{}",
        art.llvm
    );
    assert!(
        !art.llvm.contains("i64 0, i64 0, i64 0"),
        "must not emit zero placeholders:\n{}",
        art.llvm
    );
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(3),
        "1×2 × I → [1,2] sum 3; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
