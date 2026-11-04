//! Language conformance: every `tests/lang/<area>/*.sal` states its expected result in
//! leading `#` comments, written by hand (not taken from a compiler):
//!
//! - `# exit: N`      the program builds and exits with N (low byte)
//! - `# stdout: TEXT` its stdout is exactly TEXT (optional)
//! - `# error: CODE`  the compiler rejects it with diagnostic CODE
//! - `# xfail: WHY`   known gap: the file must still fail to meet its
//!                    `exit`/`error` expectation; fixing it makes the test
//!                    fail until the marker is removed.
use std::path::{Path, PathBuf};
use std::process::Command;

struct Spec {
    exit: Option<i32>,
    stdout: Option<String>,
    error: Option<String>,
    xfail: Option<String>,
}

fn parse_spec(src: &str) -> Spec {
    let mut s = Spec { exit: None, stdout: None, error: None, xfail: None };
    for line in src.lines().take_while(|l| l.starts_with('#')) {
        let body = line.trim_start_matches('#').trim();
        if let Some(v) = body.strip_prefix("exit:") {
            s.exit = Some(v.trim().parse().expect("exit code"));
        } else if let Some(v) = body.strip_prefix("stdout:") {
            s.stdout = Some(v.trim().to_string());
        } else if let Some(v) = body.strip_prefix("error:") {
            s.error = Some(v.trim().to_string());
        } else if let Some(v) = body.strip_prefix("xfail:") {
            s.xfail = Some(v.trim().to_string());
        }
    }
    s
}

/// `Ok(())` when the program meets its spec, `Err(why)` otherwise.
fn check(path: &Path, spec: &Spec, out_dir: &Path) -> Result<(), String> {
    let name = path.file_stem().unwrap().to_string_lossy().to_string();
    let bin = out_dir.join(&name);
    let _ = std::fs::remove_file(&bin);
    let built = Command::new(env!("CARGO_BIN_EXE_sal"))
        .arg("build")
        .arg(path)
        .arg("-o")
        .arg(&bin)
        .current_dir(out_dir)
        .output()
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&built.stderr).to_string();
    if let Some(code) = &spec.error {
        if built.status.success() {
            return Err(format!("expected {code}, but it compiled"));
        }
        if !stderr.contains(code.as_str()) {
            return Err(format!("expected {code}, got:\n{stderr}"));
        }
        return Ok(());
    }
    if !built.status.success() {
        return Err(format!("did not compile:\n{stderr}"));
    }
    let run = Command::new(&bin).output().map_err(|e| e.to_string())?;
    if let Some(want) = spec.exit {
        let got = run.status.code().unwrap_or(-1);
        if got != want & 0xff {
            return Err(format!("exit {got}, expected {}", want & 0xff));
        }
    }
    if let Some(want) = &spec.stdout {
        let got = String::from_utf8_lossy(&run.stdout);
        if got.trim_end() != want {
            return Err(format!("stdout {got:?}, expected {want:?}"));
        }
    }
    Ok(())
}

/// Every test program under `dir`, one subdirectory per area. `mods/` holds
/// helper modules imported by tests (no header of their own) and is skipped.
fn collect_programs(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let path = e.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "mods" && n != "target") {
                collect_programs(&path, out);
            }
        } else if path.extension().is_some_and(|x| x == "sal") {
            out.push(path);
        }
    }
}

#[test]
fn language_conformance() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/lang");
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-lang");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut files = Vec::new();
    collect_programs(&dir, &mut files);
    files.sort();
    assert!(files.len() >= 30, "conformance corpus shrank");
    let mut failures = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let spec = parse_spec(&src);
        assert!(
            spec.exit.is_some() || spec.error.is_some(),
            "{}: no `# exit:` or `# error:` header",
            path.display()
        );
        let name = path.strip_prefix(&dir).unwrap().display().to_string();
        match (check(path, &spec, &out_dir), &spec.xfail) {
            (Ok(()), None) => {}
            (Err(why), None) => failures.push(format!("{name}: {why}")),
            (Err(_), Some(_)) => {}
            (Ok(()), Some(why)) => failures.push(format!(
                "{name}: marked xfail ({why}) but now passes; remove the marker"
            )),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
