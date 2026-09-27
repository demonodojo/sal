use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use sal_compiler::compile::compile_file;
use sal_compiler::compile::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

static COMPILE_LOCK: Mutex<()> = Mutex::new(());

fn isolated_project_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        p
    })
    .clone()
}

fn standard_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let _guard = COMPILE_LOCK.lock().expect("standard compile lock");
        let opts = CompileOptions {
            release: false,
            instrument: false,
            device: "cpu".into(),
            project_root: isolated_project_root(),
            skip_link: false,
        };
        let art =
            compile_file(&root().join("standard/main.sal"), &opts).expect("compile standard");
        let bin = art.binary.expect("standard binary");
        let copy = PathBuf::from(format!(
            "/tmp/sal-standard-bin-{}",
            std::process::id()
        ));
        fs::copy(&bin, &copy).expect("copy standard bin");
        copy
    })
    .clone()
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
fn standard_check_json_includes_fix() {
    let path = root()
        .join("standard/fixtures/dupstring_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path, "--error-format", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err.lines().next().expect("json line");
    let value: serde_json::Value = serde_json::from_str(line).expect("json");
    assert_eq!(value["code"], "Style/DupString");
    assert_eq!(value["fix"], "d\"hi\"");
    assert!(value["span"]["start"].is_number());
    assert!(value["span"]["end"].as_u64().unwrap() > value["span"]["start"].as_u64().unwrap());
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
fn standard_fix_elsif_keeps_comparison() {
    let input = read_fixture("elsif_cmp_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("elsif_cmp_out.sal"));
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
fn standard_fix_stringplus_call() {
    let input = read_fixture("stringplus_call_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("stringplus_call_out.sal"));
}

#[test]
fn standard_fix_stringplus_chain() {
    let input = read_fixture("stringplus_chain_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("stringplus_chain_out.sal"));
}

#[test]
fn standard_check_stringplus_reports() {
    let path = root()
        .join("standard/fixtures/stringplus_call_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Style/StringPlus"), "stderr: {err}");
}

#[test]
fn standard_fix_plusliteral() {
    let input = read_fixture("plusliteral_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("plusliteral_out.sal"));
}

#[test]
fn standard_check_plusliteral_reports() {
    let path = root()
        .join("standard/fixtures/plusliteral_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Style/PlusLiteral"), "stderr: {err}");
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
