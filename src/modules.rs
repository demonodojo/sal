use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{EnumDef, FnDef, FrameDef, Item, Program, StructDef};
use crate::consteval::ConstValue;
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
    pub protocols: HashMap<String, crate::ast::ProtocolDef>,
    pub consts: HashMap<String, ConstValue>,
}

#[derive(Clone)]
pub struct LoadedModule {
    pub path: PathBuf,
    pub program: Program,
    /// The module defines `method`s (`methods::has_methods` before desugaring).
    pub has_methods: bool,
}

#[derive(Clone)]
pub struct ModuleGraph {
    pub root: PathBuf,
    pub project_root: PathBuf,
    /// Dependency order: imports before importers; root is last.
    pub order: Vec<LoadedModule>,
}

/// Extra search bases from manifest paths and lock packages (see `packages` module).
pub fn dependency_search_bases(project_root: &Path) -> Vec<PathBuf> {
    crate::packages::dependency_search_bases(project_root)
        .into_iter()
        .map(|b| b.path)
        .collect()
}

fn all_search_bases(project_root: &Path) -> Vec<crate::packages::DepSearchBase> {
    crate::packages::dependency_search_bases(project_root)
}

fn dep_split_matches(base: &crate::packages::DepSearchBase, dep: &str) -> bool {
    if base
        .dep_name
        .as_deref()
        .is_some_and(|n| n == dep || (n == "std" && dep == "std"))
    {
        return true;
    }
    base.path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| n == dep)
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
    push_candidates(&mut candidates, &project_root.join("sal_modules"), import_path);
    for base in all_search_bases(project_root) {
        push_candidates(&mut candidates, &base.path, import_path);
        if let Some((dep, rest)) = import_path.split_once('/') {
            if dep_split_matches(&base, dep) {
                push_candidates(&mut candidates, &base.path, rest);
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

pub(crate) fn path_key(path: &Path) -> PathBuf {
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
    let mut graph = ModuleGraph {
        root: root.clone(),
        project_root: project_root.to_path_buf(),
        order,
    };
    crate::qualify::qualify_module_graph(&mut graph)?;
    resolve_graph_methods(&mut graph)?;
    monomorphize_graph(&mut graph)?;
    Ok(graph)
}

/// `specialize::monomorphize` per module. A generic called from another module stays
/// erased: its copies would have to live in the caller's module.
fn monomorphize_graph(graph: &mut ModuleGraph) -> Result<(), Vec<Diagnostic>> {
    let called: Vec<HashSet<String>> =
        graph.order.iter().map(|m| crate::specialize::called_names(&m.program)).collect();
    for i in 0..graph.order.len() {
        let mut external: HashSet<String> = HashSet::new();
        for (j, names) in called.iter().enumerate() {
            if j != i {
                external.extend(names.iter().cloned());
            }
        }
        let mut prog = graph.order[i].program.clone();
        let path = graph.order[i].path.clone();
        let has_methods = graph.order[i].has_methods;
        let g: &ModuleGraph = graph;
        crate::specialize::monomorphize(&mut prog, &external, &|p: &Program| {
            let m = LoadedModule { path: path.clone(), program: p.clone(), has_methods };
            type_env_for_module(&m, g).ok()
        })?;
        graph.order[i].program = prog;
    }
    Ok(())
}

/// Rewrite `p.m(args)` to `T__m(p, args)` (or to the builtin, for `xs.push(v)`)
/// in every module once the graph is qualified. Only when some module defines
/// a method (definitions were already desugared at load time, which is why that
/// flag is recorded by the loader) or calls something of the form `x.m(…)`.
fn resolve_graph_methods(graph: &mut ModuleGraph) -> Result<(), Vec<Diagnostic>> {
    if !graph
        .order
        .iter()
        .any(|m| m.has_methods || crate::methods::has_field_calls(&m.program))
    {
        return Ok(());
    }
    for i in 0..graph.order.len() {
        let env = type_env_for_module(&graph.order[i], graph)?;
        let mut prog = graph.order[i].program.clone();
        crate::methods::resolve_methods(&mut prog, &env);
        graph.order[i].program = prog;
    }
    Ok(())
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
    let mut program = parse(&crate::interp::with_prelude_enums(&src)).map_err(|d| vec![d])?;
    crate::interp::expand_interpolation(&mut program);
    crate::loops::desugar_break(&mut program)?;
    let has_methods = crate::methods::has_methods(&program);
    crate::methods::desugar_method_defs(&mut program)?;
    if crate::protocols::has_protocols(&program) {
        let types = crate::infer::type_env_of_program(&program);
        crate::protocols::check_program_protocols(&program, &types)?;
    }

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
        has_methods,
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
            Item::Protocol(p) => {
                out.protocols.insert(p.name.clone(), p.clone());
            }
            Item::Const(_) | Item::Import(_) => {}
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
            || into.consts.contains_key(name)
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
            || into.consts.contains_key(name)
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
            || into.consts.contains_key(name)
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
            || into.consts.contains_key(name)
        {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{name}` defined in more than one imported module"),
                at,
            )]);
        }
        into.fns.insert(name.clone(), f.clone());
    }
    for (name, p) in &from.protocols {
        if into.protocols.contains_key(name)
            || into.structs.contains_key(name)
            || into.enums.contains_key(name)
            || into.fns.contains_key(name)
            || into.consts.contains_key(name)
        {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{name}` defined in more than one imported module"),
                at,
            )]);
        }
        into.protocols.insert(name.clone(), p.clone());
    }
    for (name, v) in &from.consts {
        if into.consts.contains_key(name)
            || into.structs.contains_key(name)
            || into.enums.contains_key(name)
            || into.fns.contains_key(name)
            || into.frames.contains_key(name)
            || into.protocols.contains_key(name)
        {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{name}` defined in more than one imported module"),
                at,
            )]);
        }
        into.consts.insert(name.clone(), v.clone());
    }
    Ok(())
}

/// Transitive dependencies reachable without `import … as` (flattened into the type env).
pub(crate) fn flat_transitive_import_modules<'a>(
    module: &'a LoadedModule,
    graph: &'a ModuleGraph,
) -> Result<Vec<&'a LoadedModule>, Vec<Diagnostic>> {
    let mut out: Vec<&LoadedModule> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    walk_flat_imports(module, graph, &mut visited, &mut out)?;
    Ok(out)
}

fn walk_flat_imports<'a>(
    module: &'a LoadedModule,
    graph: &'a ModuleGraph,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<&'a LoadedModule>,
) -> Result<(), Vec<Diagnostic>> {
    for item in &module.program.items {
        let Item::Import(imp) = item else {
            continue;
        };
        if imp.alias.is_some() {
            continue;
        }
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
        walk_flat_imports(dep, graph, visited, out)?;
        out.push(dep);
    }
    Ok(())
}

fn transitive_import_modules<'a>(
    module: &'a LoadedModule,
    graph: &'a ModuleGraph,
) -> Result<Vec<&'a LoadedModule>, Vec<Diagnostic>> {
    let mut out: Vec<&LoadedModule> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    walk_all_imports(module, graph, &mut visited, &mut out)?;
    Ok(out)
}

fn walk_all_imports<'a>(
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
        walk_all_imports(dep, graph, visited, out)?;
        out.push(dep);
    }
    Ok(())
}

/// Build typing environment: transitive imports, then this module's items.
pub fn type_env_for_module(
    module: &LoadedModule,
    graph: &ModuleGraph,
) -> Result<crate::infer::TypeEnv, Vec<Diagnostic>> {
    let own_consts = own_consts_by_module(graph)?;
    let mut imports = ModuleExports::default();
    for dep in flat_transitive_import_modules(module, graph)? {
        let mut exp = exports_for_module(dep);
        if let Some(cs) = own_consts.get(&path_key(&dep.path)) {
            exp.consts = cs.clone();
        }
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
    env.protocols = imports.protocols.clone();
    env.consts = imports.consts.clone();
    for f in imports.fns.values() {
        crate::infer::register_fn(&mut env, f);
    }
    // Functions reached through an aliased import (`alias.f()`, rewritten to a
    // plain call by `qualify`) are not flat exports but still have a signature.
    for dep in &graph.order {
        if path_key(&dep.path) == path_key(&module.path) {
            continue;
        }
        for item in &dep.program.items {
            if let Item::Fn(f) = item {
                if !env.fns.contains_key(&f.name) {
                    crate::infer::register_fn(&mut env, f);
                    // The types in its signature come with it (`one.pick1()` is a `Color`),
                    // but the module's other types stay out of this module's scope.
                    register_signature_types(&mut env, f, &dep.program.items);
                }
            }
        }
    }
    // `import … as alias` does not flatten, but `alias.NOMBRE` was rewritten to the
    // link name, so that constant's value has to be visible under that name.
    for item in &module.program.items {
        let Item::Import(imp) = item else { continue };
        if imp.alias.is_none() {
            continue;
        }
        let Some(dep_path) = resolve_import_path_with_manifest(
            &imp.path,
            &module.path,
            &graph.project_root,
        ) else {
            continue;
        };
        let Some(cs) = own_consts.get(&path_key(&dep_path)) else {
            continue;
        };
        for (n, v) in cs {
            env.consts.entry(n.clone()).or_insert_with(|| v.clone());
        }
    }
    for item in &module.program.items {
        match item {
            Item::Struct(s) => {
                if env.structs.contains_key(&s.name)
                    || env.enums.contains_key(&s.name)
                    || env.fns.contains_key(&s.name)
                    || env.frames.contains_key(&s.name)
                    || env.consts.contains_key(&s.name)
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
                    || env.consts.contains_key(&f.name)
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
                    || env.consts.contains_key(&e.name)
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
                    || env.consts.contains_key(&f.name)
                {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{}` already defined in an imported module", f.name),
                        f.span,
                    )]);
                }
                crate::infer::register_fn(&mut env, f);
            }
            Item::Protocol(p) => {
                if env.protocols.contains_key(&p.name)
                    || env.structs.contains_key(&p.name)
                    || env.enums.contains_key(&p.name)
                    || env.fns.contains_key(&p.name)
                    || env.consts.contains_key(&p.name)
                {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{}` already defined in an imported module", p.name),
                        p.span,
                    )]);
                }
                env.protocols.insert(p.name.clone(), p.clone());
            }
            Item::Const(c) => {
                if env.structs.contains_key(&c.name)
                    || env.enums.contains_key(&c.name)
                    || env.fns.contains_key(&c.name)
                    || env.frames.contains_key(&c.name)
                    || env.protocols.contains_key(&c.name)
                    || env.consts.contains_key(&c.name)
                {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{}` already defined", c.name),
                        c.span,
                    )]);
                }
            }
            Item::Import(_) => {}
        }
    }
    for item in &module.program.items {
        let Item::Const(c) = item else { continue };
        if local_item_named(&module.program, &c.name, c.span).is_some() {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{}` already defined", c.name),
                c.span,
            )]);
        }
    }
    let own = crate::consteval::eval_consts(&module.program, &env.consts)?;
    env.consts.extend(own);
    Ok(env)
}

/// Another item of `prog` (not this constant) already uses `name`.
fn local_item_named(prog: &Program, name: &str, self_span: Span) -> Option<Span> {
    for item in &prog.items {
        let (n, span) = match item {
            Item::Fn(f) => (f.name.as_str(), f.span),
            Item::Struct(s) => (s.name.as_str(), s.span),
            Item::Enum(e) => (e.name.as_str(), e.span),
            Item::Frame(f) => (f.name.as_str(), f.span),
            Item::Protocol(p) => (p.name.as_str(), p.span),
            Item::Const(c) if c.span != self_span => (c.name.as_str(), c.span),
            _ => continue,
        };
        if n == name {
            return Some(span);
        }
    }
    None
}

/// Each module's own constants, dependencies first. The key is `path_key`.
fn own_consts_by_module(
    graph: &ModuleGraph,
) -> Result<HashMap<PathBuf, HashMap<String, ConstValue>>, Vec<Diagnostic>> {
    let mut cache: HashMap<PathBuf, HashMap<String, ConstValue>> = HashMap::new();
    for m in &graph.order {
        let mut visible: HashMap<String, ConstValue> = HashMap::new();
        for dep in flat_transitive_import_modules(m, graph)? {
            let Some(cs) = cache.get(&path_key(&dep.path)) else {
                continue;
            };
            for (n, v) in cs {
                if visible.contains_key(n) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("name `{n}` defined in more than one imported module"),
                        Span::default(),
                    )]);
                }
                visible.insert(n.clone(), v.clone());
            }
        }
        for item in &m.program.items {
            let Item::Import(imp) = item else { continue };
            if imp.alias.is_none() {
                continue;
            }
            let Some(dep_path) = resolve_import_path_with_manifest(
                &imp.path,
                &m.path,
                &graph.project_root,
            ) else {
                continue;
            };
            let Some(cs) = cache.get(&path_key(&dep_path)) else {
                continue;
            };
            for (n, v) in cs {
                visible.entry(n.clone()).or_insert_with(|| v.clone());
            }
        }
        let own = crate::consteval::eval_consts(&m.program, &visible)?;
        cache.insert(path_key(&m.path), own);
    }
    Ok(cache)
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
    target: crate::target::Target,
) -> DiagResult<crate::infer::InferOutput> {
    let mut env = type_env_for_module(module, graph)?;
    env.target = target;
    crate::infer::infer_program_with_env(&module.program, &env)
}

/// Parameter types of every fn of the graph, by final name (the borrow checker reads them to
/// know what a call across modules does with each argument).
pub fn graph_sigs(graph: &ModuleGraph) -> HashMap<String, Vec<crate::ast::Type>> {
    let mut sigs = HashMap::new();
    for m in &graph.order {
        for item in &m.program.items {
            if let Item::Fn(f) = item {
                sigs.insert(f.name.clone(), f.params.iter().map(|p| p.ty.clone()).collect());
            }
        }
    }
    sigs
}

pub fn graph_enums(graph: &ModuleGraph) -> Vec<crate::ast::EnumDef> {
    let mut out = Vec::new();
    for m in &graph.order {
        for item in &m.program.items {
            if let Item::Enum(e) = item {
                out.push(e.clone());
            }
        }
    }
    out
}

pub fn check_module_semantics(
    module: &LoadedModule,
    graph: &ModuleGraph,
    target: crate::target::Target,
) -> DiagResult<()> {
    let out = infer_module(module, graph, target)?;
    let programs: Vec<&Program> = graph.order.iter().map(|m| &m.program).collect();
    let strings = crate::string_expr::StringEnv::from_program(&module.program).with_infer(&out);
    crate::attrs::check_layout_structs(&module.program)?;
    crate::attrs::check_stack_lets(&module.program, &strings)?;
    crate::attrs::check_no_heap_fns(&module.program, &strings)?;
    crate::attrs::check_arena_fns(&module.program)?;
    crate::effects::check_effects_with_graph(&module.program, &out, &programs)?;
    crate::borrowck::set_extern_sigs(graph_sigs(graph));
    crate::borrowck::set_extern_enums(graph_enums(graph));
    crate::ownership::check_ownership(&module.program, &out)?;
    crate::device::check_devices(&module.program)?;
    Ok(())
}

/// Adds to `env` the struct and enum types named in the signature of `f` that `items` define.
fn register_signature_types(env: &mut crate::infer::TypeEnv, f: &crate::ast::FnDef, items: &[Item]) {
    let mut names: Vec<String> = f.params.iter().filter_map(|p| named_type(&p.ty)).collect();
    names.extend(named_type(&f.ret));
    for name in names {
        for item in items {
            match item {
                Item::Struct(s) if s.name == name && !env.structs.contains_key(&name) => {
                    env.structs.insert(name.clone(), s.clone());
                }
                Item::Enum(e) if e.name == name && !env.opaque_enums.contains_key(&name) => {
                    env.opaque_enums.insert(name.clone(), e.clone());
                }
                _ => {}
            }
        }
    }
}

fn named_type(ty: &crate::ast::Type) -> Option<String> {
    match ty {
        crate::ast::Type::Named { name, .. } => Some(name.clone()),
        _ => None,
    }
}
