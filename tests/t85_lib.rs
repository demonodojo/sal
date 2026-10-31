use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn compile_and_run(sal_file: &PathBuf) -> i32 {
    let out = root()
        .join("target")
        .join("sal-out")
        .join("tests")
        .join(sal_file.file_stem().unwrap());
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: Some(out),
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(sal_file, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    Command::new(bin)
        .output()
        .expect("run")
        .status
        .code()
        .unwrap_or(99)
}

#[test]
fn sal_lib_unit_tests() {
    let dir = root().join("tests/sal");
    for name in [
        "test_json_esc.sal",
        "test_glob.sal",
        "test_sse.sal",
        "test_http_chunk.sal",
    ] {
        let path = dir.join(name);
        let code = compile_and_run(&path);
        assert_eq!(code, 0, "{name} failed with exit {code}");
    }
}

#[test]
fn sal_http_mock_integration() {
    let mock = root().join("../opencode-sal/tests/mock_llm.py");
    if !mock.is_file() {
        eprintln!("skip sal_http_mock: mock_llm.py not found");
        return;
    }
    let port = 17422u16;
    let guion = root().join("../opencode-sal/tests/guiones/read_then_stop.json");
    let mut child = Command::new("python3")
        .arg(&mock)
        .arg(port.to_string())
        .arg(&guion)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mock");
    std::thread::sleep(Duration::from_millis(400));
    let path = root().join("tests/sal/test_http_mock.sal");
    let bin_dir = root().join("target/sal-out/tests");
    std::fs::create_dir_all(&bin_dir).ok();
    let out = bin_dir.join("test_http_mock");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: Some(out.clone()),
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    compile_file(&path, &opts).expect("compile mock test");
    let code = Command::new(out)
        .env("OC_MOCK_PORT", port.to_string())
        .output()
        .expect("run")
        .status
        .code()
        .unwrap_or(99);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(code, 0, "test_http_mock exit {code}");
}
