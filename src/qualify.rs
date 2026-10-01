use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode};
use crate::modules::{path_key, resolve_import_path_with_manifest, LoadedModule, ModuleGraph};
use crate::span::Span;

/// Last path segment (without `.sal`) used as an implicit qualifier for `import path`.
pub fn implicit_qualifier(import_path: &str) -> Option<String> {
    let base = import_path.strip_suffix(".sal").unwrap_or(import_path);
    let seg = base.rsplit('/').next().unwrap_or(base);
    let seg = seg.rsplit('.').next().unwrap_or(seg);
    if seg.is_empty() || !seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    if seg.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(seg.to_string())
}

pub fn qualify_module_graph(graph: &mut ModuleGraph) -> Result<(), Vec<Diagnostic>> {
    if !needs_qualify(graph) {
        return Ok(());
    }

    let clashes = compute_clashing_names(graph);
    let variant_clashes = compute_clashing_variant_names(graph);
    let modids = module_modids(graph);
    let export_links = build_export_links(graph, &clashes, &modids);
    let variant_links = build_variant_links(graph, &variant_clashes, &modids);

    for m in &graph.order {
        check_flat_import_export_clashes(m, graph)?;
    }

    let n = graph.order.len();
    for i in 0..n {
        let qualifiers = module_qualifiers(&graph.order[i], graph)?;
        let path = graph.order[i].path.clone();
        qualify_program(
            &mut graph.order[i].program,
            &qualifiers,
            &export_links,
            &variant_links,
        )?;
        rename_clashing_defs(&mut graph.order[i].program, &clashes, &modids, &path);
    }
    Ok(())
}

fn needs_qualify(graph: &ModuleGraph) -> bool {
    if graph.order.iter().any(|m| {
        m.program
            .items
            .iter()
            .any(|i| matches!(i, Item::Import(imp) if imp.alias.is_some()))
    }) {
        return true;
    }
    !compute_clashing_names(graph).is_empty()
}

fn compute_clashing_names(graph: &ModuleGraph) -> HashSet<String> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for module in &graph.order {
        for name in export_item_names(&module.program) {
            *counts.entry(name).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|(n, _)| n)
        .collect()
}

fn compute_clashing_variant_names(graph: &ModuleGraph) -> HashSet<String> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for module in &graph.order {
        for item in &module.program.items {
            if let Item::Enum(e) = item {
                for v in &e.variants {
                    *counts.entry(v.name.clone()).or_default() += 1;
                }
            }
        }
    }
    counts
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|(n, _)| n)
        .collect()
}

fn export_item_names(prog: &Program) -> Vec<String> {
    let mut out = Vec::new();
    for item in &prog.items {
        match item {
            Item::Fn(f) => out.push(f.name.clone()),
            Item::Struct(s) => out.push(s.name.clone()),
            Item::Enum(e) => out.push(e.name.clone()),
            Item::Frame(f) => out.push(f.name.clone()),
            _ => {}
        }
    }
    out
}

fn check_flat_import_export_clashes(
    module: &LoadedModule,
    graph: &ModuleGraph,
) -> Result<(), Vec<Diagnostic>> {
    let mut seen: HashMap<String, Span> = HashMap::new();
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
            continue;
        };
        let dep_key = path_key(&dep_path);
        let Some(dep) = graph
            .order
            .iter()
            .find(|m| path_key(&m.path) == dep_key)
        else {
            continue;
        };
        for name in export_item_names(&dep.program) {
            if let Some(_prev) = seen.get(&name) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("name `{name}` defined in more than one imported module"),
                    imp.span,
                )]);
            }
            seen.insert(name, imp.span);
        }
    }
    Ok(())
}

fn module_modids(graph: &ModuleGraph) -> HashMap<PathBuf, String> {
    let mut out = HashMap::new();
    for m in &graph.order {
        out.insert(path_key(&m.path), modid_from_path(&m.path, &graph.project_root));
    }
    out
}

fn modid_from_path(path: &Path, project_root: &Path) -> String {
    let rel = path
        .strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy();
    let mut s = rel.replace('/', "__");
    if s.ends_with(".sal") {
        s.truncate(s.len() - 4);
    }
    s = s.replace('.', "_");
    if s.is_empty() {
        "root".to_string()
    } else {
        s
    }
}

fn link_name(modid: &str, name: &str, clashing: bool) -> String {
    if clashing {
        format!("{modid}__{name}")
    } else {
        name.to_string()
    }
}

fn build_export_links(
    graph: &ModuleGraph,
    clashes: &HashSet<String>,
    modids: &HashMap<PathBuf, String>,
) -> HashMap<PathBuf, HashMap<String, String>> {
    let _ = graph;
    let mut out = HashMap::new();
    for module in &graph.order {
        let key = path_key(&module.path);
        let modid = modids.get(&key).cloned().unwrap_or_else(|| "mod".into());
        let mut links = HashMap::new();
        for name in export_item_names(&module.program) {
            links.insert(
                name.clone(),
                link_name(&modid, &name, clashes.contains(&name)),
            );
        }
        out.insert(key, links);
    }
    out
}

fn build_variant_links(
    graph: &ModuleGraph,
    variant_clashes: &HashSet<String>,
    modids: &HashMap<PathBuf, String>,
) -> HashMap<PathBuf, HashMap<String, String>> {
    let mut out = HashMap::new();
    for module in &graph.order {
        let key = path_key(&module.path);
        let modid = modids.get(&key).cloned().unwrap_or_else(|| "mod".into());
        let mut links = HashMap::new();
        for item in &module.program.items {
            if let Item::Enum(e) = item {
                for v in &e.variants {
                    links.insert(
                        v.name.clone(),
                        link_name(&modid, &v.name, variant_clashes.contains(&v.name)),
                    );
                }
            }
        }
        out.insert(key, links);
    }
    out
}

pub(crate) struct ModuleQualifiers {
    explicit: HashMap<String, PathBuf>,
    implicit: HashMap<String, PathBuf>,
}

fn module_qualifiers(
    module: &LoadedModule,
    graph: &ModuleGraph,
) -> Result<ModuleQualifiers, Vec<Diagnostic>> {
    let mut explicit = HashMap::new();
    let mut implicit = HashMap::new();
    let local_names = local_item_names(&module.program);

    for item in &module.program.items {
        let Item::Import(imp) = item else { continue };
        let Some(dep_path) = resolve_import_path_with_manifest(
            &imp.path,
            &module.path,
            &graph.project_root,
        ) else {
            continue;
        };
        let dep_key = path_key(&dep_path);

        if let Some(alias) = &imp.alias {
            if local_names.contains(alias) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("import alias `{alias}` conflicts with a local item"),
                    imp.span,
                )]);
            }
            if explicit.contains_key(alias) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("duplicate import alias `{alias}`"),
                    imp.span,
                )]);
            }
            explicit.insert(alias.clone(), dep_key);
        } else if let Some(iq) = implicit_qualifier(&imp.path) {
            if local_names.contains(&iq) {
                continue;
            }
            if implicit.contains_key(&iq) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("ambiguous implicit qualifier `{iq}`"),
                    imp.span,
                )]);
            }
            implicit.insert(iq, dep_key);
        }
    }
    Ok(ModuleQualifiers {
        explicit,
        implicit,
    })
}

fn local_item_names(prog: &Program) -> HashSet<String> {
    export_item_names(prog).into_iter().collect()
}

pub(crate) struct QualifyCtx<'a> {
    pub qualifiers: &'a ModuleQualifiers,
    pub export_links: &'a HashMap<PathBuf, HashMap<String, String>>,
    pub variant_links: &'a HashMap<PathBuf, HashMap<String, String>>,
}

fn resolve_export_link(
    ctx: &QualifyCtx<'_>,
    dep_key: &PathBuf,
    item: &str,
    span: Span,
) -> Result<String, Vec<Diagnostic>> {
    let links = ctx.export_links.get(dep_key).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            format!("unknown module for `{item}`"),
            span,
        )]
    })?;
    links.get(item).cloned().ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            format!("`{item}` is not exported from that module"),
            span,
        )]
    })
}

fn resolve_variant_link(
    ctx: &QualifyCtx<'_>,
    dep_key: &PathBuf,
    variant: &str,
    span: Span,
) -> Result<String, Vec<Diagnostic>> {
    let links = ctx.variant_links.get(dep_key).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            format!("unknown module for variant `{variant}`"),
            span,
        )]
    })?;
    links.get(variant).cloned().ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            format!("no variant `{variant}` in that module"),
            span,
        )]
    })
}

fn qualifier_dep(ctx: &QualifyCtx<'_>, qual: &str) -> Option<PathBuf> {
    ctx.qualifiers
        .explicit
        .get(qual)
        .or_else(|| ctx.qualifiers.implicit.get(qual))
        .cloned()
}

fn qualify_program(
    prog: &mut Program,
    qualifiers: &ModuleQualifiers,
    export_links: &HashMap<PathBuf, HashMap<String, String>>,
    variant_links: &HashMap<PathBuf, HashMap<String, String>>,
) -> Result<(), Vec<Diagnostic>> {
    let ctx = QualifyCtx {
        qualifiers,
        export_links,
        variant_links,
    };
    for item in &mut prog.items {
        match item {
            Item::Fn(f) => {
                qualify_type(&mut f.ret, &ctx)?;
                for p in &mut f.params {
                    qualify_type(&mut p.ty, &ctx)?;
                }
                qualify_block(&mut f.body, &ctx)?;
            }
            Item::Struct(s) => {
                for field in &mut s.fields {
                    qualify_type(&mut field.ty, &ctx)?;
                }
            }
            Item::Enum(e) => {
                for v in &mut e.variants {
                    for t in &mut v.fields {
                        qualify_type(t, &ctx)?;
                    }
                }
            }
            Item::Frame(_) | Item::Import(_) => {}
        }
    }
    Ok(())
}

fn rename_clashing_defs(
    prog: &mut Program,
    clashes: &HashSet<String>,
    modids: &HashMap<PathBuf, String>,
    path: &Path,
) {
    let modid = modids
        .get(&path_key(path))
        .cloned()
        .unwrap_or_else(|| "mod".into());
    for item in &mut prog.items {
        match item {
            Item::Fn(f) if clashes.contains(&f.name) => {
                f.name = link_name(&modid, &f.name, true);
            }
            Item::Struct(s) if clashes.contains(&s.name) => {
                s.name = link_name(&modid, &s.name, true);
            }
            Item::Enum(e) if clashes.contains(&e.name) => {
                e.name = link_name(&modid, &e.name, true);
            }
            Item::Frame(f) if clashes.contains(&f.name) => {
                f.name = link_name(&modid, &f.name, true);
            }
            _ => {}
        }
    }
}

fn qualify_block(block: &mut Block, ctx: &QualifyCtx<'_>) -> Result<(), Vec<Diagnostic>> {
    for st in &mut block.stmts {
        qualify_stmt(st, ctx)?;
    }
    if let Some(t) = &mut block.tail {
        qualify_expr(t, ctx)?;
    }
    Ok(())
}

fn qualify_stmt(st: &mut Stmt, ctx: &QualifyCtx<'_>) -> Result<(), Vec<Diagnostic>> {
    match st {
        Stmt::Let { ty, init, .. } => {
            if let Some(t) = ty {
                qualify_type(t, ctx)?;
            }
            qualify_expr(init, ctx)?;
        }
        Stmt::Expr(e) => qualify_expr(e, ctx)?,
        Stmt::Assign { target, value, .. } => {
            qualify_expr(target, ctx)?;
            qualify_expr(value, ctx)?;
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                qualify_expr(v, ctx)?;
            }
        }
        Stmt::While { cond, body, .. } => {
            qualify_expr(cond, ctx)?;
            qualify_block(body, ctx)?;
        }
    }
    Ok(())
}

fn qualify_expr(expr: &mut Expr, ctx: &QualifyCtx<'_>) -> Result<(), Vec<Diagnostic>> {
    match expr {
        Expr::Call {
            func,
            type_args,
            args,
            ..
        } => {
            qualify_expr(func, ctx)?;
            for a in type_args {
                if let TypeArg::Type(t) = a {
                    qualify_type(t, ctx)?;
                }
            }
            for a in args {
                qualify_expr(a, ctx)?;
            }
        }
        Expr::Field { base, field, span } => {
            if let Expr::Ident { name: qual, .. } = &**base {
                if let Some(dep) = qualifier_dep(ctx, qual) {
                    let link = resolve_export_link(ctx, &dep, field, *span)?;
                    *expr = Expr::Ident {
                        name: link,
                        span: *span,
                    };
                    return Ok(());
                }
            }
            qualify_expr(base, ctx)?;
        }
        Expr::Binary { left, right, .. } => {
            qualify_expr(left, ctx)?;
            qualify_expr(right, ctx)?;
        }
        Expr::Unary { expr: inner, .. } => qualify_expr(inner, ctx)?,
        Expr::Block(b) => qualify_block(b, ctx)?,
        Expr::On { body, .. } => qualify_block(body, ctx)?,
        Expr::To { expr: inner, .. } => qualify_expr(inner, ctx)?,
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    qualify_expr(c, ctx)?;
                }
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            qualify_expr(scrutinee, ctx)?;
            for arm in arms {
                qualify_pattern(&mut arm.pattern, ctx)?;
                qualify_expr(&mut arm.body, ctx)?;
            }
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            ..
        } => {
            qualify_expr(cond, ctx)?;
            qualify_block(then_block, ctx)?;
            for e in elsifs {
                qualify_expr(&mut e.cond, ctx)?;
                qualify_block(&mut e.body, ctx)?;
            }
            if let Some(b) = else_block {
                qualify_block(b, ctx)?;
            }
        }
        Expr::Lambda { body, .. } => qualify_expr(body, ctx)?,
        Expr::Try { expr: inner, .. } => qualify_expr(inner, ctx)?,
        _ => {}
    }
    Ok(())
}

fn qualify_type(ty: &mut Type, ctx: &QualifyCtx<'_>) -> Result<(), Vec<Diagnostic>> {
    match ty {
        Type::Qualified {
            qual,
            name,
            args,
            span,
        } => {
            let dep = qualifier_dep(ctx, qual).ok_or_else(|| {
                vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("unknown qualifier `{qual}`"),
                    *span,
                )]
            })?;
            let link = resolve_export_link(ctx, &dep, name, *span)?;
            for a in args.iter_mut() {
                qualify_type(a, ctx)?;
            }
            let resolved_args = std::mem::take(args);
            *ty = Type::Named {
                name: link,
                args: resolved_args,
                span: *span,
            };
        }
        Type::Named { args, .. } => {
            for a in args {
                qualify_type(a, ctx)?;
            }
        }
        Type::Fn { params, ret, .. } => {
            for p in params {
                qualify_type(p, ctx)?;
            }
            qualify_type(ret, ctx)?;
        }
        Type::Tensor { .. } => {}
    }
    Ok(())
}

fn qualify_pattern(pat: &mut Pattern, ctx: &QualifyCtx<'_>) -> Result<(), Vec<Diagnostic>> {
    match pat {
        Pattern::Variant {
            qual,
            name,
            args,
            span,
        } => {
            if let Some(q) = qual.take() {
                let dep = qualifier_dep(ctx, &q).ok_or_else(|| {
                    vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("unknown qualifier `{q}`"),
                        *span,
                    )]
                })?;
                let _ = resolve_export_link(ctx, &dep, name, *span)?;
                let vlink = resolve_variant_link(ctx, &dep, name, *span)?;
                *name = vlink;
            }
            for a in args {
                qualify_pattern(a, ctx)?;
            }
        }
        Pattern::Ident(_, _) | Pattern::Wild(_) | Pattern::Int(_, _) => {}
    }
    Ok(())
}
