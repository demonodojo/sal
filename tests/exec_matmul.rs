use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Same operands as `tests/support/matmul_test.c`: A×I = A → elems 1,2,3,4 (sum 10).
#[test]
fn exec_matmul_2x2_exit_sum() {
    let file = root().join("examples/cpu_matmul.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile cpu_matmul.sal");
    assert!(
        art.llvm.contains("call void @sal_matmul_f32")
            && art.llvm.contains("i64 2, i64 2, i64 2"),
        "expected real dims in LLVM:\n{}",
        art.llvm
    );
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(10),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
