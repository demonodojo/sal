use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn hello_runs_42() {
    let file = root().join("examples/hello.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(42));
}

#[test]
fn incremental_cache_second_build() {
    let file = root().join("examples/hello.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: true,
    };
    let a = compile_file(&file, &opts).expect("first");
    let b = compile_file(&file, &opts).expect("second");
    assert!(b.cache_hit || a.cache_hit);
}
