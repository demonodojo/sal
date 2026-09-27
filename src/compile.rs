use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ast::{Item, Program};
use crate::device::check_devices;
use crate::diag::{Diagnostic, ErrorCode};
use crate::effects::check_effects;
use crate::fuse::fuse_module;
use crate::incremental::{
    cache_key_with_deps, cache_path, import_graph_digest, is_cache_hit, ll_cache_path,
    load_cached_meta, obj_cache_path, store_cached_meta, CacheMeta, CompileFlags,
};
use crate::infer::infer_program;
use crate::ir::{ir_to_text, lower_program_with_callables};
use crate::llvm::{collect_host_tensors, emit_llvm_with_externs, LlvmOptions};
use crate::modules::{
    callable_fn_names, infer_module, resolve_module_graph, LoadedModule, ModuleGraph,
};
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
    if let Some(path) = source_path {
        return compile_file_with_imports(program, path, opts);
    }

    let infer_out = infer_program(program)?;
    check_effects(program)?;
    check_ownership(program)?;
    check_devices(program)?;

    let typed = TypedProgram::from_program(program.clone(), infer_out.expr_types_by_fn);

    let flags = CompileFlags {
        release: opts.release,
        instrument: opts.instrument,
        device: opts.device.clone(),
        emit_entry_main: true,
    };
    let key = cache_key_with_deps(program, &typed, &flags, &[]);
    let fn_names: HashMap<String, ()> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some((f.name.clone(), ())),
            _ => None,
        })
        .collect();
    let mut artifacts = compile_single_module(
        program,
        &typed,
        &key,
        &fn_names,
        &HashMap::new(),
        opts,
        true,
    )?;
    if !opts.skip_link {
        require_device_toolchain(&opts.device)?;
        let obj = obj_cache_path(&opts.project_root, &key);
        artifacts.binary = Some(link_binary_multi(&opts.project_root, &[obj], opts)?);
    }
    Ok(artifacts)
}

fn compile_file_with_imports(
    program: &Program,
    path: &Path,
    opts: &CompileOptions,
) -> Result<CompileArtifacts, Vec<Diagnostic>> {
    let graph = resolve_module_graph(path, &opts.project_root)?;
    for m in &graph.order {
        infer_module(m, &graph)?;
        check_effects(&m.program)?;
        check_ownership(&m.program)?;
        check_devices(&m.program)?;
    }

    let root = graph
        .order
        .last()
        .ok_or_else(|| vec![internal_diag("empty module graph".into())])?;

    let infer_out = infer_module(root, &graph)?;
    let typed = TypedProgram::from_program(program.clone(), infer_out.expr_types_by_fn);

    let base_flags = CompileFlags {
        release: opts.release,
        instrument: opts.instrument,
        device: opts.device.clone(),
        emit_entry_main: false,
    };

    let mut all_objs = Vec::new();
    let mut all_cache_hit = true;
    let mut root_artifacts: Option<CompileArtifacts> = None;

    for m in &graph.order {
        let infer_m = infer_module(m, &graph)?;
        let typed_m = TypedProgram::from_program(m.program.clone(), infer_m.expr_types_by_fn);
        let deps_m = import_graph_digest(&m.program, &m.path, &graph.project_root);
        let is_root = fs::canonicalize(&m.path).unwrap_or_else(|_| m.path.clone())
            == fs::canonicalize(&root.path).unwrap_or_else(|_| root.path.clone());
        let flags_m = CompileFlags {
            emit_entry_main: is_root,
            ..base_flags.clone()
        };
        let key_m = cache_key_with_deps(&m.program, &typed_m, &flags_m, &deps_m);
        let callables = callable_fn_names(m, &graph);
        let externs = extern_fn_arity(m, &graph, &callables);
        let artifacts = compile_single_module(
            &m.program,
            &typed_m,
            &key_m,
            &callables,
            &externs,
            opts,
            is_root,
        )?;
        all_cache_hit &= artifacts.cache_hit;
        if is_root {
            root_artifacts = Some(artifacts);
        }
        let obj = obj_cache_path(&opts.project_root, &key_m);
        all_objs.push(obj);
    }

    let root_art = root_artifacts.ok_or_else(|| {
        vec![internal_diag("root module missing from compile loop".into())]
    })?;
    let ir_text = root_art.ir_text;
    let llvm = root_art.llvm;

    let binary = if opts.skip_link {
        None
    } else {
        require_device_toolchain(&opts.device)?;
        Some(link_binary_multi(&opts.project_root, &all_objs, opts)?)
    };

    Ok(CompileArtifacts {
        program: program.clone(),
        typed,
        ir_text,
        llvm,
        binary,
        cache_hit: all_cache_hit,
    })
}

fn extern_fn_arity(
    module: &LoadedModule,
    graph: &ModuleGraph,
    callables: &HashMap<String, ()>,
) -> HashMap<String, usize> {
    let local: HashSet<String> = module
        .program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    let mut out = HashMap::new();
    for name in callables.keys() {
        if local.contains(name) {
            continue;
        }
        for m in &graph.order {
            for item in &m.program.items {
                if let Item::Fn(f) = item {
                    if &f.name == name {
                        out.insert(name.clone(), f.params.len());
                    }
                }
            }
        }
    }
    out
}

fn compile_single_module(
    program: &Program,
    typed: &TypedProgram,
    key: &str,
    callables: &HashMap<String, ()>,
    externs: &HashMap<String, usize>,
    opts: &CompileOptions,
    emit_entry_main: bool,
) -> Result<CompileArtifacts, Vec<Diagnostic>> {
    let meta_file = cache_path(&opts.project_root, key);
    let ll_path = ll_cache_path(&opts.project_root, key);
    let obj_path = obj_cache_path(&opts.project_root, key);
    let cache_hit = is_cache_hit(&meta_file, key);
    let prev_meta = load_cached_meta(&meta_file);

    let mut ir = lower_program_with_callables(program, callables);
    fuse_module(&mut ir);
    let ir_text = ir_to_text(&ir);

    let (llvm, llvm_writes) = if cache_hit && ll_path.is_file() {
        let llvm = fs::read_to_string(&ll_path).map_err(|e| vec![internal_diag(e.to_string())])?;
        let writes = prev_meta.as_ref().map(|m| m.llvm_writes).unwrap_or(1);
        (llvm, writes)
    } else {
        let tensors = collect_host_tensors(program);
        let llvm = emit_llvm_with_externs(
            &ir,
            &LlvmOptions {
                instrument: opts.instrument,
                extern_user_fns: externs.clone(),
                emit_entry_main,
            },
            &tensors,
            externs,
        );
        if let Some(parent) = ll_path.parent() {
            fs::create_dir_all(parent).map_err(|e| vec![internal_diag(e.to_string())])?;
        }
        fs::write(&ll_path, &llvm).map_err(|e| vec![internal_diag(e.to_string())])?;
        let writes = prev_meta.as_ref().map(|m| m.llvm_writes + 1).unwrap_or(1);
        (llvm, writes)
    };

    if !opts.skip_link {
        require_device_toolchain(&opts.device)?;
        compile_obj_from_ll(&ll_path, &obj_path, cache_hit, opts)?;
    }

    let meta = CacheMeta {
        key: key.to_string(),
        llvm_writes,
        ll_path: ll_path.display().to_string(),
        obj_path: obj_path.display().to_string(),
    };
    let _ = store_cached_meta(&meta_file, &meta);

    Ok(CompileArtifacts {
        program: program.clone(),
        typed: typed.clone(),
        ir_text,
        llvm,
        binary: None,
        cache_hit,
    })
}

fn compile_obj_from_ll(
    ll_path: &Path,
    obj_path: &Path,
    cache_hit: bool,
    opts: &CompileOptions,
) -> Result<(), Vec<Diagnostic>> {
    if cache_hit && obj_path.is_file() {
        return Ok(());
    }
    if let Some(parent) = obj_path.parent() {
        fs::create_dir_all(parent).map_err(|e| vec![internal_diag(e.to_string())])?;
    }
    let opt_flag = if opts.release { "-O3" } else { "-O0" };
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
    run_cmd(clang)
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

fn link_binary_multi(
    root: &Path,
    obj_paths: &[PathBuf],
    opts: &CompileOptions,
) -> Result<PathBuf, Vec<Diagnostic>> {
    let out_dir = root.join("target").join("sal-out");
    fs::create_dir_all(&out_dir).map_err(|e| vec![internal_diag(e.to_string())])?;
    let bin = out_dir.join("a.out");
    let opt_flag = if opts.release { "-O3" } else { "-O0" };

    let runtime = runtime_dir(root);
    let mut link = Command::new("clang");
    for obj in obj_paths {
        link.arg(obj);
    }
    link.arg(runtime.join("sal_runtime.c"))
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
