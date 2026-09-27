use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn standard_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_standard"))
}

fn run_standard(args: &[&str]) -> std::process::Output {
    Command::new(standard_bin())
        .args(args)
        .output()
        .expect("run standard")
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(root().join("standard/fixtures").join(name)).expect("read fixture")
}

#[test]
fn standard_check_reports_dupstring() {
    let path = root()
        .join("standard/fixtures/dupstring_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Style/DupString"), "stderr: {err}");
}

#[test]
fn standard_check_clean_on_skip_fixture() {
    let path = root()
        .join("standard/fixtures/dupstring_skip_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn standard_fix_dupstring() {
    let path = root()
        .join("standard/fixtures/dupstring_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["fix", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let got = String::from_utf8_lossy(&out.stdout);
    let want = read_fixture("dupstring_out.sal");
    assert_eq!(got, want);
    let out2 = run_standard(&["fix", &path]);
    assert_eq!(String::from_utf8_lossy(&out2.stdout), got);
}

#[test]
fn standard_fix_elsif() {
    let path = root()
        .join("standard/fixtures/elsif_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["fix", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let got = String::from_utf8_lossy(&out.stdout);
    let want = read_fixture("elsif_out.sal");
    assert_eq!(got, want);
}

#[test]
fn standard_fix_elsif_two_passes() {
    let path = root()
        .join("standard/fixtures/elsif_two_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["fix", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let got = String::from_utf8_lossy(&out.stdout);
    let want = read_fixture("elsif_two_out.sal");
    assert_eq!(got, want);
}

#[test]
fn standard_fix_preserves_comment() {
    let path = root()
        .join("standard/fixtures/comment_survives_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["fix", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let got = String::from_utf8_lossy(&out.stdout);
    let want = read_fixture("comment_survives_out.sal");
    assert_eq!(got, want);
}

#[test]
fn standard_else_multi_not_auto_fixed() {
    let path = root()
        .join("standard/fixtures/else_multi_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}
