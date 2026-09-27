use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use sal_compiler::fuse::fuse_module;
use sal_compiler::ir::{ir_to_text, lower_program};
use sal_compiler::parser::parse;
use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `compile_file` links to a shared `target/sal-out/a.out`.
static LINK_LOCK: Mutex<()> = Mutex::new(());

fn corpus_files() -> Vec<PathBuf> {
    let root = root();
    [
        "corpus/moves.sal",
        "corpus/effects.sal",
        "corpus/places.sal",
        "corpus/forward_fused.sal",
        "corpus/load_salt.sal",
        "corpus/instrument_oob.sal",
        "corpus/instrument_leak.sal",
        "examples/hello.sal",
        "examples/forward.sal",
    ]
    .into_iter()
    .map(|p| root.join(p))
    .collect()
}

fn bootstrap_ir(path: &Path) -> String {
    let src = std::fs::read_to_string(path).unwrap();
    let p = parse(&src).unwrap();
    let mut ir = lower_program(&p);
    fuse_module(&mut ir);
    ir_to_text(&ir)
}

fn compile_selfhost(instrument: bool) -> PathBuf {
    let _guard = LINK_LOCK.lock().expect("link lock");
    let opts = CompileOptions {
        release: false,
        instrument,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&root().join("selfhost/main.sal"), &opts).expect("compile selfhost");
    let bin = art.binary.expect("binary");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let copy = PathBuf::from(format!(
        "/tmp/sal-selfhost-bin-{}-{}-{}",
        std::process::id(),
        stamp,
        if instrument { "inst" } else { "plain" }
    ));
    std::fs::copy(&bin, &copy).expect("copy binary");
    copy
}

fn run_emit_ir(bin: &Path, input: &Path) -> String {
    run_emit_ir_in(bin, input, &root(), "emit")
}

fn run_emit_ir_in(bin: &Path, input: &Path, project: &Path, cache_tag: &str) -> String {
    let cache = unique_cache_dir(cache_tag);
    let out = Command::new(bin)
        .arg(input)
        .env("SAL_SELFHOST_CACHE", &cache)
        .env("SAL_PROJECT_ROOT", project)
        .current_dir(project)
        .output()
        .expect("run selfhost");
    assert_eq!(
        out.status.code(),
        Some(0),
        "selfhost failed on {}: stderr={}",
        input.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 ir")
}

fn compile_stage2(stage1: &Path) -> PathBuf {
    let out = PathBuf::from(format!(
        "/tmp/sal-stage2-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = Command::new(stage1)
        .arg(root().join("selfhost/main.sal"))
        .arg("-o")
        .arg(&out)
        .current_dir(root())
        .output()
        .expect("stage1 -o");
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 failed to produce stage2: stderr={}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(out.is_file(), "stage2 binary missing");
    out
}

fn write_temp_sal(body: &str) -> PathBuf {
    let path = PathBuf::from(format!(
        "/tmp/sal-prog-{}-{}.sal",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, body).expect("write temp sal");
    path
}

fn unique_cache_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from(format!(
        "/tmp/sal-selfhost-cache-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("cache dir");
    dir
}

#[test]
fn corpus_ir_matches_bootstrap_stage1_stage2() {
    assert!(
        !root().join("runtime/sal_selfhost.c").exists(),
        "runtime/sal_selfhost.c must not exist"
    );
    let main_sal = std::fs::read_to_string(root().join("selfhost/main.sal")).unwrap();
    assert!(
        !main_sal.contains("typedef struct"),
        "selfhost/main.sal must not embed C (typedef struct)"
    );
    assert!(
        !main_sal.contains("sal_emit_ir(const char"),
        "selfhost/main.sal must not embed C sal_emit_ir"
    );

    let stage1 = compile_selfhost(false);
    let stage2 = compile_stage2(&stage1);

    // stage1 must not depend on sal_emit_ir (C selfhost removed from link)
    let nm = Command::new("nm")
        .arg(&stage1)
        .output()
        .expect("nm stage1");
    let nm_txt = String::from_utf8_lossy(&nm.stdout);
    assert!(
        !nm_txt.contains("sal_emit_ir"),
        "stage1 must not reference sal_emit_ir (got nm hit)"
    );

    // stage2 must be a real recompile, not a byte copy of stage1
    let b1 = std::fs::read(&stage1).expect("read stage1");
    let b2 = std::fs::read(&stage2).expect("read stage2");
    assert_ne!(
        b1, b2,
        "stage2 must not be a byte-for-byte copy of stage1 (sal_copy_self shortcut)"
    );

    for file in corpus_files() {
        let boot = bootstrap_ir(&file);
        let ir1 = run_emit_ir(&stage1, &file);
        let ir2 = run_emit_ir(&stage2, &file);
        assert_eq!(
            boot, ir1,
            "bootstrap vs stage1 IR mismatch for {}",
            file.display()
        );
        assert_eq!(
            boot, ir2,
            "bootstrap vs stage2 IR mismatch for {}",
            file.display()
        );
        assert_eq!(ir1, ir2, "stage1 vs stage2 for {}", file.display());
    }
}

#[test]
fn emit_ir_parses_unknown_literal_seven() {
    // Anti-table: a program the fixed string table never knew.
    let prog = write_temp_sal("fn main() -> Int\n    7\n");
    let boot = bootstrap_ir(&prog);
    assert!(
        boot.contains("value: 7"),
        "bootstrap IR must mention ConstInt 7, got:\n{boot}"
    );
    assert!(
        !boot.contains("value: 42"),
        "bootstrap IR for 7 must not be hello's 42:\n{boot}"
    );

    let stage1 = compile_selfhost(false);
    let ir1 = run_emit_ir(&stage1, &prog);
    assert!(
        ir1.contains("value: 7"),
        "stage1 IR must contain value: 7 (not a hello/empty table hit), got:\n{ir1}"
    );
    assert_eq!(boot, ir1, "bootstrap vs stage1 for literal 7");

    let stage2 = compile_stage2(&stage1);
    let ir2 = run_emit_ir(&stage2, &prog);
    assert!(
        ir2.contains("value: 7"),
        "stage2 IR must contain value: 7, got:\n{ir2}"
    );
    assert_eq!(boot, ir2, "bootstrap vs stage2 for literal 7");
}

#[test]
fn stage1_minus_o_compiles_not_copies() {
    let stage1 = compile_selfhost(false);

    // hello -> exit 42
    let hello_bin = PathBuf::from(format!(
        "/tmp/sal-hello-bin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = Command::new(&stage1)
        .arg(root().join("examples/hello.sal"))
        .arg("-o")
        .arg(&hello_bin)
        .current_dir(root())
        .output()
        .expect("stage1 -o hello");
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 -o hello failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let run = Command::new(&hello_bin).output().expect("run hello bin");
    assert_eq!(run.status.code(), Some(42), "compiled hello must exit 42");

    // main returning 7 -> exit 7
    let seven_src = write_temp_sal("fn main() -> Int\n    7\n");
    let seven_bin = PathBuf::from(format!(
        "/tmp/sal-seven-bin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = Command::new(&stage1)
        .arg(&seven_src)
        .arg("-o")
        .arg(&seven_bin)
        .current_dir(root())
        .output()
        .expect("stage1 -o seven");
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 -o seven failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let run = Command::new(&seven_bin).output().expect("run seven bin");
    assert_eq!(run.status.code(), Some(7), "compiled seven must exit 7");

    // Must not be a copy of stage1 itself
    let stage1_bytes = std::fs::read(&stage1).unwrap();
    let hello_bytes = std::fs::read(&hello_bin).unwrap();
    let seven_bytes = std::fs::read(&seven_bin).unwrap();
    assert_ne!(stage1_bytes, hello_bytes, "hello bin must not be copy of stage1");
    assert_ne!(stage1_bytes, seven_bytes, "seven bin must not be copy of stage1");
    assert_ne!(hello_bytes, seven_bytes, "hello and seven bins must differ");
}

#[test]
fn selfhost_import_two_modules_run() {
    let project = root().join("tests/import_graph");
    let a_path = project.join("a.sal");

    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: project.clone(),
        skip_link: true,
    };
    let boot_ir = {
        let _guard = LINK_LOCK.lock().expect("link lock");
        compile_file(&a_path, &opts)
            .expect("bootstrap compile import graph")
            .ir_text
    };

    let stage1 = compile_selfhost(false);
    let ir1 = run_emit_ir_in(&stage1, &a_path, &project, "import-ir-s1");
    assert_eq!(
        boot_ir, ir1,
        "bootstrap vs stage1 IR for import graph"
    );

    let bin = PathBuf::from(format!(
        "/tmp/sal-import-bin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = Command::new(&stage1)
        .arg(&a_path)
        .arg("-o")
        .arg(&bin)
        .env("SAL_PROJECT_ROOT", &project)
        .current_dir(root())
        .output()
        .expect("stage1 -o import graph");
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 -o import failed: stderr={}",
        String::from_utf8_lossy(&status.stderr)
    );
    let run = Command::new(&bin).output().expect("run import bin");
    assert_eq!(
        run.status.code(),
        Some(42),
        "import graph must return helper(): stderr={} stdout={}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );

    let stage2 = compile_stage2(&stage1);
    let ir2 = run_emit_ir_in(&stage2, &a_path, &project, "import-ir-s2");
    assert_eq!(boot_ir, ir2, "bootstrap vs stage2 IR for import graph");
    assert_eq!(ir1, ir2, "stage1 vs stage2 IR for import graph");
}

#[test]
fn selfhost_instrumented_clean_on_hello() {
    let bin = compile_selfhost(true);
    let hello = root().join("examples/hello.sal");
    let out = Command::new(&bin)
        .arg(&hello)
        .env("SAL_SELFHOST_CACHE", unique_cache_dir("inst"))
        .current_dir(root())
        .output()
        .expect("run instrumented selfhost");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "instrumented selfhost exit != 0: stderr={stderr}"
    );
    for ev in [
        "LEAK",
        "OOB",
        "USE_AFTER_FREE",
        "DOUBLE_FREE",
        "BAD_PLACE",
        "NAN",
        "INF",
    ] {
        assert!(
            !stderr.contains(ev),
            "instrumented stderr contains {ev}: {stderr}"
        );
    }
    let boot = bootstrap_ir(&hello);
    let got = String::from_utf8(out.stdout).expect("utf8");
    assert_eq!(boot, got, "instrumented emit IR must still match bootstrap");
}

#[test]
fn parse_failure_returns_result_err_nonzero() {
    let stage1 = compile_selfhost(false);
    let bad = write_temp_sal("this is not a sal program !!!\n");
    let cache = unique_cache_dir("parse-err");
    let out = Command::new(&stage1)
        .arg(&bad)
        .env("SAL_SELFHOST_CACHE", &cache)
        .current_dir(root())
        .output()
        .expect("run bad parse");
    assert_ne!(
        out.status.code(),
        Some(0),
        "invalid source must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Err("),
        "stderr must show Result Err, got: {stderr}"
    );
    assert!(
        out.stdout.is_empty() || !String::from_utf8_lossy(&out.stdout).contains("fn main:"),
        "parse failure must not emit IR"
    );

    // Valid source still exits 0 and emits IR
    let good = write_temp_sal("fn main() -> Int\n    7\n");
    let out_ok = Command::new(&stage1)
        .arg(&good)
        .env("SAL_SELFHOST_CACHE", unique_cache_dir("parse-ok"))
        .current_dir(root())
        .output()
        .expect("run good");
    assert_eq!(out_ok.status.code(), Some(0));
    let ir = String::from_utf8_lossy(&out_ok.stdout);
    assert!(ir.contains("value: 7"), "got: {ir}");
}

#[test]
fn incremental_ir_cache_hit_on_second_emit() {
    let stage1 = compile_selfhost(false);
    let prog = write_temp_sal("fn main() -> Int\n    11\n");
    let cache = unique_cache_dir("inc");

    let out1 = Command::new(&stage1)
        .arg(&prog)
        .env("SAL_SELFHOST_CACHE", &cache)
        .current_dir(root())
        .output()
        .expect("emit 1");
    assert_eq!(out1.status.code(), Some(0), "stderr={}", String::from_utf8_lossy(&out1.stderr));
    let ir1 = String::from_utf8(out1.stdout).expect("utf8");
    assert!(ir1.contains("value: 11"));

    let metas: Vec<_> = std::fs::read_dir(&cache)
        .expect("cache dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("meta"))
        .collect();
    assert!(!metas.is_empty(), "expected cache meta after first emit");
    let meta0 = std::fs::read_to_string(&metas[0]).expect("meta0");
    assert!(meta0.contains("hit=0"), "first emit must be miss: {meta0}");

    let out2 = Command::new(&stage1)
        .arg(&prog)
        .env("SAL_SELFHOST_CACHE", &cache)
        .current_dir(root())
        .output()
        .expect("emit 2");
    assert_eq!(out2.status.code(), Some(0));
    let ir2 = String::from_utf8(out2.stdout).expect("utf8");
    assert_eq!(ir1, ir2, "cached IR must match");
    let meta1 = std::fs::read_to_string(&metas[0]).expect("meta1");
    assert!(meta1.contains("hit=1"), "second emit must be hit: {meta1}");

    // Source change must miss
    std::fs::write(&prog, "fn main() -> Int\n    12\n").expect("rewrite");
    let out3 = Command::new(&stage1)
        .arg(&prog)
        .env("SAL_SELFHOST_CACHE", &cache)
        .current_dir(root())
        .output()
        .expect("emit 3");
    assert_eq!(out3.status.code(), Some(0));
    let ir3 = String::from_utf8(out3.stdout).expect("utf8");
    assert!(ir3.contains("value: 12"), "got: {ir3}");
    assert_ne!(ir1, ir3);
    let metas2: Vec<_> = std::fs::read_dir(&cache)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("meta"))
        .collect();
    let any_miss = metas2.iter().any(|p| {
        std::fs::read_to_string(p)
            .map(|s| s.contains("hit=0"))
            .unwrap_or(false)
    });
    assert!(any_miss, "changed source must create a miss meta");
}

#[test]
fn selfhost_model_package_tests_exit_zero() {
    let models_src = std::fs::read_to_string(root().join("selfhost/models.sal")).expect("models.sal");
    assert!(
        models_src.contains("on gpu") && models_src.contains("on tpu") && models_src.contains("matmul"),
        "models.sal must run matmul inside on gpu/on tpu"
    );
    assert!(
        models_src.contains("1.0") && models_src.contains("2.0") && models_src.contains("3.0") && models_src.contains("4.0"),
        "A×I coefficients must live in models.sal, not a C builtin"
    );
    assert!(
        !models_src.contains("place_matmul_ai_sum"),
        "models.sal must not call place_matmul_ai_sum"
    );

    // Compile models.sal with the bootstrap LLVM path and require real place launches.
    // Drop incremental objs so a codegen-only compiler change cannot reuse a stale .o.
    let _ = std::fs::remove_dir_all(root().join("target").join("incremental"));
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let _guard = LINK_LOCK.lock().expect("link lock");
    let art = compile_file(&root().join("selfhost/models.sal"), &opts).expect("compile models.sal");
    assert!(
        art.llvm.contains("call void @sal_matmul_f32")
            && art.llvm.contains("call void @sal_on_enter(i32 1)")
            && art.llvm.contains("call void @sal_on_enter(i32 2)"),
        "expected gpu/tpu on-enter + matmul in LLVM:\n{}",
        art.llvm
    );
    assert!(
        !art.llvm.contains("sal_place_matmul_ai_sum"),
        "LLVM must not call removed place_matmul_ai_sum builtin"
    );
    let models_bin = art.binary.expect("models binary");
    let models_out = Command::new(&models_bin).output().expect("run models");
    assert_eq!(
        models_out.status.code(),
        Some(10),
        "models.sal should exit with matmul element sum: stderr={} stdout={}",
        String::from_utf8_lossy(&models_out.stderr),
        String::from_utf8_lossy(&models_out.stdout)
    );
    let models_stdout = String::from_utf8_lossy(&models_out.stdout);
    assert!(
        models_stdout.contains("sum=10"),
        "expected sum=10 from on-gpu/tpu matmul, got: {models_stdout}"
    );
    assert!(
        models_stdout.contains("gpu=1") && models_stdout.contains("tpu=1"),
        "expected gpu=1 tpu=1 place launches, got: {models_stdout}"
    );
    drop(_guard);

    let stage1 = compile_selfhost(false);
    let bootstrap = env!("CARGO_BIN_EXE_sal");
    let out = Command::new(&stage1)
        .arg("test")
        .env("SAL_SELFHOST_CACHE", unique_cache_dir("models"))
        .env("SAL_BOOTSTRAP", bootstrap)
        .current_dir(root())
        .output()
        .expect("selfhost test");
    assert_eq!(
        out.status.code(),
        Some(0),
        "model package tests failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sum=10"),
        "expected numeric sum=10 from gpu/tpu place matmul, got: {stdout}"
    );
    assert!(
        stdout.contains("gpu=1") && stdout.contains("tpu=1"),
        "expected gpu=1 tpu=1 place launches, got: {stdout}"
    );
    assert!(
        stdout.contains("model_tests_ok"),
        "expected model_tests_ok, got: {stdout}"
    );
}
