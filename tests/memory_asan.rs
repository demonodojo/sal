//! Memory-management conformance under clang's sanitizers (ASan + LSan + UBSan).
//!
//! `sal build --sanitize` links the program and the C runtime with
//! `-fsanitize=address,undefined`, so leaks, double frees, use-after-free and overflows are
//! reported by clang's runtime, independently of our own `--instrument` bookkeeping.
//!
//! - Sweep: every `tests/lang/<area>` and `tests/memory` program with `# exit:` must build,
//!   run with the expected exit code/stdout and print no sanitizer report.
//! - `# asan-xfail: WHY` marks a known memory gap: the program must still fail under the
//!   sanitizer; fixing it makes the test fail until the marker is removed.
//! - Negative controls (`tests/support/asan_neg_*.c`) must be caught, so the harness cannot pass
//!   by accident (e.g. leak detection silently off).
//!
//! `SAL_REQUIRE_ASAN=1` turns "sanitizer unavailable" from a skip into a failure (CI).
use std::path::{Path, PathBuf};
use std::process::Command;

const AREAS: &[&str] = &[
    "drops", "ownership", "collections", "structs", "enums", "strings", "methods", "generics",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn header(src: &str, key: &str) -> Option<String> {
    src.lines()
        .take_while(|l| l.starts_with('#'))
        .find_map(|l| l.trim_start_matches('#').trim().strip_prefix(key).map(|v| v.trim().to_string()))
}

fn report(stderr: &str) -> bool {
    stderr.contains("Sanitizer") || stderr.contains("runtime error:")
}

/// `false` (after saying why) when the host cannot run an ASan binary with leak detection.
fn sanitizer_available(out: &Path) -> bool {
    let src = out.join("probe.c");
    let bin = out.join("probe");
    std::fs::write(&src, "int main(void){return 0;}\n").unwrap();
    let built = Command::new("clang")
        .args(["-fsanitize=address", "-o"])
        .arg(&bin)
        .arg(&src)
        .output();
    let ok = built.as_ref().is_ok_and(|o| o.status.success())
        && Command::new(&bin)
            .env("ASAN_OPTIONS", "detect_leaks=1")
            .output()
            .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var_os("SAL_REQUIRE_ASAN").is_none(),
            "SAL_REQUIRE_ASAN set but clang -fsanitize=address does not work here"
        );
        eprintln!("skipping: clang AddressSanitizer/LeakSanitizer unavailable");
    }
    ok
}

fn asan_env(cmd: &mut Command) -> &mut Command {
    cmd.env("ASAN_OPTIONS", "detect_leaks=1:halt_on_error=1:abort_on_error=0:exitcode=99")
        .env("UBSAN_OPTIONS", "print_stacktrace=1:halt_on_error=1")
}

fn programs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut dirs: Vec<PathBuf> = AREAS.iter().map(|a| root().join("tests/lang").join(a)).collect();
    dirs.push(root().join("tests/memory"));
    for d in dirs {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "sal") {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}

/// `Ok` when the program builds and runs clean under the sanitizer and meets its header.
fn run_sanitized(path: &Path, work: &Path) -> Result<(), String> {
    let src = std::fs::read_to_string(path).unwrap();
    let bin = work.join(path.file_stem().unwrap());
    let _ = std::fs::remove_file(&bin);
    let built = Command::new(env!("CARGO_BIN_EXE_sal"))
        .arg("--sanitize")
        .arg("build")
        .arg(path)
        .arg("-o")
        .arg(&bin)
        .current_dir(work)
        .output()
        .map_err(|e| e.to_string())?;
    if !built.status.success() {
        return Err(format!("did not build:\n{}", String::from_utf8_lossy(&built.stderr)));
    }
    let mut cmd = Command::new(&bin);
    let run = asan_env(&mut cmd).current_dir(work).output().map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&bin);
    let run = run?;
    let stderr = String::from_utf8_lossy(&run.stderr);
    if report(&stderr) {
        let head: String = stderr.lines().take(14).collect::<Vec<_>>().join("\n");
        return Err(format!("sanitizer report:\n{head}"));
    }
    if let Some(want) = header(&src, "exit:") {
        let want: i32 = want.parse().expect("exit code");
        let got = run.status.code().unwrap_or(-1);
        if got != want & 0xff {
            return Err(format!("exit {got}, expected {}", want & 0xff));
        }
    }
    if let Some(want) = header(&src, "stdout:") {
        let got = String::from_utf8_lossy(&run.stdout);
        if got.trim_end() != want {
            return Err(format!("stdout {got:?}, expected {want:?}"));
        }
    }
    Ok(())
}

#[test]
fn sanitized_programs_manage_memory() {
    let work = root().join("target/test-asan");
    std::fs::create_dir_all(&work).unwrap();
    if !sanitizer_available(&work) {
        return;
    }
    let mut failures = Vec::new();
    let mut checked = 0;
    for path in programs() {
        let src = std::fs::read_to_string(&path).unwrap();
        if header(&src, "exit:").is_none() || header(&src, "xfail:").is_some() {
            continue; // diagnostics tests and known frontend gaps do not run
        }
        checked += 1;
        let name = path.strip_prefix(root()).unwrap().display().to_string();
        match (run_sanitized(&path, &work), header(&src, "asan-xfail:")) {
            (Ok(()), None) => {}
            (Ok(()), Some(why)) => failures.push(format!("{name}: asan-xfail ({why}) but it is clean now; remove the marker")),
            (Err(e), None) => failures.push(format!("{name}: {e}")),
            (Err(_), Some(_)) => {}
        }
    }
    assert!(checked >= 100, "memory corpus shrank ({checked} programs)");
    assert!(failures.is_empty(), "{} memory failure(s):\n{}", failures.len(), failures.join("\n\n"));
}

/// Build `tests/support/<name>.c` with the runtime under ASan and run it.
fn run_negative(name: &str, work: &Path) -> (Option<i32>, String) {
    let bin = work.join(name);
    let built = Command::new("clang")
        .args(["-fsanitize=address", "-fno-omit-frame-pointer", "-g", "-O0", "-w", "-I"])
        .arg(root().join("runtime"))
        .arg("-o")
        .arg(&bin)
        .arg(root().join(format!("tests/support/{name}.c")))
        .arg(root().join("runtime/sal_runtime.c"))
        .arg(root().join("runtime/soso/linux_host_syscall.c"))
        .args(["-lm", "-lpthread"])
        .output()
        .expect("clang");
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut cmd = Command::new(&bin);
    let run = asan_env(&mut cmd).output().expect("run");
    let _ = std::fs::remove_file(&bin);
    (run.status.code(), String::from_utf8_lossy(&run.stderr).into_owned())
}

#[test]
fn sanitizer_catches_deliberate_memory_bugs() {
    let work = root().join("target/test-asan");
    std::fs::create_dir_all(&work).unwrap();
    if !sanitizer_available(&work) {
        return;
    }
    for (name, needle) in [
        ("asan_neg_leak", "LeakSanitizer: detected memory leaks"),
        ("asan_neg_double_free", "double-free|heap-use-after-free"),
        ("asan_neg_use_after_free", "heap-use-after-free"),
    ] {
        let (code, stderr) = run_negative(name, &work);
        assert_eq!(code, Some(99), "{name} was not caught:\n{stderr}");
        assert!(
            needle.split('|').any(|n| stderr.contains(n)),
            "{name}: expected {needle:?} in:\n{stderr}"
        );
    }
}
