use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sal_out() -> PathBuf {
    root().join("target").join("sal-out")
}

/// `compile_file` links to a shared `target/sal-out/a.out` when `output` is unset.
static LINK_LOCK: Mutex<()> = Mutex::new(());

/// Stage1 compiling `selfhost/main.sal` / `emit.sal` peaks near 5 GB RSS. Without this
/// lock, `cargo test` on many threads runs several of those at once and OOMs Cursor.
static STAGE1_LOCK: Mutex<()> = Mutex::new(());

fn stage1_output(mut cmd: std::process::Command) -> std::process::Output {
    std::thread::Builder::new()
        .name("sal-stage1".into())
        .stack_size(256 << 20)
        .spawn(move || {
            let _guard = STAGE1_LOCK.lock().expect("stage1 lock");
            cmd.output().expect("run stage1")
        })
        .expect("spawn stage1")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// Cross-process lock so Cursor (one test per process) and CI share the same
/// `target/sal-out/selfhost-stage{1,2}` builds instead of each recompiling.
fn with_selfhost_build_lock<T>(f: impl FnOnce() -> T) -> T {
    let dir = sal_out();
    fs::create_dir_all(&dir).expect("sal-out");
    let lock_path = dir.join("selfhost-build.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("selfhost build lock");
    let fd = file.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
    assert_eq!(rc, 0, "flock LOCK_EX failed");
    let out = f();
    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    // Keep `file` alive until after unlock.
    drop(file);
    out
}

fn selfhost_fingerprint(instrument: bool) -> u64 {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, files);
            } else if p.extension().is_some_and(|x| x == "sal") {
                files.push(p);
            }
        }
    }

    let root = root();
    let mut files = Vec::new();
    walk(&root.join("selfhost"), &mut files);
    files.push(root.join("std/prelude.sal"));
    files.push(root.join("runtime/sal_runtime.c"));
    files.push(root.join("runtime/sal_threads.c"));
    files.sort();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    instrument.hash(&mut hasher);
    for path in files {
        path.strip_prefix(&root)
            .expect("source path under repository")
            .hash(&mut hasher);
        fs::read(path).expect("read selfhost build input").hash(&mut hasher);
    }
    fs::read(env!("CARGO_BIN_EXE_sal"))
        .expect("read bootstrap binary")
        .hash(&mut hasher);
    hasher.finish()
}

fn stage2_fingerprint(stage1: &Path) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fs::read(stage1)
        .expect("read stage1 build input")
        .hash(&mut hasher);
    hasher.finish()
}

fn fingerprint_path(bin: &Path) -> PathBuf {
    bin.with_extension("fingerprint")
}

fn fingerprint_matches(bin: &Path, fingerprint: u64) -> bool {
    bin.is_file()
        && fs::read_to_string(fingerprint_path(bin))
            .is_ok_and(|stored| stored == fingerprint.to_string())
}

fn write_fingerprint(bin: &Path, fingerprint: u64) {
    fs::write(fingerprint_path(bin), fingerprint.to_string()).expect("write build fingerprint");
}

fn corpus_files() -> Vec<PathBuf> {
    let root = root();
    [
        "corpus/moves.sal",
        "corpus/effects.sal",
        "corpus/drop_ro_params.sal",
        "corpus/drop_flags.sal",
        "corpus/drop_list_struct.sal",
        "corpus/drop_temporaries.sal",
        "corpus/enum_match.sal",
        "corpus/param_modes.sal",
        "corpus/drop_struct_inline_list.sal",
        "corpus/places.sal",
        "corpus/forward_fused.sal",
        "corpus/load_salt.sal",
        "corpus/instrument_oob.sal",
        "corpus/instrument_leak.sal",
        "corpus/string_plus.sal",
        "corpus/string_concat_n.sal",
        "corpus/arena.sal",
        "corpus/column_ops.sal",
        "corpus/newtypes.sal",
        "corpus/early_return.sal",
        "corpus/drop_rebind.sal",
        "examples/hello.sal",
        "examples/forward.sal",
    ]
    .into_iter()
    .map(|p| root.join(p))
    .collect()
}

/// The text of `sal emit ir`: the whole bootstrap pipeline, with the moves the borrow checker
/// publishes (SPEC «Memoria»), not a bare `lower_program`.
fn bootstrap_ir(path: &Path) -> String {
    sal_compiler::compile::bootstrap_ir_text(path, &root(), Default::default())
        .unwrap_or_else(|d| panic!("bootstrap failed on {}: {:?}", path.display(), d))
}

/// Stage1 lives at `target/sal-out/selfhost-stage1{,-inst}`: once per process (OnceLock)
/// and shared across local Cursor runs / CI processes via the file lock + mtime check.
fn compile_selfhost(instrument: bool) -> PathBuf {
    static PLAIN: OnceLock<PathBuf> = OnceLock::new();
    static INST: OnceLock<PathBuf> = OnceLock::new();
    let cell = if instrument { &INST } else { &PLAIN };
    cell.get_or_init(|| ensure_stage1(instrument)).clone()
}

fn stage1_path(instrument: bool) -> PathBuf {
    sal_out().join(if instrument {
        "selfhost-stage1-inst"
    } else {
        "selfhost-stage1"
    })
}

fn ensure_stage1(instrument: bool) -> PathBuf {
    let out = stage1_path(instrument);
    with_selfhost_build_lock(|| {
        let fingerprint = selfhost_fingerprint(instrument);
        if fingerprint_matches(&out, fingerprint) {
            return out.clone();
        }
        let dest = out.clone();
        // The debug compile of the selfhost recurses deeper than the default thread stack.
        std::thread::Builder::new()
            .name("sal-selfhost".into())
            .stack_size(256 << 20)
            .spawn(move || compile_selfhost_to(&dest, instrument))
            .expect("spawn selfhost compile")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
        write_fingerprint(&out, fingerprint);
        out
    })
}

fn compile_selfhost_to(out: &Path, instrument: bool) {
    let _guard = LINK_LOCK.lock().expect("link lock");
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("sal-out");
    }
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: Some(out.to_path_buf()),
        release: false,
        instrument,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&root().join("selfhost/main.sal"), &opts).expect("compile selfhost");
    assert_eq!(
        art.binary.as_deref(),
        Some(out),
        "stage1 must be written to {}",
        out.display()
    );
}

fn run_emit_ir(bin: &Path, input: &Path) -> String {
    let current = std::thread::current();
    let test = current.name().unwrap_or("unknown");
    run_emit_ir_in(bin, input, &root(), &format!("emit-{test}"))
}

fn run_emit_ir_in(bin: &Path, input: &Path, project: &Path, cache_tag: &str) -> String {
    let cache = shared_cache_dir(cache_tag);
    let input = input.strip_prefix(project).unwrap_or(input);
    let mut cmd = Command::new(bin);
    cmd.arg(input)
        .env("SAL_SELFHOST_CACHE", &cache)
        .env("SAL_PROJECT_ROOT", project)
        .current_dir(project);
    let out = stage1_output(cmd);
    assert_eq!(
        out.status.code(),
        Some(0),
        "selfhost failed on {} ({:?}): stderr={}",
        input.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 ir")
}

fn compile_stage2(stage1: &Path) -> PathBuf {
    static STAGE2: OnceLock<PathBuf> = OnceLock::new();
    let stage1 = stage1.to_path_buf();
    STAGE2
        .get_or_init(|| ensure_stage2(&stage1))
        .clone()
}

fn stage2_path() -> PathBuf {
    sal_out().join("selfhost-stage2")
}

fn ensure_stage2(stage1: &Path) -> PathBuf {
    let out = stage2_path();
    with_selfhost_build_lock(|| {
        let fingerprint = stage2_fingerprint(stage1);
        if fingerprint_matches(&out, fingerprint) {
            return out;
        }
        compile_stage2_to(stage1, &out);
        write_fingerprint(&out, fingerprint);
        out
    })
}

fn compile_stage2_to(stage1: &Path, out: &Path) {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("sal-out");
    }
    let mut cmd = Command::new(stage1);
    cmd.arg("selfhost/main.sal")
        .arg("-o")
        .arg(out)
        .current_dir(root());
    let status = stage1_output(cmd);
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 failed to produce stage2 ({:?}): stderr={}",
        status.status,
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(out.is_file(), "stage2 binary missing at {}", out.display());
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

/// One cache directory per tag and process. A unique stamp per call used to leave
/// thousands of dirs on tmpfs (RAM); reuse keeps the footprint flat.
fn shared_cache_dir(tag: &str) -> PathBuf {
    static RUN_ID: OnceLock<u64> = OnceLock::new();
    let run_id = RUN_ID.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_secs()
    });
    let tag_hash = tag.bytes().fold(0u16, |h, b| {
        h.wrapping_mul(31).wrapping_add(u16::from(b))
    }) & 0x0fff;
    let dir = PathBuf::from(format!(
        "/tmp/s{:03x}-{:x}-{:x}",
        tag_hash,
        std::process::id(),
        run_id
    ));
    std::fs::create_dir_all(&dir).expect("cache dir");
    dir
}

fn unique_cache_dir(tag: &str) -> PathBuf {
    shared_cache_dir(tag)
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
    let nm = Command::new("nm").arg(&stage1).output().expect("nm stage1");
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

    let mut files = corpus_files();
    files.extend(
        [
            "tests/lang/ownership/mut_ref_local_parameter.sal",
            "tests/lang/ownership/mut_ref_local_parameter_reborrow.sal",
            "tests/lang/ownership/mut_ref_local_parameter_struct.sal",
            "tests/lang/ownership/mut_ref_local_write_through.sal",
            "tests/lang/ownership/mut_ref_local_loop.sal",
            "tests/lang/ownership/mut_ref_local_reborrow.sal",
            "tests/lang/ownership/mut_ref_local_one_field.sal",
            "tests/lang/ownership/mut_ref_local_string_field.sal",
            "tests/lang/ownership/mut_ref_local_shared_reborrow.sal",
            "tests/lang/ownership/mut_ref_local_shared_field.sal",
            "tests/lang/ownership/mut_ref_in_struct_field.sal",
            "tests/lang/ownership/mut_ref_local_return.sal",
            "tests/lang/ownership/mut_ref_local_multiple.sal",
            "tests/lang/ownership/mut_ref_local_inferred_expr.sal",
            "tests/lang/ownership/mut_ref_local_inferred_builtin.sal",
            "tests/lang/ownership/mut_ref_local_string_reassign.sal",
            "tests/lang/ownership/mut_ref_local_bool_float.sal",
            "tests/lang/ownership/mut_ref_local_arena.sal",
        ]
        .iter()
        .map(|rel| root().join(rel)),
    );
    for file in files {
        let boot = bootstrap_ir(&file);
        let ir1 = run_emit_ir(&stage1, &file);
        let ir2 = run_emit_ir(&stage2, &file);
        assert_eq!(
            boot,
            ir1,
            "bootstrap vs stage1 IR mismatch for {}",
            file.display()
        );
        assert_eq!(
            boot,
            ir2,
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
fn grouped_newlines_match_bootstrap_ir() {
    let prog = write_temp_sal(
        "fn add(a: Int, b: Int) -> Int\n    a + b\n\nfn main() -> Int\n    add(\n        (\n            1\n            + 2\n        ),\n        4\n    )\n",
    );
    let boot = bootstrap_ir(&prog);
    let stage1 = compile_selfhost(false);
    let ir1 = run_emit_ir(&stage1, &prog);
    assert_eq!(
        boot, ir1,
        "bootstrap vs stage1 for a call split inside parentheses"
    );
    let stage2 = compile_stage2(&stage1);
    let ir2 = run_emit_ir(&stage2, &prog);
    assert_eq!(
        boot, ir2,
        "bootstrap vs stage2 for a call split inside parentheses"
    );

    let bin = PathBuf::from(format!(
        "/tmp/sal-grouped-bin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&prog);
        cmd.arg("-o");
        cmd.arg(&bin);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 failed to compile grouped newlines: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let run = Command::new(&bin).output().expect("run grouped bin");
    assert_eq!(
        run.status.code(),
        Some(7),
        "add((1 + 2), 4) split across lines must exit 7; stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// stage1 compiles this through its C backend. The string must be the one the bootstrap prints:
/// a call in the middle of a concatenation is a fresh string, not a freed literal reused.
#[test]
fn stage1_c_path_prints_call_inside_concat() {
    let stage1 = compile_selfhost(false);
    let src = root().join("tests/lang/drops/drop_call_in_string_concat.sal");
    let bin = PathBuf::from(format!(
        "/tmp/sal-concat-bin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&src);
        cmd.arg("-o");
        cmd.arg(&bin);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 failed to compile the concat program: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let run = Command::new(&bin).output().expect("run concat bin");
    assert_eq!(
        run.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim_end(),
        "(b == 0LL);"
    );
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
    let status = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(root().join("examples/hello.sal"));
        cmd.arg("-o");
        cmd.arg(&hello_bin);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
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
    let status = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&seven_src);
        cmd.arg("-o");
        cmd.arg(&seven_bin);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
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
    assert_ne!(
        stage1_bytes, hello_bytes,
        "hello bin must not be copy of stage1"
    );
    assert_ne!(
        stage1_bytes, seven_bytes,
        "seven bin must not be copy of stage1"
    );
    assert_ne!(hello_bytes, seven_bytes, "hello and seven bins must differ");
}

#[test]
fn selfhost_import_two_modules_run() {
    let project = root().join("tests/import_graph");
    let a_path = project.join("a.sal");

    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
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
    assert_eq!(boot_ir, ir1, "bootstrap vs stage1 IR for import graph");

    let bin = PathBuf::from(format!(
        "/tmp/sal-import-bin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&a_path);
        cmd.arg("-o");
        cmd.arg(&bin);
        cmd.env("SAL_PROJECT_ROOT", &project);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
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
fn selfhost_import_alias_ir_and_run() {
    let project = root();
    let main_path = project.join("examples/import_alias.sal");

    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: project.clone(),
        skip_link: true,
    };
    let boot_ir = {
        let _guard = LINK_LOCK.lock().expect("link lock");
        compile_file(&main_path, &opts)
            .expect("bootstrap compile import_alias")
            .ir_text
    };

    let stage1 = compile_selfhost(false);
    let ir1 = run_emit_ir_in(&stage1, &main_path, &project, "import-alias-ir-s1");
    assert_eq!(
        boot_ir, ir1,
        "bootstrap vs stage1 IR for import alias example"
    );

    let bin = PathBuf::from(format!(
        "/tmp/sal-import-alias-bin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&main_path);
        cmd.arg("-o");
        cmd.arg(&bin);
        cmd.env("SAL_PROJECT_ROOT", &project);
        cmd.current_dir(&project);
        stage1_output(cmd)
    };
    assert_eq!(
        status.status.code(),
        Some(0),
        "stage1 -o import_alias failed: stderr={}",
        String::from_utf8_lossy(&status.stderr)
    );
    let run = Command::new(&bin).output().expect("run import_alias bin");
    assert_eq!(
        run.status.code(),
        Some(10),
        "import_alias must return L.helper() == 10: stderr={} stdout={}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );

    let stage2 = compile_stage2(&stage1);
    let ir2 = run_emit_ir_in(&stage2, &main_path, &project, "import-alias-ir-s2");
    assert_eq!(
        boot_ir, ir2,
        "bootstrap vs stage2 IR for import alias example"
    );
    assert_eq!(ir1, ir2, "stage1 vs stage2 IR for import alias example");
}

#[test]
fn selfhost_instrumented_clean_on_hello() {
    let bin = compile_selfhost(true);
    let hello = root().join("examples/hello.sal");
    let out = {
        let mut cmd = Command::new(&bin);
        cmd.arg(&hello);
        cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("inst"));
        cmd.current_dir(root());
        stage1_output(cmd)
    };
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
fn selfhost_instrumented_releases_assignment_nodes() {
    let bin = compile_selfhost(true);
    let program = write_temp_sal("fn main() -> Int\n    x = 1\n    x\n");
    let out = {
        let mut cmd = Command::new(&bin);
        cmd.arg(&program)
            .env("SAL_SELFHOST_CACHE", unique_cache_dir("inst-assignment"))
            .current_dir(root());
        stage1_output(cmd)
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "instrumented selfhost failed: {stderr}");
    assert!(!stderr.contains("LEAK"), "assignment leaked: {stderr}");
    assert_eq!(bootstrap_ir(&program), String::from_utf8(out.stdout).unwrap());
}

/// The passes that rewrite the body of every function (`desugar_break`, `qualify`, `lower_fns`)
/// must release the old body instead of leaving it behind by the overwrite. Several functions,
/// a `while` and a `break` exercise them.
#[test]
fn selfhost_instrumented_releases_rewritten_fn_bodies() {
    let bin = compile_selfhost(true);
    let program = write_temp_sal(
        "fn first(n: Int) -> Int\n    n + 1\n\nfn second(n: Int) -> Int\n    i = 0\n    while i < n\n        if i == 3\n            break\n        i = i + 1\n    i\n\nfn main() -> Int\n    first(second(5))\n",
    );
    let out = {
        let mut cmd = Command::new(&bin);
        cmd.arg(&program);
        cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("inst-bodies"));
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "instrumented selfhost exit != 0: stderr={stderr}");
    assert!(!stderr.contains("LEAK"), "rewritten bodies leak: {stderr}");
    let boot = bootstrap_ir(&program);
    let got = String::from_utf8(out.stdout).expect("utf8");
    assert_eq!(boot, got, "instrumented emit IR must still match bootstrap");
    let _ = fs::remove_file(&program);
}

/// Programs with imports go through `qualify`, the module graph, monomorphization and the
/// lambda specialization: the prelude with lambdas (`each`, `map`) and a generic from another
/// module. The instrumented selfhost builds them with no event: no `LEAK` of the copies those
/// passes make, and no `DOUBLE_FREE` of a `TProgram` handed by value (`sp_run`).
#[test]
fn selfhost_instrumented_clean_on_imports() {
    let bin = compile_selfhost(true);
    for rel in [
        "tests/lang/collections/list_each.sal",
        "tests/lang/collections/list_map_strings.sal",
        "tests/lang/generics/generic_cross_module.sal",
        "tests/lang/generics/generic_struct_cross_module.sal",
        "tests/lang/collections/list_prelude_map_reduce.sal",
        "tests/lang/modules/const_import.sal",
        "tests/lang/modules/method_module.sal",
        "tests/lang/threads/channel_producer.sal",
    ] {
        let program = root().join(rel);
        let exe = PathBuf::from(format!("/tmp/sal-inst-imports-{}", std::process::id()));
        let out = {
            let mut cmd = Command::new(&bin);
            cmd.arg(&program)
                .arg("-o")
                .arg(&exe)
                .env("SAL_SELFHOST_CACHE", unique_cache_dir("inst-imports"))
                .current_dir(root());
            stage1_output(cmd)
        };
        let _ = fs::remove_file(&exe);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{rel}: instrumented selfhost failed: {stderr}");
        for ev in ["LEAK", "OOB", "USE_AFTER_FREE", "DOUBLE_FREE", "BAD_PLACE"] {
            assert!(!stderr.contains(ev), "{rel}: instrumented stderr contains {ev}: {stderr}");
        }
    }
}

#[test]
fn selfhost_clones_and_drops_enum_struct_payload() {
    let stage1 = compile_selfhost(false);
    let program = root().join("tests/memory/enum_struct_clone.sal");
    let bin = PathBuf::from(format!("/tmp/sal-enum-struct-clone-{}", std::process::id()));
    let ir = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&program).current_dir(root());
        stage1_output(cmd)
    };
    assert!(ir.status.success(), "selfhost IR: {}", String::from_utf8_lossy(&ir.stderr));
    assert_eq!(bootstrap_ir(&program), String::from_utf8(ir.stdout).unwrap());
    let built = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&program).arg("-o").arg(&bin).current_dir(root());
        stage1_output(cmd)
    };
    assert!(built.status.success(), "selfhost build: {}", String::from_utf8_lossy(&built.stderr));
    let run = Command::new(&bin).output().expect("run enum struct clone");
    assert_eq!(run.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&run.stderr));
}

#[test]
fn undeclared_effect_is_rejected_like_bootstrap() {
    let stage1 = compile_selfhost(false);
    let cases = [
        ("io", "fn main() -> Int\n    print_str(d\"x\")\n    0\n"),
        ("alloc", "fn main() -> Int\n    s = strdup(d\"x\")\n    0\n"),
        ("gpu", "fn main() -> Int ! alloc\n    t = tensor[[1.0, 2.0], [3.0, 4.0]]\n    g = to gpu t\n    0\n"),
    ];
    for (effect, src) in cases {
        let prog = write_temp_sal(src);
        let out = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&prog);
            cmd.args(["-o", "/tmp/sal-fx-bad.out"]);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("fx-bad"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        assert_ne!(
            out.status.code(),
            Some(0),
            "`{effect}` use must be rejected"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("uses effect `{effect}`")),
            "stderr for {effect}: {stderr}"
        );
    }
    let ok = write_temp_sal("fn main() -> Int ! io, alloc\n    print_str(d\"x\")\n    0\n");
    let out = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&ok);
        cmd.args(["-o", "/tmp/sal-fx-ok.out"]);
        cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("fx-ok"));
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn parse_failure_returns_result_err_nonzero() {
    let stage1 = compile_selfhost(false);
    let bad = write_temp_sal("this is not a sal program !!!\n");
    let cache = unique_cache_dir("parse-err");
    let out = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&bad);
        cmd.env("SAL_SELFHOST_CACHE", &cache);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
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
    let out_ok = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&good);
        cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("parse-ok"));
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert_eq!(out_ok.status.code(), Some(0));
    let ir = String::from_utf8_lossy(&out_ok.stdout);
    assert!(ir.contains("value: 7"), "got: {ir}");
}

#[test]
fn incremental_ir_cache_hit_on_second_emit() {
    let stage1 = compile_selfhost(false);
    let prog = write_temp_sal("fn main() -> Int\n    11\n");
    let cache = unique_cache_dir("inc");

    let out1 = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&prog);
        cmd.env("SAL_SELFHOST_CACHE", &cache);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert_eq!(
        out1.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out1.stderr)
    );
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

    let out2 = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&prog);
        cmd.env("SAL_SELFHOST_CACHE", &cache);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert_eq!(out2.status.code(), Some(0));
    let ir2 = String::from_utf8(out2.stdout).expect("utf8");
    assert_eq!(ir1, ir2, "cached IR must match");
    let meta1 = std::fs::read_to_string(&metas[0]).expect("meta1");
    assert!(meta1.contains("hit=1"), "second emit must be hit: {meta1}");

    // Source change must miss
    std::fs::write(&prog, "fn main() -> Int\n    12\n").expect("rewrite");
    let out3 = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&prog);
        cmd.env("SAL_SELFHOST_CACHE", &cache);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
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

/// Stage2 runs the selfhost C backend (`emit_c_*`). A regression there once
/// lowered `StrPath.raw + "/" + key` with a bogus left operand, wrote cache
/// paths under `/`, and segfaulted in `write_file`. This test ensures stage2
/// actually populates `SAL_SELFHOST_CACHE` with `.ir`/`.meta` artifacts.
#[test]
fn stage2_emit_ir_writes_selfhost_cache() {
    let stage1 = compile_selfhost(false);
    let stage2 = compile_stage2(&stage1);
    let prog = write_temp_sal("fn main() -> Int\n    13\n");
    let cache = unique_cache_dir("stage2-cache");

    let out = Command::new(&stage2)
        .arg(&prog)
        .env("SAL_SELFHOST_CACHE", &cache)
        .env("SAL_PROJECT_ROOT", root())
        .current_dir(root())
        .output()
        .expect("stage2 emit");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stage2 failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ir = String::from_utf8(out.stdout).expect("utf8");
    assert!(ir.contains("value: 13"), "got: {ir}");

    let ir_files: Vec<_> = std::fs::read_dir(&cache)
        .expect("cache dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("ir"))
        .collect();
    assert!(
        !ir_files.is_empty(),
        "stage2 must write at least one .ir under SAL_SELFHOST_CACHE ({})",
        cache.display()
    );
    for p in &ir_files {
        assert!(
            p.starts_with(&cache),
            "cache artifact must live under SAL_SELFHOST_CACHE: {}",
            p.display()
        );
        let body = std::fs::read_to_string(p).expect("read cached ir");
        assert!(
            body.contains("fn main:"),
            "cached .ir must contain lowered IR: {}",
            p.display()
        );
    }

    let metas: Vec<_> = std::fs::read_dir(&cache)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("meta"))
        .collect();
    assert!(!metas.is_empty(), "expected .meta after stage2 emit");
    let meta0 = std::fs::read_to_string(&metas[0]).expect("meta");
    assert!(meta0.contains("hit=0"), "first emit must be miss: {meta0}");
}

#[test]
fn selfhost_model_package_tests_exit_zero() {
    let models_src =
        std::fs::read_to_string(root().join("selfhost/models.sal")).expect("models.sal");
    assert!(
        models_src.contains("on gpu")
            && models_src.contains("on tpu")
            && models_src.contains("matmul"),
        "models.sal must run matmul inside on gpu/on tpu"
    );
    assert!(
        models_src.contains("1.0")
            && models_src.contains("2.0")
            && models_src.contains("3.0")
            && models_src.contains("4.0"),
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
        toolchain: Default::default(),
        output: None,
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
            && art.llvm.contains("call void @sal_on_enter(i64 1)")
            && art.llvm.contains("call void @sal_on_enter(i64 2)"),
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
        Some(0),
        "models.sal returns its last expression (0): stderr={} stdout={}",
        String::from_utf8_lossy(&models_out.stderr),
        String::from_utf8_lossy(&models_out.stdout)
    );
    let models_stdout = String::from_utf8_lossy(&models_out.stdout);
    assert!(
        models_stdout.contains("argmax=3"),
        "expected argmax=3 from on-gpu/tpu matmul, got: {models_stdout}"
    );
    assert!(
        models_stdout.contains("gpu=1") && models_stdout.contains("tpu=1"),
        "expected gpu=1 tpu=1 place launches, got: {models_stdout}"
    );
    drop(_guard);

    let stage1 = compile_selfhost(false);
    let bootstrap = env!("CARGO_BIN_EXE_sal");
    let out = {
        let mut cmd = Command::new(&stage1);
        cmd.arg("test");
        cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("models"));
        cmd.env("SAL_BOOTSTRAP", bootstrap);
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert_eq!(
        out.status.code(),
        Some(0),
        "model package tests failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("argmax=3"),
        "expected argmax=3 from gpu/tpu place matmul, got: {stdout}"
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

#[test]
fn selfhost_resolves_git_package_import() {
    use std::fs;
    use std::process::Command;
    use std::sync::Mutex;

    static PACKAGE_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    }

    let repo = tempfile::tempdir().expect("repo");
    fs::write(
        repo.path().join("pkg.sal"),
        "fn dep_value() -> Int\n    9\n",
    )
    .expect("pkg");
    run_git(repo.path(), &["init"]);
    run_git(repo.path(), &["config", "user.email", "t@test"]);
    run_git(repo.path(), &["config", "user.name", "t"]);
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-m", "v1"]);
    run_git(repo.path(), &["tag", "v0.2.0"]);
    let url = format!(
        "file://{}",
        repo.path().canonicalize().expect("canonical").display()
    );

    let cache = tempfile::tempdir().expect("cache");
    let project = tempfile::tempdir().expect("project");
    let project_root = project.path().to_path_buf();
    fs::write(
        project_root.join("Sal.toml"),
        format!(
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n\n[dependencies]\ndep = {{ git = \"{url}\", version = \"0.2\" }}\n"
        ),
    )
    .expect("manifest");
    fs::write(
        project_root.join("main.sal"),
        "import \"dep/pkg\"\n\nfn main() -> Int\n    dep_value()\n",
    )
    .expect("main");

    let _guard = PACKAGE_ENV_LOCK.lock().expect("package env lock");
    std::env::set_var("SAL_PACKAGES", cache.path());
    sal_compiler::packages::fetch_packages(&project_root, false).expect("fetch");

    let stage1 = compile_selfhost(false);
    let out = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(project_root.join("main.sal"));
        cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("pkg-selfhost"));
        cmd.env("SAL_PROJECT_ROOT", &project_root);
        cmd.env("SAL_PACKAGES", cache.path());
        cmd.current_dir(&project_root);
        stage1_output(cmd)
    };
    assert_eq!(
        out.status.code(),
        Some(0),
        "selfhost package import failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ir = String::from_utf8(out.stdout).expect("utf8 ir");
    assert!(
        ir.contains("dep_value") || ir.contains("main"),
        "expected lowered IR, got: {ir}"
    );
}

/// IR text from the bootstrap CLI, which resolves imports like the selfhost does.
fn bootstrap_cli_ir(path: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["emit", "ir"])
        .arg(path)
        .current_dir(root())
        .output()
        .expect("run sal emit ir");
    assert_eq!(
        out.status.code(),
        Some(0),
        "bootstrap emit ir failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 ir")
}

/// Files where the selfhost already emits the same IR as the bootstrap. Grow this
/// list as the selfhost closes gaps; never shrink it.
const PARITY_FILES: &[&str] = &[
    "benches/framebuffer/fill_copy.sal",
    "benches/speed/work.sal",
    "corpus/bytes_if_phi.sal",
    "corpus/column_ops.sal",
    "corpus/early_return.sal",
    "corpus/effects.sal",
    "corpus/forward_fused.sal",
    "corpus/load_salt.sal",
    "corpus/moves.sal",
    "corpus/newtypes.sal",
    "corpus/drop_ro_params.sal",
    "corpus/drop_flags.sal",
    "corpus/drop_list_struct.sal",
    "corpus/drop_temporaries.sal",
    "corpus/drop_rebind.sal",
    "corpus/enum_match.sal",
    "corpus/param_modes.sal",
    "corpus/drop_struct_inline_list.sal",
    "corpus/places.sal",
    "corpus/string_plus.sal",
    "corpus/string_concat_n.sal",
    "corpus/arena.sal",
    "examples/column_cpu.sal",
    "examples/column_gpu.sal",
    "examples/forward.sal",
    "examples/forward_salt.sal",
    "examples/gpu_roundtrip.sal",
    "examples/hello.sal",
    "examples/import_alias_left.sal",
    "examples/import_alias_right.sal",
    "examples/import_alias.sal",
    "examples/infer/tiny_decode.sal",
    "examples/infer/tiny_prefill.sal",
    "examples/instrument_nan.sal",
    "examples/instrument_oob.sal",
    "examples/plot_csv.sal",
    "examples/salt_matmul.sal",
    "examples/std_io_smoke.sal",
    "examples/transformer_gpu.sal",
    "pkg.sal",
    "selfhost/ast/tytext.sal",
    "selfhost/sem/attrs.sal",
    "selfhost/pkg/cache.sal",
    "selfhost/emit/cbuiltin.sal",
    "selfhost/emit/cenv.sal",
    "selfhost/sem/checks.sal",
    "selfhost/emit/cir.sal",
    "selfhost/sem/device.sal",
    "selfhost/ir/desc.sal",
    "selfhost/ir/drops.sal",
    "selfhost/sem/effects.sal",
    "selfhost/emit/emit.sal",
    "selfhost/ir/frc.sal",
    "selfhost/ir/fuse.sal",
    "selfhost/ir/fused.sal",
    "selfhost/pkg/str_path.sal",
    "selfhost/sem/infer.sal",
    "selfhost/ir/ir.sal",
    "selfhost/emit/irtext.sal",
    "selfhost/front/lexer.sal",
    "selfhost/main.sal",
    "selfhost/models.sal",
    "selfhost/pkg/modules.sal",
    "selfhost/sem/ownership.sal",
    "selfhost/front/parser.sal",
    "selfhost/pkg/qualify.sal",
    "selfhost/pkg/result.sal",
    "selfhost/ast/tast.sal",
    "selfhost/emit/tcstate.sal",
    "selfhost/ir/tensorops.sal",
    "selfhost/ast/tshape.sal",
    "selfhost/ast/ttypes.sal",
    "standard/cop_dupstring.sal",
    "standard/cop_elsif.sal",
    "standard/cop_matchif.sal",
    "standard/cop_plusliteral.sal",
    "standard/cop_stringplus.sal",
    "standard/lexer.sal",
    "standard/main.sal",
    "standard/patch.sal",
    "standard/util.sal",
    "std/http/client.sal",
    "std/http/sse.sal",
    "std/io/abi.sal",
    "std/io/cwd.sal",
    "std/io/env.sal",
    "std/io/fs.sal",
    "std/io/io.sal",
    "std/io/net.sal",
    "std/io/pipe.sal",
    "std/io/process.sal",
    "std/io/rand.sal",
    "std/io/time.sal",
    "std/io/util.sal",
    "std/json/build.sal",
    "std/prelude.sal",
    "std/text/glob.sal",
    "std/text/pattern.sal",
    "std/text/strutil.sal",
    "tests/fixtures/frame_shop.sal",
    "tests/fixtures/frame_where.sal",
    "tests/glue_escape/alias.sal",
    "tests/glue_escape/branch.sal",
    "tests/glue_escape/chain.sal",
    "tests/glue_escape/ctor.sal",
    "tests/import_graph/a.sal",
    "tests/import_graph/b.sal",
    "tests/lang/collections/dict_int_keys.sal",
    "tests/lang/collections/dict_key_outlives_caller.sal",
    "tests/lang/collections/dict_keys_values.sal",
    "tests/lang/collections/dict_len_remove.sal",
    "tests/lang/drops/drop_dict_get_int_payload_let.sal",
    "tests/lang/drops/drop_dict_get_int_payload_tail.sal",
    "tests/lang/ownership/match_arm_without_binding_keeps_value_ok.sal",
    "tests/lang/ownership/mut_ref_local_parameter.sal",
    "tests/lang/ownership/mut_ref_local_parameter_reborrow.sal",
    "tests/lang/ownership/mut_ref_local_parameter_struct.sal",
    "tests/lang/ownership/mut_ref_local_write_through.sal",
    "tests/lang/ownership/mut_ref_local_loop.sal",
    "tests/lang/ownership/mut_ref_local_reborrow.sal",
    "tests/lang/ownership/mut_ref_local_one_field.sal",
    "tests/lang/ownership/mut_ref_local_string_field.sal",
    "tests/lang/ownership/mut_ref_local_shared_reborrow.sal",
    "tests/lang/ownership/mut_ref_local_shared_field.sal",
    "tests/lang/ownership/mut_ref_in_struct_field.sal",
    "tests/lang/ownership/mut_ref_local_return.sal",
    "tests/lang/ownership/mut_ref_local_multiple.sal",
    "tests/lang/ownership/mut_ref_local_inferred_expr.sal",
    "tests/lang/ownership/mut_ref_local_inferred_builtin.sal",
    "tests/lang/ownership/mut_ref_local_string_reassign.sal",
    "tests/lang/ownership/mut_ref_local_bool_float.sal",
    "tests/lang/ownership/mut_ref_local_arena.sal",
    "tests/lang/strings/get_alias_accumulate.sal",
    "tests/lang/structs/struct_ref_return_field.sal",
    "tests/sal/support/assert.sal",
    "tests/sal/test_glob.sal",
    "tests/sal/test_http_chunk.sal",
    "tests/sal/http_mock_integration.sal",
    "tests/sal/test_json_esc.sal",
    "tests/sal/test_sse.sal",
    "tests/lang/functions/lambda_arg_fn.sal",
    "tests/lang/functions/lambda_arg_multi.sal",
    "tests/lang/functions/lambda_block_body.sal",
    "tests/lang/functions/lambda_capture_int.sal",
    "tests/lang/functions/lambda_effect_inherit.sal",
    "tests/lang/functions/lambda_forward.sal",
    "tests/lang/functions/lambda_generic_ret.sal",
    "tests/lang/functions/lambda_trailing_two_params.sal",
    "tests/lang/ownership/lambda_capture_string.sal",
    "tests/lang/functions/lambda_trailing_method.sal",
    // SPEC «Hilos».
    "tests/lang/threads/spawn_join_sum.sal",
    "tests/lang/threads/spawn_lends_list.sal",
    "tests/lang/threads/spawn_moves_string.sal",
    "tests/lang/threads/thread_drop_joins.sal",
    "tests/lang/threads/thread_join_ends_loan.sal",
    "tests/lang/threads/semaphore_permits.sal",
    "tests/lang/threads/semaphore_workers.sal",
    "tests/lang/threads/mutex_counter.sal",
    "tests/lang/threads/mutex_list.sal",
    "tests/lang/threads/channel_pending_released.sal",
    "tests/lang/threads/channel_producer.sal",
    "tests/lang/threads/channel_recv_match_temp.sal",
    "tests/lang/threads/mutex_string.sal",
    "tests/lang/threads/thread_panic_ends_process.sal",
    // A unit variant arm over a `&Option` (`None`) tests the tag; a `while` tail has no value.
    "tests/lang/drops/drop_match_fresh_enum_call.sal",
    "tests/lang/drops/drop_match_arm_block_local.sal",
    "tests/lang/drops/drop_match_arm_branches.sal",
    "tests/lang/drops/drop_string_moved_by_operator_tail.sal",
    "tests/lang/drops/drop_enum_same_name_sibling_branches.sal",
    "tests/lang/drops/drop_match_arm_moved_param.sal",
    "tests/lang/drops/drop_struct_moved_on_one_branch.sal",
    "tests/lang/control_flow/if_branch_ends_in_while.sal",
    "tests/lang/control_flow/while_tail_has_no_value.sal",
    // A struct of scalar fields in a variant is owned by the enum box (`sal_enum_set_struct_flat`).
    "tests/lang/enums/enum_owns_flat_struct_payload.sal",
    "tests/lang/structs/struct_enum_payload.sal",
];

#[test]
fn selfhost_conditional_string_param_drops_match_bootstrap() {
    let stage1 = compile_selfhost(false);
    for rel in [
        "selfhost/ast/tytext.sal",
        "selfhost/sem/attrs.sal",
        "tests/lang/drops/drop_branch_moved_string_param.sal",
        "tests/lang/drops/drop_branch_moved_string_elsif.sal",
        "tests/lang/drops/drop_branch_moved_struct.sal",
    ] {
        let file = root().join(rel);
        assert_eq!(bootstrap_cli_ir(&file), run_emit_ir(&stage1, &file), "{rel}");
    }
}

#[test]
fn selfhost_phase_one_lambda_ir_matches_bootstrap() {
    let stage1 = compile_selfhost(false);
    for rel in PARITY_FILES.iter().filter(|p| p.contains("/lambda_")) {
        let file = root().join(rel);
        assert_eq!(bootstrap_cli_ir(&file), run_emit_ir(&stage1, &file), "{rel}");
    }
}

#[test]
fn selfhost_ir_matches_bootstrap_on_parity_files() {
    let stage1 = compile_selfhost(false);
    for rel in PARITY_FILES {
        let file = root().join(rel);
        let boot = bootstrap_cli_ir(&file);
        let got = run_emit_ir(&stage1, &file);
        assert_eq!(boot, got, "selfhost IR differs from bootstrap for {rel}");
    }
}

/// (source, moved?) pairs; the bootstrap `sal check` must agree with the selfhost.
const OWNERSHIP_CASES: &[(&str, bool)] = &[
    ("fn eat(s: String) -> Int\n    0\n\nfn main() -> Int\n    s = \"a\"\n    n = eat(s)\n    n + str_len(s)\n", true),
    // A by-value argument moves even when the callee only reads it (SPEC «Memoria»).
    ("fn eat(s: String) -> Int\n    str_len(&s)\n\nfn main() -> Int\n    s = \"a\"\n    n = eat(s)\n    n + str_len(&s)\n", true),
    ("fn eat(s: &String) -> Int\n    str_len(s)\n\nfn main() -> Int\n    s = \"a\"\n    n = eat(&s)\n    n + str_len(&s)\n", false),
    ("fn eat(s: String) -> Int\n    0\n\nfn fwd(s: String) -> Int\n    eat(s)\n\nfn main() -> Int\n    s = \"a\"\n    n = fwd(s)\n    n + str_len(s)\n", true),
    ("fn main() -> Int ! alloc\n    xs = list_new[String]()\n    s = int_to_str(7)\n    _a = list_push(xs, s)\n    str_len(s)\n", true),
    ("struct Tag\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    s = int_to_str(7)\n    t = Tag(s, 1)\n    str_len(s)\n", true),
    ("struct P\n    x: Int\n    y: Int\n\nfn main() -> Int\n    p = P(1, 2)\n    q = p\n    p.x + q.y\n", false),
    ("fn main() -> Int\n    s = \"abc\"\n    t = s\n    u = s\n    0\n", true),
    ("fn main() -> Int\n    a = 1\n    b = a\n    c = a\n    0\n", false),
    ("fn main() -> Int ! alloc\n    t = tensor[[1.0, 2.0], [3.0, 4.0]]\n    u = t\n    v = t\n    0\n", true),
    ("fn main() -> Int\n    s = \"x\"\n    t = s\n    s = \"y\"\n    u = s\n    0\n", false),
    ("fn f(s: String) -> String\n    return s\n    0\n", false),
    ("fn main() -> Int\n    s = \"x\"\n    if 1 == 1\n        t = s\n        0\n    else\n        0\n    u = s\n    0\n", true),
    ("fn f(c: Int) -> Int\n    s = \"x\"\n    if c == 1\n        t = s\n        return 0\n    u = s\n    0\n\nfn main() -> Int\n    0\n", false),
];

#[test]
fn use_after_move_matches_bootstrap_check() {
    let stage1 = compile_selfhost(false);
    for (src, moved) in OWNERSHIP_CASES {
        let prog = write_temp_sal(src);
        let boot = Command::new(env!("CARGO_BIN_EXE_sal"))
            .arg("check")
            .arg(&prog)
            .current_dir(root())
            .output()
            .expect("bootstrap check");
        let boot_moved = String::from_utf8_lossy(&boot.stderr).contains("E_MOVED");
        assert_eq!(boot_moved, *moved, "bootstrap verdict for:\n{src}");
        let out = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&prog);
            cmd.args(["-o", "/tmp/sal-own.out"]);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("own"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        let self_moved = String::from_utf8_lossy(&out.stderr).contains("use of moved value");
        assert_eq!(self_moved, *moved, "selfhost verdict for:\n{src}");
    }
}

/// (source, is_type_error?) pairs; `sal check` must agree with the selfhost.
const TYPE_CASES: &[(&str, bool)] = &[
    ("fn main() -> Int\n    x = y\n    0\n", true),
    ("fn main() -> Int\n    \"a\"\n", true),
    ("fn main() -> Int\n    if true\n        1\n", true),
    (
        "fn main() -> Int\n    if true\n        1\n    else\n        2\n",
        false,
    ),
    (
        "fn main() -> Int\n    if true\n        1\n    else\n        \"s\"\n",
        true,
    ),
    (
        "fn f(a: Int) -> Int\n    a\n\nfn main() -> Int\n    f(1, 2)\n",
        true,
    ),
    (
        "fn f(a: Int) -> Int\n    a\n\nfn main() -> Int\n    f(\"s\")\n",
        true,
    ),
    ("fn main() -> Int\n    return \"s\"\n", true),
    ("fn main() -> Int\n    1 + \"s\"\n", true),
    ("fn main() -> Int\n    \"s\" + 1\n", true),
    ("fn main() -> Int\n    1.5 % 2\n", true),
    ("fn main() -> Int\n    let x: String = 1\n    0\n", true),
    ("fn main() -> Int\n    return 1\n    2\n", true),
    (
        "fn main() -> Int\n    a = 1\n    b = a + 2 * 3\n    b\n",
        false,
    ),
    ("fn main() -> Float\n    1.5 + 2.0\n", false),
];

#[test]
fn scalar_type_errors_match_bootstrap_check() {
    let stage1 = compile_selfhost(false);
    for (src, is_err) in TYPE_CASES {
        let prog = write_temp_sal(src);
        let boot = Command::new(env!("CARGO_BIN_EXE_sal"))
            .arg("check")
            .arg(&prog)
            .current_dir(root())
            .output()
            .expect("bootstrap check");
        let boot_err = String::from_utf8_lossy(&boot.stderr).contains("E_TYPE");
        assert_eq!(boot_err, *is_err, "bootstrap verdict for:\n{src}");
        let out = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&prog);
            cmd.args(["-o", "/tmp/sal-ty.out"]);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("ty"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        assert_eq!(
            out.status.code() != Some(0),
            *is_err,
            "selfhost verdict for:\n{src}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// (source, device/place error?) pairs; `sal check` must agree with the selfhost.
const DEVICE_CASES: &[(&str, bool)] = &[
    (
        "fn main() -> Int ! alloc, gpu\n    t = tensor[[1.0, 2.0], [3.0, 4.0]]\n    on gpu\n        relu(t)\n    0\n",
        true,
    ),
    (
        "fn main() -> Int ! alloc, gpu\n    t = tensor[[1.0, 2.0], [3.0, 4.0]]\n    g = to gpu t\n    on gpu\n        relu(g)\n    0\n",
        false,
    ),
    (
        "fn main() -> Int ! alloc, gpu, tpu\n    t = tensor[[1.0, 2.0], [3.0, 4.0]]\n    g = to gpu t\n    p = to tpu t\n    on gpu\n        matmul(g, p)\n    0\n",
        true,
    ),
    (
        "fn main() -> Int ! alloc, tpu\n    t = tensor[[1.0, 2.0], [3.0, 4.0]]\n    g = to tpu t\n    on tpu kernel\n        relu(g)\n    0\n",
        true,
    ),
    (
        "fn main() -> Int ! alloc, gpu\n    s = \"x\"\n    on gpu\n        s\n    0\n",
        true,
    ),
    (
        "fn main() -> Int ! alloc, gpu\n    s = \"x\"\n    g = to gpu s\n    0\n",
        true,
    ),
];

#[test]
fn device_errors_match_bootstrap_check() {
    let stage1 = compile_selfhost(false);
    for (src, is_err) in DEVICE_CASES {
        let prog = write_temp_sal(src);
        let boot = Command::new(env!("CARGO_BIN_EXE_sal"))
            .arg("check")
            .arg(&prog)
            .current_dir(root())
            .output()
            .expect("bootstrap check");
        let stderr = String::from_utf8_lossy(&boot.stderr);
        let boot_err = stderr.contains("E_PLACE") || stderr.contains("E_DEVICE");
        assert_eq!(boot_err, *is_err, "bootstrap verdict for:\n{src}\n{stderr}");
        let out = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&prog);
            cmd.args(["-o", "/tmp/sal-dev.out"]);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("dev"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        assert_eq!(
            out.status.code() != Some(0),
            *is_err,
            "selfhost verdict for:\n{src}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn multi_field_structs_compile_and_run() {
    let stage1 = compile_selfhost(false);
    let src = "struct P\n    x: Int\n    y: Int\n\nstruct Q\n    a: P\n    z: Int\n\nfn norm(p: P) -> Int\n    p.x * p.x + p.y * p.y\n\nfn main() -> Int\n    p = P(3, 4)\n    q = Q(p, 2)\n    p.x = 6\n    norm(p) + q.z + q.a.y\n";
    let prog = write_temp_sal(src);
    let out_bin = std::env::temp_dir().join(format!("sal-struct-{}.out", std::process::id()));
    let out = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&prog);
        cmd.arg("-o");
        cmd.arg(&out_bin);
        cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("struct"));
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&out_bin).status().expect("run compiled");
    assert_eq!(run.code(), Some(58));
}

/// (source, is_shape_error?) pairs; `sal check` must agree with the selfhost.
const SHAPE_CASES: &[(&str, bool)] = &[
    ("fn f(a: Tensor[F32, 2, 3] on cpu, b: Tensor[F32, 2, 3] on cpu) -> Int ! alloc\n    c = matmul(a, b)\n    0\n\nfn main() -> Int\n    0\n", true),
    ("fn f(a: Tensor[F32, 2, 3] on cpu, b: Tensor[F32, 3, 4] on cpu) -> Int ! alloc\n    c = matmul(a, b)\n    0\n\nfn main() -> Int\n    0\n", false),
    ("fn f(a: Tensor[F32, 2, 3] on cpu, b: Tensor[F32, 3, 4] on cpu, d: Tensor[F32, 5, 6] on cpu) -> Int ! alloc\n    c = matmul(matmul(a, b), d)\n    0\n\nfn main() -> Int\n    0\n", true),
    ("fn f(a: Tensor[F32, 2, 3] on cpu, b: Tensor[I8, 3, 4] on cpu) -> Int ! alloc\n    c = matmul(a, b)\n    0\n\nfn main() -> Int\n    0\n", true),
    ("fn f(a: Tensor[F32, 2, 3] on cpu) -> Int ! alloc\n    c = transpose(a)\n    d = matmul(a, c)\n    0\n\nfn main() -> Int\n    0\n", false),
    ("fn f(a: Tensor[F32, 2, 3] on cpu) -> Int ! alloc\n    c = transpose(a)\n    d = matmul(c, a)\n    e = matmul(d, a)\n    0\n\nfn main() -> Int\n    0\n", true),
    ("fn f(a: Tensor[F32, 2, 3] on cpu) -> Int ! alloc\n    c = reshape(a, 3, 2)\n    d = matmul(a, c)\n    0\n\nfn main() -> Int\n    0\n", false),
    ("fn f(a: Tensor[F32, 2, 3] on cpu) -> Int ! alloc\n    c = reshape(a, 4, 2)\n    0\n\nfn main() -> Int\n    0\n", true),
    ("fn f(a: Tensor[F32, 2, 3] on cpu) -> Int ! alloc\n    c = softmax(a, 5)\n    0\n\nfn main() -> Int\n    0\n", true),
];

#[test]
fn shape_errors_match_bootstrap_check() {
    let stage1 = compile_selfhost(false);
    for (src, bad) in SHAPE_CASES {
        let prog = write_temp_sal(src);
        let boot = Command::new(env!("CARGO_BIN_EXE_sal"))
            .arg("check")
            .arg(&prog)
            .current_dir(root())
            .output()
            .expect("bootstrap check");
        let boot_bad = String::from_utf8_lossy(&boot.stderr).contains("E_SHAPE");
        assert_eq!(boot_bad, *bad, "bootstrap verdict for:\n{src}");
        let out = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&prog);
            cmd.args(["-o", "/tmp/sal-shape.out"]);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("shape"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        let err = String::from_utf8_lossy(&out.stderr);
        let self_bad = err.contains("do not match")
            || err.contains("matmul element")
            || err.contains("rank >= 2")
            || err.contains("volume mismatch")
            || err.contains("axis out of range");
        assert_eq!(self_bad, *bad, "selfhost verdict for:\n{src}\n{err}");
    }
}

/// Programs both compilers build; the selfhost binary must behave like the
/// bootstrap one (exit code and stdout).
const RUN_PARITY: &[&str] = &[
    "corpus/bytes_if_phi.sal",
    "corpus/effects.sal",
    "corpus/early_return.sal",
    "corpus/forward_fused.sal",
    "corpus/moves.sal",
    "examples/forward.sal",
    "examples/gpu_roundtrip.sal",
    "examples/import_alias.sal",
    "examples/import_alias_left.sal",
    "examples/std_io_smoke.sal",
    "examples/cpu_matmul.sal",
    "corpus/drop_ro_params.sal",
    "corpus/drop_flags.sal",
    "corpus/drop_list_struct.sal",
    "corpus/drop_temporaries.sal",
    "corpus/enum_match.sal",
    "corpus/param_modes.sal",
    "corpus/places.sal",
];

#[test]
fn selfhost_binaries_behave_like_bootstrap_binaries() {
    let stage1 = compile_selfhost(false);
    let dir = std::env::temp_dir().join(format!("sal-runpar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for rel in RUN_PARITY {
        let src = root().join(rel);
        let stem = src.file_stem().unwrap().to_string_lossy().to_string();
        let boot_bin = dir.join(format!("{stem}.boot"));
        let self_bin = dir.join(format!("{stem}.self"));
        let b = Command::new(env!("CARGO_BIN_EXE_sal"))
            .arg("build")
            .arg(&src)
            .arg("-o")
            .arg(&boot_bin)
            .current_dir(root())
            .output()
            .expect("bootstrap build");
        assert!(
            b.status.success(),
            "bootstrap build {rel}: {}",
            String::from_utf8_lossy(&b.stderr)
        );
        let s = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&src);
            cmd.arg("-o");
            cmd.arg(&self_bin);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("runpar"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        assert!(
            s.status.success(),
            "selfhost build {rel}: {}",
            String::from_utf8_lossy(&s.stderr)
        );
        let rb = Command::new(&boot_bin).output().expect("run boot");
        let rs = Command::new(&self_bin).output().expect("run self");
        assert_eq!(rb.status.code(), rs.status.code(), "exit code of {rel}");
        assert_eq!(rb.stdout, rs.stdout, "stdout of {rel}");
    }
}

/// The hand-written conformance corpus of `tests/lang` (see `tests/lang.rs`)
/// must also hold for the selfhost compiler: same exit code and stdout for
/// valid programs, a rejection for `# error:` programs. `# xfail:` files are
/// skipped (they document gaps of the bootstrap).
#[test]
fn selfhost_meets_language_conformance() {
    let stage1 = compile_selfhost(false);
    let dir = root().join("tests/lang");
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).expect("tests/lang").flatten() {
            let path = e.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|n| n != "mods" && n != "target")
                {
                    collect(&path, out);
                }
            } else if path.extension().is_some_and(|x| x == "sal") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    collect(&dir, &mut files);
    files.sort();
    let mut failures = Vec::new();
    for path in files {
        let src = std::fs::read_to_string(&path).expect("read");
        let header: Vec<&str> = src.lines().take_while(|l| l.starts_with('#')).collect();
        let field = |key: &str| -> Option<String> {
            header.iter().find_map(|l| {
                l.trim_start_matches('#')
                    .trim()
                    .strip_prefix(key)
                    .map(|v| v.trim().to_string())
            })
        };
        if field("xfail:").is_some() || field("selfhost-xfail:").is_some() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let out = std::env::temp_dir().join(format!("sal-lang-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_file(&out);
        let built = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&path);
            cmd.arg("-o");
            cmd.arg(&out);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("lang"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        if field("error:").is_some() {
            if built.status.success() && out.is_file() {
                failures.push(format!(
                    "{name}: selfhost compiled a program that must be rejected"
                ));
            }
            continue;
        }
        if !built.status.success() || !out.is_file() {
            failures.push(format!(
                "{name}: selfhost failed to build:\n{}",
                String::from_utf8_lossy(&built.stderr)
            ));
            continue;
        }
        let run = Command::new(&out).output().expect("run");
        let _ = std::fs::remove_file(&out);
        let want: i32 = field("exit:")
            .expect("exit header")
            .parse()
            .expect("exit code");
        let got = run.status.code().unwrap_or(-1);
        if got != want & 0xff {
            failures.push(format!("{name}: exit {got}, expected {}", want & 0xff));
        }
        if let Some(text) = field("stdout:") {
            let got = String::from_utf8_lossy(&run.stdout);
            if got.trim_end() != text {
                failures.push(format!("{name}: stdout {got:?}, expected {text:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Every program under `examples/`, `corpus/`, `tests/fixtures/` and `benches/`
/// that the bootstrap builds must also build with the selfhost compiler and
/// behave the same (exit code and stdout). Programs that print timings are
/// excluded; programs the bootstrap rejects are skipped.
#[test]
fn selfhost_matches_bootstrap_on_every_repo_program() {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "sal") {
                out.push(p);
            }
        }
    }
    let stage1 = compile_selfhost(false);
    let mut files = Vec::new();
    for d in [
        "examples",
        "corpus",
        "tests/fixtures",
        "tests/sal",
        "benches",
        "std",
        "packages",
    ] {
        collect(&root().join(d), &mut files);
    }
    files.sort();
    let dir = std::env::temp_dir().join(format!("sal-repopar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut compared = 0;
    let mut failures = Vec::new();
    for src in files {
        let rel = src.strip_prefix(root()).unwrap().display().to_string();
        let stem = rel.replace(['/', '.'], "_");
        if stem.contains("tiny_prefill")
            || stem.contains("tiny_decode")
            || rel.contains("/support/")
        {
            continue;
        }
        let boot_bin = dir.join(format!("{stem}.boot"));
        let self_bin = dir.join(format!("{stem}.self"));
        let b = Command::new(env!("CARGO_BIN_EXE_sal"))
            .arg("build")
            .arg(&src)
            .arg("-o")
            .arg(&boot_bin)
            .current_dir(root())
            .output()
            .expect("bootstrap build");
        if !b.status.success() {
            continue;
        }
        let s = {
            let mut cmd = Command::new(&stage1);
            cmd.arg(&src);
            cmd.arg("-o");
            cmd.arg(&self_bin);
            cmd.env("SAL_SELFHOST_CACHE", unique_cache_dir("repopar"));
            cmd.current_dir(root());
            stage1_output(cmd)
        };
        if !s.status.success() {
            failures.push(format!(
                "{rel}: selfhost build failed: {}",
                String::from_utf8_lossy(&s.stderr)
                    .lines()
                    .next()
                    .unwrap_or("")
            ));
            continue;
        }
        let run = |bin: &Path| {
            Command::new("timeout")
                .args(["10"])
                .arg(bin)
                .stdin(std::process::Stdio::null())
                .output()
                .expect("run")
        };
        let rb = run(&boot_bin);
        let rs = run(&self_bin);
        compared += 1;
        if rb.status.code() != rs.status.code() || rb.stdout != rs.stdout {
            failures.push(format!(
                "{rel}: boot={:?} self={:?}",
                rb.status.code(),
                rs.status.code()
            ));
        }
        let _ = std::fs::remove_file(&boot_bin);
        let _ = std::fs::remove_file(&self_bin);
    }
    assert!(compared >= 40, "only {compared} programs compared");
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// The `W_DEPRECATED` warning lines of a compilation, location included (`path:line:column`).
fn deprecation_messages(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| l.starts_with("warning[W_DEPRECATED]: "))
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn selfhost_deprecation_warnings_match_bootstrap() {
    let stage1 = compile_selfhost(false);
    let dir = std::env::temp_dir().join(format!("sal-depwarn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.sal"),
        "@deprecated(\"use two\")\nfn one() -> Int\n    1\n\nfn two() -> Int\n    2\n\n@deprecated(\"use Pt2\")\nstruct Pt\n    x: Int\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.sal"),
        "import \"lib.sal\" as l\n\nfn use_pt(p: l.Pt) -> Int\n    p.x\n\nfn main() -> Int\n    l.one() + l.two()\n",
    )
    .unwrap();
    let file = dir.join("main.sal");
    let boot = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["check"])
        .arg(&file)
        .current_dir(root())
        .output()
        .expect("run sal check");
    let self_out = {
        let mut cmd = Command::new(&stage1);
        cmd.arg(&file);
        cmd.arg("-o");
        cmd.arg(dir.join("out.bin"));
        cmd.env("SAL_SELFHOST_CACHE", dir.join("cache"));
        cmd.env("SAL_PROJECT_ROOT", root());
        cmd.current_dir(root());
        stage1_output(cmd)
    };
    let want = deprecation_messages(&String::from_utf8_lossy(&boot.stderr));
    let got = deprecation_messages(&String::from_utf8_lossy(&self_out.stderr));
    assert!(
        !want.is_empty(),
        "bootstrap printed no W_DEPRECATED warning"
    );
    assert_eq!(
        got, want,
        "selfhost warnings differ from the bootstrap's (location included)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
