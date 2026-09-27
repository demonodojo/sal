use std::fs;
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
    fs::read_to_string(root().join("standard/fixtures").join(name)).expect("read fixture")
}

fn fix_in_temp(input: &str) -> (tempfile::TempDir, String, std::process::Output) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("input.sal");
    fs::write(&path, input).expect("write temp input");
    let path_str = path.to_string_lossy().into_owned();
    let out = run_standard(&["fix", &path_str]);
    let got = fs::read_to_string(&path).expect("read fixed file");
    (dir, got, out)
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
    let input = read_fixture("dupstring_in.sal");
    let (dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let want = read_fixture("dupstring_out.sal");
    assert_eq!(got, want);
    let path = dir.path().join("input.sal");
    let path_str = path.to_string_lossy().into_owned();
    let out2 = run_standard(&["fix", &path_str]);
    assert_eq!(out2.status.code(), Some(0));
    let got2 = fs::read_to_string(&path).expect("read again");
    assert_eq!(got2, want);
}

#[test]
fn standard_fix_elsif() {
    let input = read_fixture("elsif_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("elsif_out.sal"));
}

#[test]
fn standard_fix_elsif_two_passes() {
    let input = read_fixture("elsif_two_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("elsif_two_out.sal"));
}

#[test]
fn standard_fix_preserves_comment() {
    let input = read_fixture("comment_survives_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("comment_survives_out.sal"));
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
