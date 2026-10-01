use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn speed_work_sal_matches_rust_output() {
    let rust_src = root().join("benches/speed/work.rs");
    let sal_src = root().join("benches/speed/work.sal");
    let out_dir = root().join("target/test-bench-runtime");
    std::fs::create_dir_all(&out_dir).expect("mkdir");
    let rust_bin = out_dir.join("work-rust");

    let rustc = Command::new("rustc")
        .args(["-C", "opt-level=3", rust_src.to_str().unwrap(), "-o"])
        .arg(&rust_bin)
        .status()
        .expect("rustc");
    assert!(rustc.success(), "rustc failed to build benches/speed/work.rs");

    let opts = CompileOptions {
        release: true,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&sal_src, &opts).expect("compile work.sal");
    let sal_bin = art.binary.expect("sal binary");

    let rust_out = Command::new(&rust_bin).output().expect("run rust");
    let sal_out = Command::new(&sal_bin).output().expect("run sal");

    assert_eq!(
        rust_out.status.code(),
        sal_out.status.code(),
        "exit codes must match (sync OUTER/INNER in work.rs and work.sal)"
    );
}
