use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ast::Program;
use crate::device::check_devices;
use crate::diag::{Diagnostic, ErrorCode};
use crate::effects::check_effects;
use crate::fuse::fuse_module;
use crate::incremental::{
    cache_key_with_deps, cache_path, import_graph_digest, is_cache_hit, ll_cache_path,
    load_cached_meta, obj_cache_path, store_cached_meta, CacheMeta, CompileFlags,
};
use crate::infer::infer_program;
use crate::ir::{ir_to_text, lower_program};
use crate::llvm::{collect_host_tensors, emit_llvm_with_tensors, LlvmOptions};
use crate::ownership::check_ownership;
use crate::parser::parse;
use crate::typed::TypedProgram;

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub release: bool,
    pub instrument: bool,
    pub device: String,
    pub project_root: PathBuf,
    pub skip_link: bool,
}

#[derive(Debug)]
pub struct CompileArtifacts {
    pub program: Program,
    pub typed: TypedProgram,
    pub ir_text: String,
    pub llvm: String,
    pub binary: Option<PathBuf>,
    pub cache_hit: bool,
}

pub fn compile_source(source: &str, opts: &CompileOptions) -> Result<CompileArtifacts, Vec<Diagnostic>> {
    let program = parse(source).map_err(|d| vec![d])?;
    compile_program(&program, opts)
}

pub fn compile_file(path: &Path, opts: &CompileOptions) -> Result<CompileArtifacts, Vec<Diagnostic>> {
    let source = fs::read_to_string(path).map_err(|e| {
        vec![Diagnostic::new(
            ErrorCode::EInternal,
            e.to_string(),
            Default::default(),
        )]
    })?;
    let program = parse(&source).map_err(|d| vec![d])?;
    compile_program_at(&program, opts, Some(path))
}

pub fn compile_program(program: &Program, opts: &CompileOptions) -> Result<CompileArtifacts, Vec<Diagnostic>> {
    compile_program_at(program, opts, None)
}

fn compile_program_at(
    program: &Program,
    opts: &CompileOptions,
    source_path: Option<&Path>,
) -> Result<CompileArtifacts, Vec<Diagnostic>> {
    let infer_out = infer_program(program)?;
    check_effects(program)?;
    check_ownership(program)?;
    check_devices(program)?;

    let typed = TypedProgram::from_program(program.clone(), infer_out.expr_types_by_fn);

    let flags = CompileFlags {
        release: opts.release,
        instrument: opts.instrument,
        device: opts.device.clone(),
    };
    let deps = source_path
        .map(|p| import_graph_digest(program, p, &opts.project_root))
        .unwrap_or_default();
    let key = cache_key_with_deps(program, &typed, &flags, &deps);
    let meta_file = cache_path(&opts.project_root, &key);
    let ll_path = ll_cache_path(&opts.project_root, &key);
    let obj_path = obj_cache_path(&opts.project_root, &key);
    let cache_hit = is_cache_hit(&meta_file, &key);
    let prev_meta = load_cached_meta(&meta_file);

    let mut ir = lower_program(program);
    fuse_module(&mut ir);
    let ir_text = ir_to_text(&ir);

    let (llvm, llvm_writes) = if cache_hit && ll_path.is_file() {
        let llvm = fs::read_to_string(&ll_path).map_err(|e| vec![internal_diag(e.to_string())])?;
        let writes = prev_meta.as_ref().map(|m| m.llvm_writes).unwrap_or(1);
        (llvm, writes)
    } else {
        let tensors = collect_host_tensors(program);
        let llvm = emit_llvm_with_tensors(
            &ir,
            &LlvmOptions {
                instrument: opts.instrument,
            },
            &tensors,
        );
        if let Some(parent) = ll_path.parent() {
            fs::create_dir_all(parent).map_err(|e| vec![internal_diag(e.to_string())])?;
        }
        fs::write(&ll_path, &llvm).map_err(|e| vec![internal_diag(e.to_string())])?;
        let writes = prev_meta.as_ref().map(|m| m.llvm_writes + 1).unwrap_or(1);
        (llvm, writes)
    };

    let binary = if opts.skip_link {
        None
    } else {
        require_device_toolchain(&opts.device)?;
        Some(link_binary(
            &opts.project_root,
            &ll_path,
            &obj_path,
            &llvm,
            cache_hit,
            opts,
        )?)
    };

    let meta = CacheMeta {
        key: key.clone(),
        llvm_writes,
        ll_path: ll_path.display().to_string(),
        obj_path: obj_path.display().to_string(),
    };
    let _ = store_cached_meta(&meta_file, &meta);

    Ok(CompileArtifacts {
        program: program.clone(),
        typed,
        ir_text,
        llvm,
        binary,
        cache_hit,
    })
}

/// Whether a physical device / native toolchain is present (no network).
pub fn device_toolchain_present(device: &str) -> bool {
    match device {
        "cpu" => true,
        "gpu" => {
            std::env::var_os("SAL_HAVE_GPU").is_some()
                || Path::new("/dev/nvidia0").exists()
                || command_exists("nvcc")
        }
        "tpu" => std::env::var_os("SAL_HAVE_TPU").is_some(),
        _ => false,
    }
}

fn require_device_toolchain(device: &str) -> Result<(), Vec<Diagnostic>> {
    if device_toolchain_present(device) {
        return Ok(());
    }
    Err(vec![Diagnostic::new(
        ErrorCode::EDeviceMissing,
        format!("device '{device}' or its toolchain is not available"),
        Default::default(),
    )])
}

fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn runtime_dir(root: &Path) -> PathBuf {
    let candidate = root.join("runtime");
    if candidate.join("sal_runtime.c").exists() {
        candidate
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime")
    }
}

fn link_binary(
    root: &Path,
    ll_path: &Path,
    obj_path: &Path,
    llvm: &str,
    cache_hit: bool,
    opts: &CompileOptions,
) -> Result<PathBuf, Vec<Diagnostic>> {
    let out_dir = root.join("target").join("sal-out");
    fs::create_dir_all(&out_dir).map_err(|e| vec![internal_diag(e.to_string())])?;
    let bin = out_dir.join("a.out");
    let opt_flag = if opts.release { "-O3" } else { "-O0" };

    // Ensure .ll on disk (may already exist from cache).
    if !ll_path.is_file() {
        if let Some(parent) = ll_path.parent() {
            fs::create_dir_all(parent).map_err(|e| vec![internal_diag(e.to_string())])?;
        }
        fs::write(ll_path, llvm).map_err(|e| vec![internal_diag(e.to_string())])?;
    }

    if !(cache_hit && obj_path.is_file()) {
        if let Some(parent) = obj_path.parent() {
            fs::create_dir_all(parent).map_err(|e| vec![internal_diag(e.to_string())])?;
        }
        let mut clang = Command::new("clang");
        clang
            .arg("-x")
            .arg("ir")
            .arg(ll_path)
            .arg("-c")
            .arg("-o")
            .arg(obj_path);
        clang.arg(opt_flag);
        if opts.instrument || !opts.release {
            clang.arg("-g");
        }
        run_cmd(clang)?;
    }

    let runtime = runtime_dir(root);
    let mut link = Command::new("clang");
    link.arg(obj_path)
        .arg(runtime.join("sal_runtime.c"))
        .arg(runtime.join("kernels.c"))
        .arg(runtime.join("instrument.c"));
    if opts.instrument {
        link.arg("-DSAL_INSTRUMENT=1");
    }
    link.arg("-o").arg(&bin).arg(opt_flag).arg("-lm");
    run_cmd(link)?;
    Ok(bin)
}

fn run_cmd(mut cmd: Command) -> Result<(), Vec<Diagnostic>> {
    let out = cmd.output().map_err(|e| vec![internal_diag(e.to_string())])?;
    if !out.status.success() {
        return Err(vec![internal_diag(format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        ))]);
    }
    Ok(())
}

fn internal_diag(msg: String) -> Diagnostic {
    Diagnostic::new(ErrorCode::EInternal, msg, Default::default())
}
