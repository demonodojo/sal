use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Minimal little-endian .salt: magic SALT, version u16, elem u8, rank u8, dims u64[], payload f32.
fn write_minimal_salt(path: &PathBuf, dims: &[u64], payload: &[f32]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir salt fixture");
    }
    let mut f = fs::File::create(path).expect("create salt");
    f.write_all(b"SALT").unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap(); // version
    f.write_all(&[0u8]).unwrap(); // elem = F32
    f.write_all(&[dims.len() as u8]).unwrap(); // rank
    for d in dims {
        f.write_all(&d.to_le_bytes()).unwrap();
    }
    for v in payload {
        f.write_all(&v.to_le_bytes()).unwrap();
    }
}

/// Same operands as `tests/exec_matmul.rs` / `matmul_test.c`, loaded from `.salt`.
/// A×I = A → elems 1,2,3,4 (sum 10).
#[test]
fn exec_salt_matmul_2x2_exit_sum() {
    let fixtures = root().join("target/salt_matmul");
    write_minimal_salt(
        &fixtures.join("a.salt"),
        &[2, 2],
        &[1.0, 2.0, 3.0, 4.0],
    );
    write_minimal_salt(
        &fixtures.join("b.salt"),
        &[2, 2],
        &[1.0, 0.0, 0.0, 1.0],
    );

    let file = root().join("examples/salt_matmul.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile salt_matmul.sal");
    assert!(
        art.llvm.contains("sal_load_f32")
            && art.llvm.contains("call void @sal_matmul_f32")
            && art.llvm.contains("i64 2, i64 2, i64 2"),
        "expected load + matmul with real dims in LLVM:\n{}",
        art.llvm
    );
    let bin = art.binary.expect("binary");
    let out = Command::new(bin)
        .current_dir(root())
        .output()
        .expect("run");
    assert_eq!(
        out.status.code(),
        Some(10),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
