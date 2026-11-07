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

fn standard_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        // The default test thread (2 MiB) is too small to compile standard/.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(compile_standard_bin)
            .expect("spawn standard compile")
            .join()
            .expect("standard compile thread")
    })
    .clone()
}

fn compile_standard_bin() -> PathBuf {
    let _guard = COMPILE_LOCK.lock().expect("standard compile lock");
    let _ = fs::remove_dir_all(root().join("target/sal-out"));
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&root().join("standard/main.sal"), &opts).expect("compile standard");
    let bin = art.binary.expect("standard binary");
    let copy = PathBuf::from(format!("/tmp/sal-standard-bin-{}", std::process::id()));
    fs::copy(&bin, &copy).expect("copy standard bin");
    copy
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
    fix_with(input, &[])
}

fn fix_with(input: &str, extra: &[&str]) -> (tempfile::TempDir, String, std::process::Output) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("input.sal");
    fs::write(&path, input).expect("write temp input");
    let path_str = path.to_string_lossy().into_owned();
    let mut args = vec!["fix", path_str.as_str()];
    args.extend(extra);
    let out = run_standard(&args);
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

#[test]
fn standard_check_clean_on_method_chain() {
    let path = root()
        .join("standard/fixtures/method_chain_skip_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let fixed = run_standard(&["fix", &path]);
    assert_eq!(fixed.status.code(), Some(0));
    assert_eq!(fs::read_to_string(&path).unwrap(), read_fixture("method_chain_skip_in.sal"));
}

#[test]
fn standard_fix_unusedbinding() {
    let input = read_fixture("unusedbinding_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("unusedbinding_out.sal"));
}

#[test]
fn standard_check_unusedbinding_reports() {
    let path = root()
        .join("standard/fixtures/unusedbinding_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Style/UnusedBinding"), "stderr: {err}");
}

#[test]
fn standard_fix_method() {
    let input = read_fixture("method_in.sal");
    let (dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let want = read_fixture("method_out.sal");
    assert_eq!(got, want);
    let path = dir.path().join("input.sal");
    let path_str = path.to_string_lossy().into_owned();
    let out2 = run_standard(&["fix", &path_str]);
    assert_eq!(out2.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out2.stderr));
    let got2 = fs::read_to_string(&path).expect("read again");
    assert_eq!(got2, want);
}

#[test]
fn standard_check_method_reports() {
    let path = root()
        .join("standard/fixtures/method_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Style/Method"), "stderr: {err}");
    assert!(err.contains("use .len() instead of str_len"), "stderr: {err}");
    assert!(err.contains("use List.new instead of list_new"), "stderr: {err}");
}

#[test]
fn standard_method_skip_is_clean() {
    let input = read_fixture("method_skip_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, input);
    let path = root()
        .join("standard/fixtures/method_skip_in.sal")
        .to_string_lossy()
        .into_owned();
    let check = run_standard(&["check", &path]);
    assert_eq!(check.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&check.stderr));
}

#[test]
fn standard_fix_and() {
    let input = read_fixture("and_in.sal");
    let (dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let want = read_fixture("and_out.sal");
    assert_eq!(got, want);
    let path = dir.path().join("input.sal");
    let path_str = path.to_string_lossy().into_owned();
    let out2 = run_standard(&["fix", &path_str]);
    assert_eq!(out2.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out2.stderr));
    let got2 = fs::read_to_string(&path).expect("read again");
    assert_eq!(got2, want);
}

#[test]
fn standard_check_and_reports() {
    let path = root()
        .join("standard/fixtures/and_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Style/And"), "stderr: {err}");
    assert!(err.contains("use && instead of nested if"), "stderr: {err}");
}

#[test]
fn standard_fix_and_or() {
    let input = read_fixture("and_or_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("and_or_out.sal"));
}

#[test]
fn standard_fix_and_two_passes() {
    let input = read_fixture("and_two_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("and_two_out.sal"));
}

#[test]
fn standard_and_skip_is_clean() {
    let input = read_fixture("and_skip_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, input);
    let path = root()
        .join("standard/fixtures/and_skip_in.sal")
        .to_string_lossy()
        .into_owned();
    let check = run_standard(&["check", &path]);
    assert_eq!(check.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&check.stderr));
}

#[test]
fn standard_and_comment_blocks_fix() {
    let input = read_fixture("and_comment_in.sal");
    let path = root()
        .join("standard/fixtures/and_comment_in.sal")
        .to_string_lossy()
        .into_owned();
    let check = run_standard(&["check", &path]);
    assert_eq!(check.status.code(), Some(1));
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(err.contains("Style/And"), "stderr: {err}");
    assert!(err.contains("comment blocks auto-fix"), "stderr: {err}");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, input);
}

/// A binding that is read (also inside `"{n}"`), or the last statement of its block (the value
/// of the block), is not an offense.
#[test]
fn standard_unusedbinding_skip_is_clean() {
    let path = root()
        .join("standard/fixtures/unusedbinding_skip_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn standard_fix_matchif_int() {
    let input = read_fixture("matchif_int_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("matchif_int_out.sal"));
}

#[test]
fn standard_fix_matchif_str() {
    let input = read_fixture("matchif_str_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("matchif_str_out.sal"));
}

#[test]
fn standard_fix_matchif_block() {
    let input = read_fixture("matchif_block_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("matchif_block_out.sal"));
}

#[test]
fn standard_matchif_skip_unchanged() {
    let input = read_fixture("matchif_skip_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("matchif_skip_out.sal"));
}

#[test]
fn standard_fix_matchif_inner() {
    let input = read_fixture("matchif_inner_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("matchif_inner_out.sal"));
}

#[test]
fn standard_fix_stdapi() {
    let input = read_fixture("stdapi_min_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, read_fixture("stdapi_min_out.sal"));
}

#[test]
fn standard_fix_range_only_middle() {
    let input = "fn main() -> String ! alloc\n    strdup(\"a\")\n    strdup(\"b\")\n    strdup(\"c\")\n";
    let start = input.find("strdup(\"b\")").expect("middle call");
    let end = start + "strdup(\"b\")".len();
    let start_s = start.to_string();
    let end_s = end.to_string();
    let (_dir, got, out) = fix_with(input, &["--start", &start_s, "--end", &end_s]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        got,
        "fn main() -> String ! alloc\n    strdup(\"a\")\n    d\"b\"\n    strdup(\"c\")\n"
    );
}

#[test]
fn standard_fix_range_second_pass_stays_inside() {
    let input = "\
fn keep() -> String ! alloc
    strdup(\"z\")

fn touch() -> Int
    if a
        1
    else
        if b
            2
        else
            3
";
    let start = input.find("fn touch").expect("second fn");
    let start_s = start.to_string();
    let end_s = input.len().to_string();
    let (_dir, got, out) = fix_with(input, &["--start", &start_s, "--end", &end_s]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        got,
        "\
fn keep() -> String ! alloc
    strdup(\"z\")

fn touch() -> Int
    if a
        1
    elsif b
        2
    else
        3
"
    );
}

#[test]
fn standard_fix_constfn() {
    let input = read_fixture("constfn_in.sal");
    let (dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let want = read_fixture("constfn_out.sal");
    assert_eq!(got, want);
    let path = dir.path().join("input.sal");
    let path_str = path.to_string_lossy().into_owned();
    let out2 = run_standard(&["fix", &path_str]);
    assert_eq!(out2.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out2.stderr));
    let got2 = fs::read_to_string(&path).expect("read again");
    assert_eq!(got2, want);
}

#[test]
fn standard_check_constfn_reports() {
    let path = root()
        .join("standard/fixtures/constfn_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Style/ConstFn"), "stderr: {err}");
    assert!(err.contains("use a module constant instead of TK_EOF"), "stderr: {err}");
    assert!(err.contains("call the module constant TK_EOF"), "stderr: {err}");
}

#[test]
fn standard_constfn_skip_is_clean() {
    let input = read_fixture("constfn_skip_in.sal");
    let (_dir, got, out) = fix_in_temp(&input);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(got, input);
    let path = root()
        .join("standard/fixtures/constfn_skip_in.sal")
        .to_string_lossy()
        .into_owned();
    let check = run_standard(&["check", &path]);
    assert_eq!(check.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&check.stderr));
}

#[test]
fn standard_fix_range_rejects_bad_args() {
    let input = read_fixture("dupstring_in.sal");
    let (_dir, got, out) = fix_with(&input, &["--start"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(got, input);
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}

#[test]
fn standard_lexer_rstring_ok() {
    let path = root()
        .join("standard/fixtures/rstring_lex_in.sal")
        .to_string_lossy()
        .into_owned();
    let out = run_standard(&["check", &path]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}
