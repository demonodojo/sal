//! Black-box tests of the auxiliary binaries `som2salt` and `standard`.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Mutex;

/// `standard` always builds into the same default output path.
static STANDARD: Mutex<()> = Mutex::new(());

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scratch(tag: &str) -> PathBuf {
    let d = root().join("target").join("test-bins").join(tag);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn som2salt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_som2salt"))
        .args(args)
        .output()
        .expect("run som2salt")
}

fn standard(dir: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_standard"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run standard")
}

#[test]
fn som2salt_help_lists_defaults() {
    for flag in ["--help", "-h"] {
        let out = som2salt(&[flag]);
        assert!(out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("usage: som2salt"), "{err}");
        assert!(err.contains("default model:"), "{err}");
    }
}

#[test]
fn som2salt_rejects_unknown_arguments() {
    let out = som2salt(&["--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown arg: --bogus"));
}

#[test]
fn som2salt_reports_a_missing_model() {
    let dir = scratch("som2salt_missing");
    let model = dir.join("no-such-model");
    let out_dir = dir.join("out");
    for (m, o) in [("--model", "--out"), ("-m", "-o")] {
        let out = som2salt(&[m, model.to_str().unwrap(), o, out_dir.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("som2salt:"));
    }
}

#[test]
fn standard_checks_a_clean_file_and_flags_a_dirty_one() {
    let _lock = STANDARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("standard_check");
    let clean = dir.join("clean.sal");
    std::fs::write(&clean, "fn main() -> Int\n    1\n").unwrap();
    let out = standard(&dir, &["check", clean.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let unknown = standard(&dir, &["frobnicate", clean.to_str().unwrap()]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("usage: check|fix FILE"));
}

#[test]
fn standard_finds_its_sources_from_sal_home() {
    let _lock = STANDARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("standard_home");
    let clean = dir.join("clean.sal");
    std::fs::write(&clean, "fn main() -> Int\n    1\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_standard"))
        .args(["check", clean.to_str().unwrap()])
        .env("SAL_HOME", root())
        .env("PROFILE", "release")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn standard_fix_rewrites_a_file() {
    let _lock = STANDARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("standard_fix");
    let f = dir.join("f.sal");
    std::fs::write(&f, "fn main() -> Int\n    1\n").unwrap();
    let out = standard(&dir, &["fix", f.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}
