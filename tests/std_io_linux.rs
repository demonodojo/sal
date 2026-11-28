use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn std_io_smoke_linux() {
    let file = root().join("examples/std_io_smoke.sal");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile std_io_smoke");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Issue #3: the Linux host maps `O_EXCL`, so a second `O_CREAT + O_EXCL` open of the same path
/// fails instead of succeeding (`tests/sal/test_open_excl.sal` exits 0 only then).
#[test]
fn open_excl_fails_when_the_file_exists() {
    let file = root().join("tests/sal/test_open_excl.sal");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile test_open_excl");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
