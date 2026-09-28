use std::path::PathBuf;
use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn exec_column_cpu_builds_and_runs() {
    let file = root().join("examples/column_cpu.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: true,
    };
    let art = compile_file(&file, &opts).expect("compile column_cpu.sal");
    assert!(
        art.llvm.contains("sal_tensor_bin_f32"),
        "expected tensor bin in LLVM:\n{}",
        art.llvm
    );
}
