use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn framebuffer_fill_copy_release() {
    let sal_src = root().join("benches/framebuffer/fill_copy.sal");
    let out_dir = root().join("target/test-bench-framebuffer");
    std::fs::create_dir_all(&out_dir).expect("mkdir");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: true,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&sal_src, &opts).expect("compile framebuffer bench");
    let bin = art.binary.expect("sal binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(5));
}
