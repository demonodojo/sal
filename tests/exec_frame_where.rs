use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn exec_frame_where_links_and_runs() {
    let file = root().join("tests/fixtures/frame_where.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile frame_where.sal");
    assert!(
        art.llvm.contains("sal_tensor_select_f32"),
        "expected where column select in LLVM:\n{}",
        art.llvm
    );
    let bin = art.binary.expect("linked binary");
    let out = Command::new(&bin)
        .output()
        .expect("run frame_where binary");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
