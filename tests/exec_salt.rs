use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// Minimal little-endian .salt: magic SALT, version u16, elem u8, rank u8, dims u64[], payload f32.
fn write_minimal_salt(path: &PathBuf, dims: &[u64], payload: &[f32]) {
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

#[test]
fn load_salt_via_runtime() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = tempfile::tempdir().expect("tmpdir");
    let salt_path = tmp.path().join("w.salt");
    write_minimal_salt(&salt_path, &[2, 2], &[1.0, 2.0, 3.0, 4.0]);

    let c_src = tmp.path().join("salt_load_test.c");
    let bin = tmp.path().join("salt_load_test");
    let salt_c_path = salt_path.to_string_lossy().replace('\\', "\\\\");
    fs::write(
        &c_src,
        format!(
            r#"
#include "{root}/runtime/sal_runtime.h"
#include <assert.h>
#include <math.h>
#include <stdlib.h>
#include <stdio.h>

int main(void) {{
    int64_t n = 0;
    float *buf = (float *)sal_load_f32("{salt}", &n);
    assert(buf != NULL);
    assert(n == 4);
    assert(fabsf(buf[0] - 1.f) < 1e-6f);
    assert(fabsf(buf[1] - 2.f) < 1e-6f);
    assert(fabsf(buf[2] - 3.f) < 1e-6f);
    assert(fabsf(buf[3] - 4.f) < 1e-6f);
    free(buf);
    return 0;
}}
"#,
            root = root.display(),
            salt = salt_c_path,
        ),
    )
    .expect("write c");

    let status = Command::new("clang")
        .arg(&c_src)
        .arg(root.join("runtime/sal_runtime.c"))
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("clang");
    assert!(status.success(), "clang failed to link sal_runtime.c");

    let out = Command::new(&bin).output().expect("run");
    assert!(
        out.status.success(),
        "salt load failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn reject_bad_magic() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = tempfile::tempdir().expect("tmpdir");
    let salt_path = tmp.path().join("bad.salt");
    fs::write(&salt_path, b"XXXX").unwrap();

    let c_src = tmp.path().join("salt_bad_test.c");
    let bin = tmp.path().join("salt_bad_test");
    let salt_c_path = salt_path.to_string_lossy().replace('\\', "\\\\");
    fs::write(
        &c_src,
        format!(
            r#"
#include "{root}/runtime/sal_runtime.h"
#include <assert.h>
#include <stddef.h>
int main(void) {{
    int64_t n = 99;
    void *buf = sal_load_f32("{salt}", &n);
    assert(buf == NULL);
    return 0;
}}
"#,
            root = root.display(),
            salt = salt_c_path,
        ),
    )
    .unwrap();

    let status = Command::new("clang")
        .arg(&c_src)
        .arg(root.join("runtime/sal_runtime.c"))
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("clang");
    assert!(status.success());
    let out = Command::new(&bin).output().expect("run");
    assert!(out.status.success());
}
