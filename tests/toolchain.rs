use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sal_compiler::compile::{compile_file, device_toolchain_present, CompileOptions};
use sal_compiler::modules::resolve_import_path_with_manifest;
use sal_compiler::packages::{fetch_packages, read_lock, std_home};
use sal_compiler::incremental::{
    cache_key, cache_key_with_deps, cache_path, file_mtime, import_graph_digest, is_cache_hit,
    ll_cache_path, load_cached_meta, obj_cache_path, CompileFlags,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sal_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sal"))
}

fn isolated_root() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Cache under temp; runtime resolved via CARGO_MANIFEST_DIR fallback in compile.rs.
    tmp
}

#[test]
fn incremental_cache_reuses_object_and_skips_ll_rewrite() {
    let tmp = isolated_root();
    let project = tmp.path().to_path_buf();
    let file = root().join("examples/hello.sal");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: project.clone(),
        skip_link: false,
    };

    let a = compile_file(&file, &opts).expect("first build");
    assert!(!a.cache_hit);
    assert!(a.binary.is_some());

    let flags = CompileFlags {
        toolchain: opts.toolchain.identity(),
        release: false,
        instrument: false,
        device: "cpu".into(),
        emit_entry_main: true,
    };
    let key = cache_key(&a.program, &a.typed, &flags);
    let meta_path = cache_path(&project, &key);
    let ll = ll_cache_path(&project, &key);
    let obj = obj_cache_path(&project, &key);

    assert!(ll.is_file(), "cached .ll must exist");
    assert!(obj.is_file(), "cached object must exist");
    let meta1 = load_cached_meta(&meta_path).expect("meta after first");
    assert_eq!(meta1.llvm_writes, 1);
    let mtime1 = file_mtime(&ll).expect("ll mtime");

    let b = compile_file(&file, &opts).expect("second build");
    assert!(b.cache_hit, "second build must hit cache");
    assert!(obj.is_file());

    let meta2 = load_cached_meta(&meta_path).expect("meta after second");
    assert_eq!(
        meta2.llvm_writes, 1,
        "second build must not regenerate .ll (llvm_writes stays 1)"
    );
    let mtime2 = file_mtime(&ll).expect("ll mtime after second");
    assert_eq!(mtime1, mtime2, "second build must not rewrite .ll");
}

#[test]
fn check_accepts_gpu_without_physical_device() {
    // Ensure we are not forcing a fake GPU for this assertion.
    assert!(
        !device_toolchain_present("gpu") || std::env::var_os("SAL_HAVE_GPU").is_none(),
        "test assumes no forced GPU; unset SAL_HAVE_GPU if set"
    );

    // hello.sal is enough: check must not require a physical GPU for --device gpu.
    // (gpu_roundtrip.sal may still fail elsewhere in the pipeline while other agents land.)
    let out = Command::new(sal_bin())
        .current_dir(root())
        .args([
            "check",
            "--device",
            "gpu",
            root().join("examples/hello.sal").to_str().unwrap(),
        ])
        .output()
        .expect("sal check");
    assert!(
        out.status.success(),
        "sal check --device gpu should accept program without GPU: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn build_gpu_fails_with_device_missing() {
    if device_toolchain_present("gpu") {
        // Real GPU present — cannot assert E_DEVICE_MISSING.
        return;
    }

    let out = Command::new(sal_bin())
        .current_dir(root())
        .env_remove("SAL_HAVE_GPU")
        .args([
            "build",
            "--device",
            "gpu",
            "--error-format",
            "json",
            root().join("examples/hello.sal").to_str().unwrap(),
        ])
        .output()
        .expect("sal build");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("E_DEVICE_MISSING") || err.contains("EDeviceMissing"),
        "expected E_DEVICE_MISSING in json diagnostics, got: {err}"
    );
}

#[test]
fn build_tpu_fails_with_device_missing() {
    if device_toolchain_present("tpu") {
        return;
    }

    let out = Command::new(sal_bin())
        .current_dir(root())
        .env_remove("SAL_HAVE_TPU")
        .args([
            "build",
            "--device",
            "tpu",
            "--error-format",
            "json",
            root().join("examples/hello.sal").to_str().unwrap(),
        ])
        .output()
        .expect("sal build");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("E_DEVICE_MISSING"),
        "expected E_DEVICE_MISSING, got: {err}"
    );
}

#[test]
fn error_format_json_on_check_parse_error() {
    let tmp = tempfile::tempdir().expect("tmp");
    let bad = tmp.path().join("bad.sal");
    fs::write(&bad, "fn main( -> Int\n    0\n").expect("write");

    let out = Command::new(sal_bin())
        .current_dir(root())
        .args([
            "check",
            "--error-format",
            "json",
            bad.to_str().unwrap(),
        ])
        .output()
        .expect("sal check");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    // Diagnostic JSON should include a code field.
    assert!(
        err.contains("\"code\"") || err.contains("E_PARSE"),
        "expected JSON diagnostic, got: {err}"
    );
}

#[test]
fn emit_ast_and_compile_from_ast() {
    let hello = root().join("examples/hello.sal");
    let tmp = tempfile::tempdir().expect("tmp");
    let ast_json = tmp.path().join("hello.ast.json");

    let emit = Command::new(sal_bin())
        .current_dir(root())
        .args(["emit", "ast", hello.to_str().unwrap()])
        .output()
        .expect("emit");
    assert!(emit.status.success(), "{}", String::from_utf8_lossy(&emit.stderr));
    fs::write(&ast_json, &emit.stdout).expect("write ast");

    // Round-trip: Program deserializes (compile --ast).
    let compile = Command::new(sal_bin())
        .current_dir(root())
        .args([
            "compile",
            "--ast",
            ast_json.to_str().unwrap(),
        ])
        .output()
        .expect("compile --ast");
    assert!(
        compile.status.success(),
        "sal compile --ast failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
}

#[test]
fn skip_link_second_build_marks_cache_hit() {
    let tmp = isolated_root();
    let file = root().join("examples/hello.sal");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: tmp.path().to_path_buf(),
        skip_link: true,
    };
    let _a = compile_file(&file, &opts).expect("first");
    let b = compile_file(&file, &opts).expect("second");
    assert!(b.cache_hit);
}

#[test]
fn import_change_invalidates_importer_cache() {
    let tmp = isolated_root();
    let project = tmp.path().to_path_buf();
    let b_path = project.join("b.sal");
    let a_path = project.join("a.sal");

    fs::write(
        &b_path,
        "fn helper() -> Int\n    1\n",
    )
    .expect("write b");
    fs::write(
        &a_path,
        "import \"./b.sal\"\n\nfn main() -> Int\n    42\n",
    )
    .expect("write a");

    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: project.clone(),
        skip_link: false,
    };
    let flags = CompileFlags {
        toolchain: opts.toolchain.identity(),
        release: false,
        instrument: false,
        device: "cpu".into(),
        emit_entry_main: true,
    };

    let first = compile_file(&a_path, &opts).expect("first build of A");
    assert!(!first.cache_hit);
    assert!(first.binary.is_some());

    let deps1 = import_graph_digest(&first.program, &a_path, &project);
    assert!(
        !deps1.is_empty(),
        "A imports B so the import digest must be non-empty"
    );
    let key1 = cache_key_with_deps(&first.program, &first.typed, &flags, &deps1);
    let obj1 = obj_cache_path(&project, &key1);
    assert!(obj1.is_file(), "first build must store object for A");

    // Change only B — A's source is unchanged, but its cache key must move.
    fs::write(
        &b_path,
        "fn helper() -> Int\n    2\n",
    )
    .expect("rewrite b");

    let second = compile_file(&a_path, &opts).expect("second build after B change");
    assert!(
        !second.cache_hit,
        "changing imported B must miss A's object cache"
    );
    let deps2 = import_graph_digest(&second.program, &a_path, &project);
    let key2 = cache_key_with_deps(&second.program, &second.typed, &flags, &deps2);
    assert_ne!(key1, key2, "importer cache key must change when B's typed AST changes");
    let obj2 = obj_cache_path(&project, &key2);
    assert!(obj2.is_file(), "miss path must write a new object");
    assert_ne!(obj1, obj2);

    // Unchanged B → hit under the post-invalidation key.
    let third = compile_file(&a_path, &opts).expect("third build with B unchanged");
    assert!(third.cache_hit, "unchanged import graph must be a cache hit");
    assert!(obj2.is_file());
    // Stale object under key1 must not be the lookup target after B changed.
    assert!(!is_cache_hit(&cache_path(&project, &key1), &key2));
}

#[test]
fn import_as_two_modules_run() {
    let tmp = isolated_root();
    let project = tmp.path().to_path_buf();
    let left = project.join("left.sal");
    let right = project.join("right.sal");
    let main = project.join("main.sal");
    fs::write(&left, "fn helper() -> Int\n    10\n").expect("write left");
    fs::write(&right, "fn helper() -> Int\n    32\n").expect("write right");
    fs::write(
        &main,
        "import \"./left.sal\" as L\nimport \"./right.sal\" as R\n\nfn main() -> Int\n    _ = L.helper()\n    0\n",
    )
    .expect("write main");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: project,
        skip_link: false,
    };
    let art = compile_file(&main, &opts).expect("compile import-as graph");
    let bin = art.binary.expect("binary");
    let out = std::process::Command::new(&bin)
        .output()
        .expect("run");
    assert!(out.status.success());
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn import_two_modules_run() {
    let tmp = isolated_root();
    let project = tmp.path().to_path_buf();
    let b_path = project.join("b.sal");
    let a_path = project.join("a.sal");
    fs::write(&b_path, "fn helper() -> Int\n    42\n").expect("write b");
    fs::write(
        &a_path,
        "import \"./b.sal\"\n\nfn main() -> Int\n    helper()\n",
    )
    .expect("write a");
    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: project,
        skip_link: false,
    };
    let art = compile_file(&a_path, &opts).expect("compile import graph");
    let bin = art.binary.expect("binary");
    let out = std::process::Command::new(&bin)
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(42));
}

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_git_dep_repo(repo_dir: &Path) -> String {
    fs::write(
        repo_dir.join("pkg.sal"),
        "fn dep_value() -> Int\n    7\n",
    )
    .expect("write pkg");
    run_git(repo_dir, &["init"]);
    run_git(repo_dir, &["config", "user.email", "t@test"]);
    run_git(repo_dir, &["config", "user.name", "t"]);
    run_git(repo_dir, &["add", "."]);
    run_git(repo_dir, &["commit", "-m", "v1"]);
    run_git(repo_dir, &["tag", "v0.1.0"]);
    fs::write(
        repo_dir.join("pkg.sal"),
        "fn dep_value() -> Int\n    9\n",
    )
    .expect("write pkg2");
    run_git(repo_dir, &["add", "."]);
    run_git(repo_dir, &["commit", "-m", "v2"]);
    run_git(repo_dir, &["tag", "v0.2.0"]);
    format!(
        "file://{}",
        repo_dir.canonicalize().expect("canonical").display()
    )
}

struct PackageConsumer {
    project: PathBuf,
    _repo: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

fn setup_package_consumer() -> PackageConsumer {
    let repo = tempfile::tempdir().expect("repo");
    let url = init_git_dep_repo(repo.path());
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
    std::env::set_var("SAL_PACKAGES", cache.path());
    fetch_packages(&project_root, false).expect("fetch");
    PackageConsumer {
        project: project_root,
        _repo: repo,
        _cache: cache,
    }
}

#[test]
fn fetch_writes_lock_and_check_resolves_git_package() {
    let fx = setup_package_consumer();
    let lock = read_lock(&fx.project);
    assert_eq!(lock.package.len(), 1);
    assert_eq!(lock.package[0].name, "dep");
    assert_eq!(lock.package[0].version, "0.2.0");

    let opts = CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: fx.project.clone(),
        skip_link: false,
    };
    let art = compile_file(&fx.project.join("main.sal"), &opts).expect("build with package");
    let bin = art.binary.expect("binary");
    let run = Command::new(&bin).output().expect("run");
    assert_eq!(run.status.code(), Some(9));
}

#[test]
fn fetch_fails_when_no_matching_tag() {
    let repo = tempfile::tempdir().expect("repo");
    let url = init_git_dep_repo(repo.path());
    let cache = tempfile::tempdir().expect("cache");
    let project = tempfile::tempdir().expect("project");
    fs::write(
        project.path().join("Sal.toml"),
        format!(
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n\n[dependencies]\ndep = {{ git = \"{url}\", version = \"99.0\" }}\n"
        ),
    )
    .expect("manifest");
    std::env::set_var("SAL_PACKAGES", cache.path());
    let err = fetch_packages(project.path(), false).unwrap_err();
    assert!(err.contains("no tag matching"), "{err}");
}

#[test]
fn import_not_found_when_lock_package_missing_from_cache() {
    let fx = setup_package_consumer();
    fs::remove_dir_all(
        sal_compiler::packages::package_cache_path("dep", "0.2.0"),
    )
    .expect("rm cache");
    let main = fx.project.join("main.sal");
    let hit = resolve_import_path_with_manifest("dep/pkg", &main, &fx.project);
    assert!(hit.is_none());
}

#[test]
fn std_import_resolves_via_sal_home_outside_compiler_tree() {
    let project = tempfile::tempdir().expect("project");
    let project_root = project.path().to_path_buf();
    fs::write(
        project_root.join("main.sal"),
        "import \"std/prelude.sal\"\n\nfn main() -> Int\n    0\n",
    )
    .expect("main");
    let home = std_home().expect("compiler std");
    let main = project_root.join("main.sal");
    let hit = resolve_import_path_with_manifest("std/prelude.sal", &main, &project_root);
    assert_eq!(hit, Some(home.join("std/prelude.sal")));
}

#[test]
fn sal_selfhost_builds_stage2() {
    let tmp = isolated_root();
    let project = tmp.path().to_path_buf();
    let stage2 = project.join("stage2-bin");
    let status = Command::new(sal_bin())
        .args([
            "selfhost",
            "--out",
            stage2.to_str().expect("utf8 path"),
        ])
        .current_dir(root())
        .status()
        .expect("sal selfhost");
    assert!(status.success(), "sal selfhost failed");
    assert!(stage2.is_file(), "stage2 binary missing");
    let nm = Command::new("nm").arg(&stage2).output().expect("nm");
    let txt = String::from_utf8_lossy(&nm.stdout);
    assert!(
        !txt.contains("sal_emit_ir"),
        "stage2 must not link legacy sal_emit_ir"
    );
}
