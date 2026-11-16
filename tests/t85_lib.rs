use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::thread::JoinHandle;

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
        "test_env.sal",
    ] {
        let path = dir.join(name);
        let code = compile_and_run(&path);
        assert_eq!(code, 0, "{name} failed with exit {code}");
    }
}

/// Minimal HTTP server for one request: answers with a chat-completion JSON body.
fn spawn_mock() -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept mock");
        let mut buf = [0u8; 8192];
        let mut request = Vec::new();
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match conn.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }
        let body = r#"{"id":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = conn.write_all(response.as_bytes());
        let _ = conn.shutdown(std::net::Shutdown::Write);
    });
    (port, handle)
}

#[test]
fn sal_http_mock_integration() {
    let (port, server) = spawn_mock();
    let path = root().join("tests/sal/http_mock_integration.sal");
    let bin_dir = root().join("target/sal-out/tests");
    std::fs::create_dir_all(&bin_dir).ok();
    let out = bin_dir.join("http_mock_integration");
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
    server.join().expect("mock server");
    assert_eq!(code, 0, "http_mock_integration exit {code}");
}
