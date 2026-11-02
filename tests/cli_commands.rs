//! Black-box tests of the `sal` command line: exit codes, streams and the
//! output of each subcommand.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn work(tag: &str) -> PathBuf {
    let d = root().join("target").join("test-cli").join(tag);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sal(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run sal")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

fn write(dir: &Path, name: &str, src: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, src).unwrap();
    p
}

const GOOD: &str = "fn main() -> Int\n    7\n";
const BAD_TYPE: &str = "fn main() -> Int\n    true\n";
const BAD_PARSE: &str = "fn main( -> Int\n    0\n";

#[test]
fn check_accepts_and_rejects() {
    let d = work("check");
    let ok = write(&d, "ok.sal", GOOD);
    assert!(sal(&d, &["check", ok.to_str().unwrap()]).status.success());

    let bad = write(&d, "bad.sal", BAD_TYPE);
    let o = sal(&d, &["check", bad.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("E_TYPE"), "{}", text(&o.stderr));

    let bad = write(&d, "badp.sal", BAD_PARSE);
    let o = sal(&d, &["check", bad.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("E_PARSE"));
}

#[test]
fn check_error_format_json_is_machine_readable() {
    let d = work("check-json");
    let bad = write(&d, "bad.sal", BAD_TYPE);
    let o = sal(&d, &["check", "--error-format", "json", bad.to_str().unwrap()]);
    assert!(!o.status.success());
    let line = text(&o.stderr).lines().find(|l| l.starts_with('{')).expect("json line").to_string();
    let v: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(v["code"], "E_TYPE");
    assert!(v["message"].is_string());
}

#[test]
fn check_missing_file_fails_cleanly() {
    let d = work("check-missing");
    let o = sal(&d, &["check", "nope.sal"]);
    assert!(!o.status.success());
}

#[test]
fn run_propagates_exit_code_and_stdout() {
    let d = work("run");
    let p = write(
        &d,
        "p.sal",
        "fn main() -> Int ! io, alloc\n    print(d\"out\")\n    9\n",
    );
    let o = sal(&d, &["run", p.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(9));
    assert_eq!(text(&o.stdout).trim_end(), "out");
}

#[test]
fn run_passes_arguments_after_double_dash() {
    let d = work("run-args");
    let p = write(&d, "p.sal", "fn main() -> Int ! alloc\n    str_len(argv(1))\n");
    let o = sal(&d, &["run", p.to_str().unwrap(), "--", "abcd"]);
    assert_eq!(o.status.code(), Some(4), "stderr={}", text(&o.stderr));
}

#[test]
fn run_reports_compile_errors_without_running() {
    let d = work("run-bad");
    let p = write(&d, "p.sal", BAD_TYPE);
    let o = sal(&d, &["run", p.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("E_TYPE"));
}

#[test]
fn build_writes_the_requested_output() {
    let d = work("build-o");
    let p = write(&d, "p.sal", GOOD);
    let out = d.join("custom_bin");
    let o = sal(&d, &["build", p.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(out.is_file());
    assert_eq!(Command::new(&out).status().unwrap().code(), Some(7));
}

#[test]
fn build_release_and_debug_behave_the_same() {
    let d = work("build-rel");
    let p = write(&d, "p.sal", "fn main() -> Int\n    (6 * 7) % 100\n");
    for (tag, extra) in [("dbg", None), ("rel", Some("--release"))] {
        let out = d.join(tag);
        let mut args = vec!["build", p.to_str().unwrap(), "-o", out.to_str().unwrap()];
        if let Some(e) = extra {
            args.push(e);
        }
        assert!(sal(&d, &args).status.success());
        assert_eq!(Command::new(&out).status().unwrap().code(), Some(42), "{tag}");
    }
}

#[test]
fn fmt_prints_canonical_source_to_stdout() {
    let d = work("fmt");
    let p = write(&d, "p.sal", "fn   main( )   ->   Int\n    x   =   1+2\n    x\n");
    let o = sal(&d, &["fmt", p.to_str().unwrap()]);
    assert!(o.status.success());
    let out = text(&o.stdout);
    assert!(out.contains("fn main() -> Int"), "{out}");
    assert!(out.contains("x = 1 + 2"), "{out}");
}

#[test]
fn emit_ast_is_json_that_compile_accepts() {
    let d = work("emit-ast");
    let p = write(&d, "p.sal", GOOD);
    let o = sal(&d, &["emit", "ast", p.to_str().unwrap()]);
    assert!(o.status.success());
    let ast = write(&d, "p.json", &text(&o.stdout));
    let _: serde_json::Value = serde_json::from_str(&text(&o.stdout)).expect("json");
    let out = d.join("from_ast");
    let c = sal(&d, &["compile", "--ast", ast.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(c.status.success(), "{}", text(&c.stderr));
    assert_eq!(Command::new(&out).status().unwrap().code(), Some(7));
}

#[test]
fn emit_typed_ir_and_llvm_have_their_shape() {
    let d = work("emit-kinds");
    let p = write(&d, "p.sal", GOOD);
    let typed = sal(&d, &["emit", "typed", p.to_str().unwrap()]);
    assert!(typed.status.success());
    let _: serde_json::Value = serde_json::from_str(&text(&typed.stdout)).expect("typed json");

    let ir = sal(&d, &["emit", "ir", p.to_str().unwrap()]);
    assert!(ir.status.success());
    assert!(text(&ir.stdout).contains("main"), "{}", text(&ir.stdout));

    let ll = sal(&d, &["emit", "llvm", p.to_str().unwrap()]);
    assert!(ll.status.success());
    let ll = text(&ll.stdout);
    assert!(ll.contains("define"), "{ll}");
    assert!(ll.contains("main"), "{ll}");
}

#[test]
fn emit_unknown_kind_fails() {
    let d = work("emit-unknown");
    let p = write(&d, "p.sal", GOOD);
    let o = sal(&d, &["emit", "bogus", p.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("unknown emit kind"));
}

#[test]
fn emit_ir_reports_errors_in_the_module_graph() {
    let d = work("emit-ir-bad");
    let p = write(&d, "p.sal", "import \"missing.sal\"\n\nfn main() -> Int\n    0\n");
    let o = sal(&d, &["emit", "ir", p.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("E_TYPE"), "{}", text(&o.stderr));
}

#[test]
fn test_command_runs_test_files_and_reports_failures() {
    let d = work("test-cmd");
    write(&d, "test_pass.sal", "fn main() -> Int\n    0\n");
    write(&d, "helper.sal", "fn main() -> Int\n    1\n"); // not a test_*.sal file
    let o = sal(&d, &["test", d.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("ok") && out.contains("test_pass.sal"), "{out}");
    assert!(!out.contains("helper.sal"));

    write(&d, "test_fail.sal", "fn main() -> Int\n    3\n");
    let o = sal(&d, &["test", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "one failing test -> exit 1");
    assert!(text(&o.stderr).contains("FAIL"));
    assert!(text(&o.stderr).contains("test_fail.sal"));
}

#[test]
fn test_command_counts_compile_failures() {
    let d = work("test-compile-fail");
    write(&d, "test_a.sal", BAD_TYPE);
    write(&d, "test_b.sal", BAD_PARSE);
    let o = sal(&d, &["test", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).contains("compile FAIL"));
}

#[test]
fn test_command_rejects_missing_dir_and_empty_dir() {
    let d = work("test-empty");
    let o = sal(&d, &["test", d.join("nope").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("not a directory"));

    let o = sal(&d, &["test", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("no test_*.sal"));
}

#[test]
fn unknown_subcommand_and_missing_args_are_usage_errors() {
    let d = work("usage");
    assert!(!sal(&d, &["frobnicate"]).status.success());
    assert!(!sal(&d, &["check"]).status.success());
    let h = sal(&d, &["--help"]);
    assert!(h.status.success());
    assert!(text(&h.stdout).contains("Commands:"));
}
