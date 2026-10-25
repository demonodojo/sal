use sal_compiler::{
    compile_file,
    target::{Target, Toolchain},
    CompileOptions,
};

fn options(root: &std::path::Path, target: Target) -> CompileOptions {
    CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root.into(),
        skip_link: true,
        output: None,
        toolchain: Toolchain {
            target,
            cc: "missing-test-cc".into(),
            linker: "missing-test-ld".into(),
            sdk: root.join("sdk"),
        },
    }
}

#[test]
fn cache_separates_target_sdk_and_tool_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("main.sal");
    std::fs::write(&file, "fn main() -> Int\n    42\n").unwrap();
    std::fs::create_dir(root.join("sdk")).unwrap();
    std::fs::write(root.join("sdk/manifest.txt"), "sdk=1").unwrap();
    let linux = compile_file(&file, &options(root, Target::Linux)).unwrap();
    assert!(linux.llvm.contains("x86_64-unknown-linux-gnu"));
    let mut opts = options(root, Target::Soso);
    let a = compile_file(&file, &opts).unwrap();
    assert!(!a.cache_hit);
    assert!(a.llvm.contains("x86_64-unknown-none-elf"));
    assert!(compile_file(&file, &opts).unwrap().cache_hit);
    std::fs::write(root.join("sdk/manifest.txt"), "sdk=2").unwrap();
    assert!(!compile_file(&file, &opts).unwrap().cache_hit);
    opts.toolchain.cc = "different-test-cc".into();
    assert!(!compile_file(&file, &opts).unwrap().cache_hit);
    opts.toolchain.cc = root.join("fake-tool");
    std::fs::write(&opts.toolchain.cc, "tool-v1").unwrap();
    assert!(!compile_file(&file, &opts).unwrap().cache_hit);
    assert!(compile_file(&file, &opts).unwrap().cache_hit);
    std::fs::write(&opts.toolchain.cc, "tool-v2").unwrap();
    assert!(!compile_file(&file, &opts).unwrap().cache_hit);
}

#[test]
fn soso_rejects_instrumentation_and_non_cpu_source_in_imports() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("main.sal");
    std::fs::write(&file, "fn main() -> Int\n    42\n").unwrap();
    let mut opts = options(root, Target::Soso);
    opts.instrument = true;
    assert!(compile_file(&file, &opts).unwrap_err()[0]
        .message
        .contains("sin instrumentación"));
    opts.instrument = false;
    opts.device = "gpu".into();
    assert!(compile_file(&file, &opts).is_err());
    opts.device = "cpu".into();
    std::fs::write(
        root.join("other.sal"),
        "fn other() -> Int\n    on gpu\n        7\n",
    )
    .unwrap();
    std::fs::write(&file, "import other\nfn main() -> Int\n    other()\n").unwrap();
    assert!(compile_file(&file, &opts).is_err());
}

#[test]
fn utf8_string_at_token_boundary_does_not_panic() {
    let p =
        sal_compiler::parser::parse("fn main() -> Int ! io\n    print_str(\"ñ€🙂\")\n").unwrap();
    assert!(!p.items.is_empty());
    assert!(sal_compiler::parser::parse("fn ñ() -> Int\n    0\n").is_err());
}

#[test]
fn soso_syscall_and_bytes_get_lower_to_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("main.sal");
    std::fs::write(
        &file,
        r#"fn main() -> Int ! io, alloc
    b = bytes_new(8)
    x = bytes_get_u32(b, 0)
    syscall(1, 0, 0, 0, 0, 0, 0) + x
"#,
    )
    .unwrap();
    std::fs::create_dir(root.join("sdk")).unwrap();
    std::fs::write(root.join("sdk/manifest.txt"), "sdk=1").unwrap();
    let art = compile_file(&file, &options(root, Target::Soso)).expect("compile");
    assert!(art.llvm.contains("@sal_os_syscall"));
    assert!(art.llvm.contains("sal_panic"));
    assert!(art.llvm.contains("load i32"));
}

#[test]
fn bytes_set_inside_if_emits_valid_llvm() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("main.sal");
    std::fs::write(&file, include_str!("../corpus/bytes_if_phi.sal")).unwrap();
    std::fs::create_dir(root.join("sdk")).unwrap();
    std::fs::write(root.join("sdk/manifest.txt"), "sdk=1").unwrap();
    let art = compile_file(&file, &options(root, Target::Soso)).expect("compile");
    assert!(art.llvm.contains("@sal_bytes_check"));
}
