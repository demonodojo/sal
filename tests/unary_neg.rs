use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile_source;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn unary_neg_literal_in_fn() {
    let src = r#"
fn neg1() -> Int
    -1

fn main() -> Int
    neg1() + 3
"#;
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: true,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    assert!(
        art.llvm.contains("sub i64 0,") || art.llvm.contains("sub i64 0, "),
        "expected unary neg as sub i64 0, x; got:\n{}",
        art.llvm
    );
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(2),
        "neg1()+3 should be 2 (exit code 2)"
    );
}
