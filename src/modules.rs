use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{EnumDef, FnDef, FrameDef, Item, Program, StructDef};
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::parser::parse;
use crate::span::Span;

/// Exported signatures visible across modules.
#[derive(Default, Clone)]
pub struct ModuleExports {
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    pub frames: HashMap<String, FrameDef>,
    pub fns: HashMap<String, FnDef>,
}

#[derive(Clone)]
pub struct LoadedModule {
    pub path: PathBuf,
    pub program: Program,
}

#[derive(Clone)]
pub struct ModuleGraph {
    pub root: PathBuf,
    pub project_root: PathBuf,
    /// Dependency order: imports before importers; root is last.
    pub order: Vec<LoadedModule>,
}

/// Extra search bases from `[dependencies]` in `Sal.toml` (`path = "..."`).
pub fn dependency_search_bases(project_root: &Path) -> Vec<PathBuf> {
    let manifest = project_root.join("Sal.toml");
    let Ok(raw) = fs::read_to_string(&manifest) else {
        return Vec::new();
    };
    let mut bases = Vec::new();
    let mut in_deps = false;
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t == "[dependencies]";
            continue;
        }
        if !in_deps {
            continue;
        }
        if let Some(i) = t.find("path = \"") {
            let rest = &t[i + 7..];
            if let Some(j) = rest.find('"') {
                let path = &rest[..j];
                if !path.is_empty() {
                    bases.push(project_root.join(path));
                }
            }
        }
    }
    bases
}

pub fn resolve_import_path_with_manifest(
    import_path: &str,
    source_file: &Path,
    project_root: &Path,
) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = source_file.parent() {
        push_candidates(&mut candidates, dir, import_path);
    }
    push_candidates(&mut candidates, project_root, import_path);
    for base in dependency_search_bases(project_root) {
        push_candidates(&mut candidates, &base, import_path);
        if let Some((dep, rest)) = import_path.split_once('/') {
            if base.file_name().and_then(|s| s.to_str()) == Some(dep) {
                push_candidates(&mut candidates, &base, rest);
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn push_candidates(out: &mut Vec<PathBuf>, base: &Path, import_path: &str) {
    out.push(base.join(import_path));
    if !import_path.ends_with(".sal") {
        out.push(base.join(format!("{import_path}.sal")));
    }
}

fn path_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn resolve_module_graph(root: &Path, project_root: &Path) -> Result<ModuleGraph, Vec<Diagnostic>> {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut order: Vec<LoadedModule> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    load_module_recursive(
        &root,
        project_root,
        &mut order,
        &mut visited,
        &mut stack,
        None,
    )?;
    Ok(ModuleGraph {
        root: root.clone(),
        project_root: project_root.to_path_buf(),
        order,
    })
}

fn load_module_recursive(
    path: &Path,
    project_root: &Path,
    order: &mut Vec<LoadedModule>,
    visited: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
    cycle_span: Option<Span>,
) -> Result<(), Vec<Diagnostic>> {
    let key = path_key(path);
    if stack.iter().any(|p| path_key(p) == key) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "import cycle",
            cycle_span.unwrap_or_default(),
        )]);
    }
    if visited.contains(&key) {
        return Ok(());
    }
    stack.push(path.to_path_buf());
    let src = fs::read_to_string(path).map_err(|e| {
        vec![Diagnostic::new(
            ErrorCode::EInternal,
            e.to_string(),
            Default::default(),
        )]
    })?;
    let program = parse(&src).map_err(|d| vec![d])?;

    for item in &program.items {
        let Item::Import(imp) = item else {
            continue;
        };
        let Some(dep_path) =
            resolve_import_path_with_manifest(&imp.path, path, project_root)
        else {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("import not found: {}", imp.path),
                imp.span,
            )]);
        };
        load_module_recursive(
            &dep_path,
            project_root,
            order,
            visited,
            stack,
            Some(imp.span),
        )?;
    }

    stack.pop();
    visited.insert(key);
    order.push(LoadedModule {
        path: path.to_path_buf(),
        program,
    });
    Ok(())
}

pub fn exports_for_module(module: &LoadedModule) -> ModuleExports {
    let mut out = ModuleExports::default();
    for item in &module.program.items {
        match item {
            Item::Struct(s) => {
                out.structs.insert(s.name.clone(), s.clone());
            }
            Item::Enum(e) => {
                out.enums.insert(e.name.clone(), e.clone());
            }
            Item::Fn(f) => {
                out.fns.insert(f.name.clone(), f.clone());
            }
            Item::Frame(f) => {
                out.frames.insert(f.name.clone(), f.clone());
                out.structs
                    .insert(f.name.clone(), crate::infer::frame_as_struct(f));
            }
            _ => {}
        }
    }
    out
}

fn merge_exports(
    into: &mut ModuleExports,
    from: &ModuleExports,
    at: Span,
) -> Result<(), Vec<Diagnostic>> {
    for (name, f) in &from.frames {
        if into.frames.contains_key(name)
            || into.structs.contains_key(name)
            || into.enums.contains_key(name)
            || into.fns.contains_key(name)
        {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{name}` defined in more than one imported module"),
                at,
            )]);
        }
        into.frames.insert(name.clone(), f.clone());
    }
    for (name, s) in &from.structs {
        if into.structs.contains_key(name)
            || into.frames.contains_key(name)
            || into.enums.contains_key(name)
            || into.fns.contains_key(name)
        {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{name}` defined in more than one imported module"),
                at,
            )]);
        }
        into.structs.insert(name.clone(), s.clone());
    }
    for (name, e) in &from.enums {
        if into.structs.contains_key(name)
            || into.enums.contains_key(name)
            || into.fns.contains_key(name)
        {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{name}` defined in more than one imported module"),
                at,
            )]);
        }
        into.enums.insert(name.clone(), e.clone());
    }
    for (name, f) in &from.fns {
        if into.structs.contains_key(name)
            || into.enums.contains_key(name)
            || into.fns.contains_key(name)
        {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{name}` defined in more than one imported module"),
                at,
            )]);
        }
        into.fns.insert(name.clone(), f.clone());
    }
    Ok(())
}

fn transitive_import_modules<'a>(
    module: &'a LoadedModule,
    graph: &'a ModuleGraph,
) -> Result<Vec<&'a LoadedModule>, Vec<Diagnostic>> {
    let mut out: Vec<&LoadedModule> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    walk_imports(module, graph, &mut visited, &mut out)?;
    Ok(out)
}

fn walk_imports<'a>(
    module: &'a LoadedModule,
    graph: &'a ModuleGraph,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<&'a LoadedModule>,
) -> Result<(), Vec<Diagnostic>> {
    for item in &module.program.items {
        let Item::Import(imp) = item else {
            continue;
        };
        let Some(dep_path) = resolve_import_path_with_manifest(
            &imp.path,
            &module.path,
            &graph.project_root,
        ) else {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("import not found: {}", imp.path),
                imp.span,
            )]);
        };
        let key = path_key(&dep_path);
        if !visited.insert(key.clone()) {
            continue;
        }
        let Some(dep) = graph
            .order
            .iter()
            .find(|m| path_key(&m.path) == key)
        else {
            continue;
        };
        walk_imports(dep, graph, visited, out)?;
        out.push(dep);
    }
    Ok(())
}

/// Build typing environment: transitive imports, then this module's items.
pub fn type_env_for_module(
    module: &LoadedModule,
    graph: &ModuleGraph,
) -> Result<crate::infer::TypeEnv, Vec<Diagnostic>> {
    let mut imports = ModuleExports::default();
    for dep in transitive_import_modules(module, graph)? {
        let exp = exports_for_module(dep);
        let span = module
            .program
            .items
            .iter()
            .find_map(|i| {
                if let Item::Import(imp) = i {
                    let p = resolve_import_path_with_manifest(
                        &imp.path,
                        &module.path,
                        &graph.project_root,
                    )?;
                    if path_key(&p) == path_key(&dep.path) {
                        Some(imp.span)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap_or_default();
        merge_exports(&mut imports, &exp, span)?;
    }

    let mut env = crate::infer::TypeEnv::default();
    env.structs = imports.structs.clone();
    env.enums = imports.enums.clone();
    env.frames = imports.frames.clone();
    for f in imports.fns.values() {
        let params: Vec<_> = f.params.iter().map(|p| p.ty.clone()).collect();
        env.fns.insert(f.name.clone(), (params, f.ret.clone()));
    }
    for item in &module.program.items {
        match item {
            Item::Struct(s) => {
                if env.structs.contains_key(&s.name)
                    || env.enums.contains_key(&s.name)
                    || env.fns.contains_key(&s.name)
                    || env.frames.contains_key(&s.name)
                {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{}` already defined in an imported module", s.name),
                        s.span,
                    )]);
                }
                env.structs.insert(s.name.clone(), s.clone());
            }
            Item::Frame(f) => {
                if env.structs.contains_key(&f.name)
                    || env.enums.contains_key(&f.name)
                    || env.fns.contains_key(&f.name)
                    || env.frames.contains_key(&f.name)
                {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{}` already defined in an imported module", f.name),
                        f.span,
                    )]);
                }
                env.frames.insert(f.name.clone(), f.clone());
                env.structs
                    .insert(f.name.clone(), crate::infer::frame_as_struct(f));
            }
            Item::Enum(e) => {
                if env.structs.contains_key(&e.name)
                    || env.enums.contains_key(&e.name)
                    || env.fns.contains_key(&e.name)
                {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{}` already defined in an imported module", e.name),
                        e.span,
                    )]);
                }
                env.enums.insert(e.name.clone(), e.clone());
            }
            Item::Fn(f) => {
                if env.structs.contains_key(&f.name)
                    || env.enums.contains_key(&f.name)
                    || env.fns.contains_key(&f.name)
                {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{}` already defined in an imported module", f.name),
                        f.span,
                    )]);
                }
                let params: Vec<_> = f.params.iter().map(|p| p.ty.clone()).collect();
                env.fns.insert(f.name.clone(), (params, f.ret.clone()));
            }
            Item::Import(_) => {}
        }
    }
    Ok(env)
}

/// Callable user function names visible when lowering `module` (imports + local).
pub fn callable_fn_names(module: &LoadedModule, graph: &ModuleGraph) -> HashMap<String, ()> {
    let mut names = HashMap::new();
    if let Ok(deps) = transitive_import_modules(module, graph) {
        for m in deps {
            for item in &m.program.items {
                if let Item::Fn(f) = item {
                    names.insert(f.name.clone(), ());
                }
            }
        }
    }
    for item in &module.program.items {
        if let Item::Fn(f) = item {
            names.insert(f.name.clone(), ());
        }
    }
    names
}

pub fn infer_module(
    module: &LoadedModule,
    graph: &ModuleGraph,
) -> DiagResult<crate::infer::InferOutput> {
    let env = type_env_for_module(module, graph)?;
    crate::infer::infer_program_with_env(&module.program, &env)
}

pub fn check_module_semantics(
    module: &LoadedModule,
    graph: &ModuleGraph,
) -> DiagResult<()> {
    let out = infer_module(module, graph)?;
    crate::effects::check_effects_with_infer(&module.program, &out)?;
    crate::ownership::check_ownership(&module.program)?;
    crate::device::check_devices(&module.program)?;
    Ok(())
}
