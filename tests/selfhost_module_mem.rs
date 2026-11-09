//! Memory of each selfhost module, compiled and run on its own.
//!
//! `selfhost/main.sal` and `selfhost/emit/emit.sal` import the whole compiler
//! (every module). They are not built here: that is the compile that exhausts
//! memory. Every other module is typechecked by itself, and the leaf modules
//! under `tests/selfhost_mem/` are executed with instrumentation.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn selfhost_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read selfhost").flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "sal") {
                out.push(path);
            }
        }
    }
    walk(&root().join("selfhost"), &mut out);
    out.sort();
    out
}

/// Roots that pull every selfhost module into one compile.
fn pulls_whole_compiler(path: &Path) -> bool {
    let rel = path.strip_prefix(root()).unwrap_or(path);
    rel == Path::new("selfhost/main.sal") || rel == Path::new("selfhost/emit/emit.sal")
}

fn check_rss(sal: &str, path: &Path) -> (u64, Option<i32>, String) {
    let log = std::env::temp_dir().join(format!(
        "sal-modrss-{}-{}",
        std::process::id(),
        path.file_name().unwrap().to_string_lossy()
    ));
    let out = Command::new("/usr/bin/time")
        .args(["-f", "%M", "-o"])
        .arg(&log)
        .arg(sal)
        .arg("check")
        .arg(path)
        .current_dir(root())
        .output()
        .expect("time sal check");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    let rss = text
        .lines()
        .rev()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    (rss, out.status.code(), err)
}

/// Typechecking one module stays well under the cost of compiling the whole compiler.
#[test]
fn each_selfhost_module_checks_under_memory_cap() {
    // `qualify.sal` was the heaviest measured root, at about 85 MB.
    const CAP_KB: u64 = 160_000;
    let sal = env!("CARGO_BIN_EXE_sal");
    let mut bad = Vec::new();
    let mut checked = 0;
    for path in selfhost_sources() {
        if pulls_whole_compiler(&path) {
            continue;
        }
        checked += 1;
        let rel = path.strip_prefix(root()).unwrap_or(&path).display().to_string();
        let (rss, code, err) = check_rss(sal, &path);
        if code != Some(0) {
            bad.push(format!("{rel}: check exit {code:?}\n{err}"));
        } else if rss > CAP_KB {
            bad.push(format!("{rel}: {rss} KB RSS, cap is {CAP_KB} KB"));
        }
    }
    assert!(checked >= 30, "only {checked} modules checked");
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

fn leak_sites(stderr: &str) -> Vec<String> {
    let mut sites = Vec::new();
    for line in stderr.lines() {
        let Some(rest) = line.split("\"site\":\"").nth(1) else {
            continue;
        };
        let Some(site) = rest.split('"').next() else {
            continue;
        };
        if line.contains("\"code\":\"LEAK\"") {
            sites.push(site.to_string());
        }
    }
    sites
}

/// One run of each leaf module. The sites are the leaks that run still reports.
#[test]
fn leaf_selfhost_modules_report_known_leaks() {
    let sal = env!("CARGO_BIN_EXE_sal");
    let known: &[(&str, &[&str])] = &[
        ("cache_alloc.sal", &["strdup"]),
        ("lexer_alloc.sal", &["strdup"]),
        ("result_alloc.sal", &["vec_new"]),
        ("thash_alloc.sal", &["strdup"]),
        ("tshape_alloc.sal", &["strdup"]),
        ("tytext_alloc.sal", &["strdup"]),
    ];
    let mut bad = Vec::new();
    for (name, want) in known {
        let src = root().join("tests/selfhost_mem").join(name);
        let bin = std::env::temp_dir().join(format!("sal-modbin-{}-{}", std::process::id(), name));
        let built = Command::new(sal)
            .arg("-o")
            .arg(&bin)
            .args(["build", "--instrument"])
            .arg(&src)
            .current_dir(root())
            .output()
            .expect("sal build");
        if !built.status.success() {
            bad.push(format!(
                "{name}: build failed\n{}",
                String::from_utf8_lossy(&built.stderr)
            ));
            continue;
        }
        let run = Command::new(&bin)
            .env("SAL_INSTRUMENT_STRICT", "1")
            .current_dir(root())
            .output()
            .expect("run module");
        let _ = std::fs::remove_file(&bin);
        let err = String::from_utf8_lossy(&run.stderr);
        if err.contains("DOUBLE_FREE") || err.contains("USE_AFTER_FREE") {
            bad.push(format!("{name}: unsafe free\n{err}"));
            continue;
        }
        let got = leak_sites(&err);
        let want_v: Vec<String> = want.iter().map(|s| (*s).to_string()).collect();
        if got != want_v {
            bad.push(format!("{name}: leaks {got:?}, expected {want_v:?}\n{err}"));
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}
