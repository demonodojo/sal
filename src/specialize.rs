//! Monomorphization (SPEC «Tipos»: genéricos → monomorfización).
//!
//! A call to a generic `fn g[T, …]` whose argument types bind every type
//! parameter to a concrete type gets its own copy `g__<tipos>` with the type
//! parameters replaced, and the call is renamed to it. Copies can call other
//! generics, so the pass iterates until no call changes. The generic original
//! is then removed: every later pass (effects, ownership, drops, lowering)
//! sees only concrete types, and no two functions share spans.
//!
//! All or nothing per generic: if some call of `g` cannot be specialized
//! (an argument of unknown type, a tensor, a call from another module) `g`
//! stays erased, as before this pass, and none of its calls is rewritten.
//!
//! Runs after `methods::resolve_methods` and before the regular inference.
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::DiagResult;
use crate::infer::{infer_program_with_env, TypeEnv};
use crate::span::Span;

/// (calling fn, call span start, call span end) → (callee, type arguments).
pub type GenericCalls = HashMap<(String, u32, u32), (String, Option<Vec<Type>>)>;

/// The type parameters of `f` that are types (not lifetimes).
pub fn type_params_of(f: &FnDef) -> Vec<String> {
    f.type_params.iter().filter(|p| !p.starts_with('\'')).cloned().collect()
}

/// Called by inference at each call of a user fn with its argument types.
/// Returns the call's result type with the bound type parameters substituted
/// (unbound ones are `Unknown`), or `None` when `callee` is not generic.
pub fn record_generic_call(types: &TypeEnv, callee: &FnDef, arg_tys: &[Type], span: Span) -> Option<Type> {
    let tps = type_params_of(callee);
    if tps.is_empty() {
        return None;
    }
    let mut binds: HashMap<String, Type> = HashMap::new();
    for (p, a) in callee.params.iter().zip(arg_tys) {
        bind(&p.ty, a, &tps, &mut binds);
    }
    let args: Option<Vec<Type>> = tps
        .iter()
        .map(|t| binds.get(t).filter(|ty| is_concrete(ty)).cloned())
        .collect();
    let caller = types.current_fn.borrow().clone();
    types
        .generic_calls
        .borrow_mut()
        .insert((caller, span.start, span.end), (callee.name.clone(), args));
    let unknown = Type::Named { name: "Unknown".into(), args: vec![], span };
    let sub: HashMap<String, Type> = tps
        .iter()
        .map(|t| (t.clone(), binds.get(t).cloned().unwrap_or_else(|| unknown.clone())))
        .collect();
    Some(subst(&callee.ret, &sub))
}

/// The concrete types `actual` gives the parameters `tps` through `decl`, in `tps` order; `None`
/// when one stays unfixed or is not concrete.
pub fn bind_all(decl: &[Type], actual: &[Type], tps: &[String]) -> Option<Vec<Type>> {
    let mut binds: HashMap<String, Type> = HashMap::new();
    for (d, a) in decl.iter().zip(actual) {
        bind(d, a, tps, &mut binds);
    }
    tps.iter().map(|t| binds.get(t).filter(|ty| is_concrete(ty)).cloned()).collect()
}

pub(crate) fn bind_partial(decl: &[Type], actual: &[Type], tps: &[String]) -> HashMap<String, Type> {
    let mut binds = HashMap::new();
    for (d, a) in decl.iter().zip(actual) {
        bind(d, a, tps, &mut binds);
    }
    binds
}

fn bind(decl: &Type, actual: &Type, tps: &[String], out: &mut HashMap<String, Type>) {
    match (decl, actual) {
        (Type::Named { name, args, .. }, _) if args.is_empty() && tps.contains(name) => {
            if !out.contains_key(name) && !is_unknown(actual) {
                out.insert(name.clone(), strip_span(actual));
            }
        }
        (Type::Named { name: a, args: aa, .. }, Type::Named { name: b, args: ba, .. })
            if a == b && aa.len() == ba.len() =>
        {
            for (x, y) in aa.iter().zip(ba) {
                bind(x, y, tps, out);
            }
        }
        (Type::Qualified { name: a, args: aa, .. }, Type::Qualified { name: b, args: ba, .. })
            if a == b && aa.len() == ba.len() =>
        {
            for (x, y) in aa.iter().zip(ba) {
                bind(x, y, tps, out);
            }
        }
        (Type::Ref { inner: d, .. }, Type::Ref { inner: a, .. }) => bind(d, a, tps, out),
        // `&T` receiving a value (auto-reference of a receiver): bind through it.
        (Type::Ref { inner: d, .. }, a) => bind(d, a, tps, out),
        (Type::Fn { params: dp, ret: dr, .. }, Type::Fn { params: ap, ret: ar, .. }) if dp.len() == ap.len() => {
            for (d, a) in dp.iter().zip(ap) { bind(d, a, tps, out); }
            bind(dr, ar, tps, out);
        }
        _ => {}
    }
}

/// `Unknown` anywhere in `t`.
pub fn mentions_unknown(t: &Type) -> bool {
    match t {
        Type::Named { name, args, .. } => name == "Unknown" || args.iter().any(mentions_unknown),
        Type::Qualified { args, .. } => args.iter().any(mentions_unknown),
        Type::Ref { inner, .. } => mentions_unknown(inner),
        _ => false,
    }
}

fn is_unknown(t: &Type) -> bool {
    matches!(t, Type::Named { name, .. } if name == "Unknown")
}

/// A type that can name a copy: no `Unknown` anywhere, no tensor or function.
fn is_concrete(t: &Type) -> bool {
    match t {
        Type::Named { name, args, .. } => name != "Unknown" && args.iter().all(is_concrete),
        Type::Qualified { args, .. } => args.iter().all(is_concrete),
        Type::Ref { inner, .. } => is_concrete(inner),
        Type::Tensor { .. } | Type::Fn { .. } => false,
    }
}

fn strip_span(t: &Type) -> Type {
    let s = Span::default();
    match t {
        Type::Named { name, args, .. } => {
            Type::Named { name: name.clone(), args: args.iter().map(strip_span).collect(), span: s }
        }
        Type::Qualified { qual, name, args, .. } => Type::Qualified {
            qual: qual.clone(),
            name: name.clone(),
            args: args.iter().map(strip_span).collect(),
            span: s,
        },
        Type::Ref { mutable, lifetime, inner, .. } => Type::Ref {
            mutable: *mutable,
            lifetime: lifetime.clone(),
            inner: Box::new(strip_span(inner)),
            span: s,
        },
        other => other.clone(),
    }
}

/// `List[Int]` → `List_Int`, `&String` → `ref_String`.
fn mangle_ty(t: &Type) -> String {
    match t {
        Type::Named { name, args, .. } => {
            let mut s = name.clone();
            for a in args {
                s.push('_');
                s.push_str(&mangle_ty(a));
            }
            s
        }
        Type::Qualified { qual, name, args, .. } => {
            let mut s = format!("{qual}_{name}");
            for a in args {
                s.push('_');
                s.push_str(&mangle_ty(a));
            }
            s
        }
        Type::Ref { mutable, inner, .. } => {
            format!("{}_{}", if *mutable { "mut" } else { "ref" }, mangle_ty(inner))
        }
        Type::Tensor { .. } | Type::Fn { .. } => "x".into(),
    }
}

/// Copy name: `g__Int_String`.
pub fn mangled_name(callee: &str, args: &[Type]) -> String {
    let parts: Vec<String> = args.iter().map(mangle_ty).collect();
    format!("{callee}__{}", parts.join("_"))
}

/// What one module's monomorphization sees of the rest of the graph.
#[derive(Default)]
pub struct MonoCtx {
    /// Generic fns of the other modules that this one can call, by final name.
    pub externals: HashMap<String, FnDef>,
    /// Copies already made in the module that defines their generic (so visible wherever the
    /// generic is): (generic, mangled types) → copy name. A call that needs one of them calls it.
    pub existing: HashMap<(String, Vec<String>), String>,
    /// Every fn name of the graph, copies included: a new copy takes a free one.
    pub taken: HashSet<String>,
    /// Remove the generic originals at the end (a single file); a graph sweeps them later
    /// (`sweep_generics`), once every module had its calls renamed.
    pub remove_originals: bool,
}

/// Single-file entry (`methods::prepare_program`).
pub fn monomorphize_program(prog: &mut Program) -> DiagResult<()> {
    let mut ctx = MonoCtx { remove_originals: true, ..Default::default() };
    monomorphize(prog, &mut ctx, &|p: &Program| {
        let mut env = crate::infer::type_env_of_program(p);
        env.consts = crate::consteval::eval_consts(p, &HashMap::new()).unwrap_or_default();
        Some(env)
    })?;
    Ok(())
}

/// Specializes the generic calls of `prog` (its own generics and `ctx.externals`). Returns the
/// copies made of its own generics, for `ctx.existing` of the modules that import it.
pub fn monomorphize(
    prog: &mut Program,
    ctx: &mut MonoCtx,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
) -> DiagResult<Vec<((String, Vec<String>), String)>> {
    let has_generic = prog
        .items
        .iter()
        .any(|i| matches!(i, Item::Fn(f) if !type_params_of(f).is_empty()));
    let names = called_names(prog);
    let calls_external = ctx.externals.keys().any(|g| names.contains(g));
    if !has_generic && !calls_external {
        return Ok(Vec::new());
    }
    // A program that does not type-check as written keeps its errors for the
    // regular inference, which reports them in order.
    let Some(env) = make_env(prog) else { return Ok(Vec::new()) };
    if infer_program_with_env(prog, &env).is_err() {
        return Ok(Vec::new());
    }
    let mut erased: HashSet<String> = HashSet::new();
    loop {
        let mut p = prog.clone();
        let mut taken = ctx.taken.clone();
        match run(&mut p, &erased, ctx, &mut taken, make_env)? {
            Outcome::Done(made) => {
                *prog = p;
                ctx.taken = taken;
                return Ok(made);
            }
            Outcome::Erase(g) => {
                if !erased.insert(g) {
                    return Ok(Vec::new());
                }
            }
            Outcome::Abort => return Ok(Vec::new()),
        }
    }
}

/// Lift callbacks passed to `fn(…)` parameters and make one callee copy per lambda.
/// The pass runs after generic instantiation, while the AST still carries lambdas.
pub fn specialize_lambdas_program(
    prog: &mut Program,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
) -> DiagResult<()> {
    specialize_lambdas_inner(prog, make_env, false)
}

pub(crate) fn specialize_lambdas_graph(
    prog: &mut Program,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
) -> DiagResult<()> {
    specialize_lambdas_inner(prog, make_env, true)
}

fn specialize_lambdas_inner(
    prog: &mut Program,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
    preserve_originals: bool,
) -> DiagResult<()> {
    let Some(env) = make_env(prog) else { return Ok(()) };
    let Ok(inferred) = infer_program_with_env(prog, &env) else { return Ok(()) };
    let entries: HashMap<String, Vec<crate::typed::ExprTypeEntry>> = inferred
        .expr_types_by_fn
        .into_iter()
        .map(|(f, es)| (f.name, es))
        .collect();
    let defs = env.fn_defs.clone();
    let mut calls = Vec::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            let mut local_lambdas = HashMap::new();
            let mut body = f.body.clone();
            walk_block(&mut body, &mut |_| {}, &mut |st| match st {
                Stmt::Let { name, init, .. } if matches!(init, Expr::Lambda { .. }) => {
                    local_lambdas.insert(name.clone(), init.clone());
                }
                Stmt::Assign { target: Expr::Ident { name, .. }, value, .. } if matches!(value, Expr::Lambda { .. }) => {
                    local_lambdas.insert(name.clone(), value.clone());
                }
                _ => {}
            });
            let mut body = f.body.clone();
            walk_block(&mut body, &mut |e| {
                let Expr::Call { func, args, span, .. } = e else { return };
                let Expr::Ident { name, .. } = func.as_ref() else { return };
                let Some(def) = defs.get(name) else { return };
                for (i, (arg, param)) in args.iter().zip(&def.params).enumerate() {
                    let callback = match arg {
                        Expr::Lambda { .. } => Some(arg.clone()),
                        Expr::Ident { name, .. } => local_lambdas.get(name).cloned(),
                        _ => None,
                    };
                    if let (Some(callback), Type::Fn { .. }) = (callback, &param.ty) {
                        calls.push((f.name.clone(), *span, name.clone(), i, callback));
                    }
                }
            }, &mut |_| {});
        }
    }
    let callback_fns: HashSet<String> = defs.iter().filter_map(|(name, f)| {
        f.params.iter().any(|p| matches!(p.ty, Type::Fn { .. })).then(|| name.clone())
    }).collect();
    if calls.is_empty() {
        if !preserve_originals {
            prog.items.retain(|item| !matches!(item, Item::Fn(f) if callback_fns.contains(&f.name)));
        }
        return Ok(())
    }

    // The call order is the source order, with a per-callee suffix just like generic copies.
    let mut used: HashSet<String> = prog.items.iter().filter_map(|i| match i {
        Item::Fn(f) => Some(f.name.clone()), _ => None,
    }).collect();
    let mut serials: HashMap<String, usize> = HashMap::new();
    let mut generated = Vec::new();
    let mut rewrites = Vec::new();
    for (caller, at, callee, callback_i, lambda) in calls {
        let Some(def) = defs.get(&callee) else { continue };
        let Some(Type::Fn { params: lambda_params, ret: lambda_ret, effects: declared_fx, .. }) =
            def.params.get(callback_i).map(|p| &p.ty) else { continue };
        let Expr::Lambda { params, body, span: lambda_span } = lambda else { continue };
        if params.len() != lambda_params.len() { continue }
        let local_entries = entries.get(&caller).cloned().unwrap_or_default();
        let captures = lambda_captures(&body, &params, &local_entries, &env);
        if let Some(at) = lambda_writes_capture(&body, &captures) {
            return Err(vec![crate::diag::Diagnostic::new(
                crate::diag::ErrorCode::EBorrow,
                "cannot assign to a captured value while it is shared-borrowed",
                at,
            )]);
        }
        let n = serials.entry(callee.clone()).or_default();
        let copy_name = fresh_specialized_name(&format!("{callee}__s{n}"), &mut used);
        *n += 1;
        let caller_n = serials.entry(format!("{caller}__lam")).or_default();
        let lambda_name = fresh_specialized_name(&format!("{caller}__lam{caller_n}"), &mut used);
        *caller_n += 1;

        let cap_params: Vec<Param> = captures.iter().map(|c| Param {
            name: c.param.clone(), ty: c.param_ty.clone(), mode: ParamMode::Inferred, span: c.span,
        }).collect();
        let mut lifted_body = (*body).clone();
        rename_captured_names(&mut lifted_body, &captures);
        let lambda_block = match lifted_body {
            Expr::Block(b) => b,
            e => Block { stmts: Vec::new(), tail: Some(Box::new(e)), span: lambda_span },
        };
        let helper_fx = if declared_fx.is_empty() {
            let mut effect_program = prog.clone();
            let mut effect_names: HashSet<String> = effect_program.items.iter().filter_map(|i| match i {
                Item::Fn(f) => Some(f.name.clone()), _ => None,
            }).collect();
            for f in env.fn_defs.values() {
                if effect_names.insert(f.name.clone()) { effect_program.items.push(Item::Fn(f.clone())); }
            }
            crate::effects::required_effects_for_block(&lambda_block, &[&effect_program])
        } else { declared_fx.clone() };
        let lifted = FnDef {
            name: lambda_name.clone(), receiver: None, self_mode: ParamMode::Inferred,
            self_ref: 0, method_kind: MethodKind::Method, type_params: Vec::new(),
            params: cap_params.iter().cloned().chain(params.iter().zip(lambda_params).map(|(name, ty)| Param {
                name: name.clone(), ty: ty.clone(), mode: ParamMode::Inferred, span: lambda_span,
            })).collect(),
            ret: (**lambda_ret).clone(), effects: helper_fx.clone(), body: lambda_block,
            no_heap: false, arena: false, deprecated: None, span: lambda_span,
        };

        let mut copy = def.clone();
        copy.name = copy_name.clone();
        let callback_name = copy.params[callback_i].name.clone();
        copy.params.remove(callback_i);
        copy.params.extend(cap_params.clone());
        replace_callback_calls(&mut copy.body, &callback_name, &lambda_name, &captures);
        for fx in &helper_fx {
            if !copy.effects.contains(fx) { copy.effects.push(fx.clone()); }
        }
        inline_forwarded_callbacks(&mut copy.body, &callback_name, &lambda_name, &captures, &defs, &mut copy.effects, &mut used, 0);

        let cap_args: Vec<Expr> = captures.iter().map(|c| {
            let id = Expr::Ident { name: c.source.clone(), span: c.span };
            if c.by_value { id } else { Expr::Unary { op: UnOp::Ref, expr: Box::new(id), span: c.span } }
        }).collect();
        rewrites.push((caller, at, callee, callback_i, copy_name, cap_args));
        generated.push((def.name.clone(), lifted));
        generated.push((def.name.clone(), copy));
    }

    for item in &mut prog.items {
        let Item::Fn(f) = item else { continue };
        let rs: Vec<_> = rewrites.iter().filter(|r| r.0 == f.name).cloned().collect();
        if rs.is_empty() { continue }
        walk_block(&mut f.body, &mut |e| {
            let Expr::Call { func, args, span, .. } = e else { return };
            let Expr::Ident { name, .. } = func.as_mut() else { return };
            if let Some((_, _, original, cb_i, specialized, captures)) = rs.iter().find(|r| r.1 == *span && r.2 == *name) {
                *name = specialized.clone();
                if *cb_i < args.len() { args.remove(*cb_i); }
                args.extend(captures.clone());
                let _ = original;
            }
        }, &mut |_| {});
    }
    // The unspecialized callback body is checked above but cannot be lowered: a function value
    // has no runtime representation in this language.
    if !preserve_originals {
        prog.items.retain(|item| !matches!(item, Item::Fn(f) if callback_fns.contains(&f.name)));
    }
    let stride = max_span_end(prog) as u64 + 1;
    for (i, (_, mut f)) in generated.into_iter().enumerate() {
        shift_spans(&mut f, span_base(stride, i as u64 + 1));
        prog.items.push(Item::Fn(f));
    }
    if let Some(final_env) = make_env(prog) {
        infer_program_with_env(prog, &final_env)?;
    }
    Ok(())
}

#[derive(Clone)]
struct LambdaCapture { source: String, param: String, param_ty: Type, by_value: bool, span: Span }

fn fresh_specialized_name(base: &str, used: &mut HashSet<String>) -> String {
    let mut name = base.to_string();
    while !used.insert(name.clone()) { name.push('_'); }
    name
}

fn lambda_captures(
    body: &Expr,
    params: &[String],
    entries: &[crate::typed::ExprTypeEntry],
    env: &TypeEnv,
) -> Vec<LambdaCapture> {
    let mut locals = HashSet::new();
    let mut callees = HashSet::new();
    let mut ordered = Vec::new();
    let mut copy = body.clone();
    walk_expr(&mut copy, &mut |e| {
        if let Expr::Call { func, .. } = e {
            if let Expr::Ident { name, .. } = func.as_ref() { callees.insert(name.clone()); }
        }
    }, &mut |s| {
        if let Stmt::Let { name, .. } = s { locals.insert(name.clone()); }
    });
    let mut seen = HashSet::new();
    let mut copy = body.clone();
    walk_expr(&mut copy, &mut |e| {
        if let Expr::Ident { name, span } = e {
            if seen.insert(name.clone()) { ordered.push((name.clone(), *span)); }
        }
    }, &mut |_| {});
    let mut out = Vec::new();
    for (name, span) in ordered {
        if params.contains(&name) || locals.contains(&name) || callees.contains(&name)
            || env.fns.contains_key(&name) || env.consts.contains_key(&name)
            || env.structs.contains_key(&name) || env.enums.contains_key(&name) { continue }
        let Some(ty) = entries.iter().find(|e| e.span_start == span.start && e.span_end == span.end).map(|e| e.ty.clone()) else { continue };
        let by_value = is_copy_capture(&ty, &env.structs);
        let param_ty = if by_value { ty } else { Type::Ref { mutable: false, lifetime: None, inner: Box::new(ty), span } };
        let n = out.len();
        out.push(LambdaCapture { source: name, param: format!("__cap{n}"), param_ty, by_value, span });
    }
    out
}

fn is_copy_capture(ty: &Type, structs: &HashMap<String, StructDef>) -> bool {
    match ty {
        Type::Named { name, args, .. } | Type::Qualified { name, args, .. } => {
            if matches!(name.as_str(), "Int" | "Float" | "Bool" | "Unit") { return true }
            structs.get(name).is_some_and(|s| s.fields.iter().all(|f| is_copy_capture(&f.ty, structs))) && args.is_empty()
        }
        Type::Ref { mutable, .. } => !mutable,
        Type::Tensor { .. } | Type::Fn { .. } => false,
    }
}

fn lambda_writes_capture(body: &Expr, captures: &[LambdaCapture]) -> Option<Span> {
    let names: HashSet<&str> = captures.iter().filter(|c| !c.by_value).map(|c| c.source.as_str()).collect();
    if names.is_empty() { return None }
    let mut found = None;
    let mut e = body.clone();
    walk_expr(&mut e, &mut |_| {}, &mut |st| {
        if let Stmt::Assign { target: Expr::Ident { name, span, .. }, .. } = st {
            if names.contains(name.as_str()) { found = Some(*span); }
        }
    });
    found
}

fn rename_captured_names(e: &mut Expr, captures: &[LambdaCapture]) {
    walk_expr(e, &mut |node| {
        if let Expr::Ident { name, .. } = node {
            if let Some(c) = captures.iter().find(|c| c.source == *name) { *name = c.param.clone(); }
        }
    }, &mut |_| {});
}

fn replace_callback_calls(b: &mut Block, callback: &str, lifted: &str, captures: &[LambdaCapture]) {
    walk_block(b, &mut |e| {
        let Expr::Call { func, args, span, .. } = e else { return };
        if !matches!(func.as_ref(), Expr::Ident { name, .. } if name == callback) { return }
        let mut lifted_args: Vec<Expr> = captures.iter().map(|c| Expr::Ident { name: c.param.clone(), span: c.span }).collect();
        lifted_args.extend(args.clone());
        *e = Expr::Call { func: Box::new(Expr::Ident { name: lifted.to_string(), span: *span }), type_args: Vec::new(), args: lifted_args, span: *span };
    }, &mut |_| {});
}

fn inline_forwarded_callbacks(
    body: &mut Block,
    callback: &str,
    lifted: &str,
    captures: &[LambdaCapture],
    defs: &HashMap<String, FnDef>,
    effects: &mut Vec<Effect>,
    used: &mut HashSet<String>,
    depth: usize,
) {
    if depth > 64 { return }
    walk_block(body, &mut |e| {
        let Expr::Call { func, args, span, .. } = e else { return };
        let Expr::Ident { name: callee, .. } = func.as_ref() else { return };
        let Some(def) = defs.get(callee) else { return };
        let forwarded = def.params.iter().enumerate().find(|(i, p)| {
            matches!(p.ty, Type::Fn { .. })
                && matches!(args.get(*i), Some(Expr::Ident { name, .. }) if name == callback)
        });
        let Some((callback_i, _)) = forwarded else { return };
        let replacement = inline_forwarded_call(
            def, callback_i, args, *span, lifted, captures, defs, effects, used, depth + 1,
        );
        *e = replacement;
    }, &mut |_| {});
}

fn inline_forwarded_call(
    def: &FnDef,
    callback_i: usize,
    args: &[Expr],
    call_span: Span,
    lifted: &str,
    captures: &[LambdaCapture],
    defs: &HashMap<String, FnDef>,
    effects: &mut Vec<Effect>,
    used: &mut HashSet<String>,
    depth: usize,
) -> Expr {
    for fx in &def.effects { if !effects.contains(fx) { effects.push(fx.clone()); } }
    let callback_name = def.params[callback_i].name.clone();
    let mut names = HashMap::new();
    let mut prefix = Vec::new();
    for (i, param) in def.params.iter().enumerate() {
        if i == callback_i { continue }
        let Some(arg) = args.get(i) else { continue };
        let local = fresh_specialized_name(&format!("__forward{i}"), used);
        names.insert(param.name.clone(), local.clone());
        prefix.push(Stmt::Let {
            name: local,
            ty: None,
            init: arg.clone(),
            frozen: false,
            stack: false,
            span: call_span,
        });
    }
    let mut block = def.body.clone();
    rename_forwarded_params(&mut block, &names);
    replace_callback_calls(&mut block, &callback_name, lifted, captures);
    inline_forwarded_callbacks(&mut block, &callback_name, lifted, captures, defs, effects, used, depth);
    prefix.extend(block.stmts);
    block.stmts = prefix;
    block.span = call_span;
    Expr::Block(block)
}

fn rename_forwarded_params(block: &mut Block, names: &HashMap<String, String>) {
    walk_block(block, &mut |e| {
        if let Expr::Ident { name, .. } = e {
            if let Some(replacement) = names.get(name) { *name = replacement.clone(); }
        }
    }, &mut |_| {});
}

enum Outcome {
    /// The copies made of this module's own generics.
    Done(Vec<((String, Vec<String>), String)>),
    /// This generic has a call that cannot be specialized: start over with it erased.
    Erase(String),
    Abort,
}

fn run(
    p: &mut Program,
    erased: &HashSet<String>,
    ctx: &MonoCtx,
    taken: &mut HashSet<String>,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
) -> DiagResult<Outcome> {
    let own: HashMap<String, FnDef> = p
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) if !type_params_of(f).is_empty() && !erased.contains(&f.name) => {
                Some((f.name.clone(), f.clone()))
            }
            _ => None,
        })
        .collect();
    let mut generics = own.clone();
    for (n, f) in &ctx.externals {
        if !erased.contains(n) && !generics.contains_key(n) {
            generics.insert(n.clone(), f.clone());
        }
    }
    if generics.is_empty() {
        return Ok(Outcome::Done(Vec::new()));
    }
    let mut copies: HashMap<(String, Vec<String>), String> = HashMap::new();
    let mut made: Vec<((String, Vec<String>), String)> = Vec::new();
    for i in &p.items {
        if let Item::Fn(f) = i {
            taken.insert(f.name.clone());
        }
    }
    // Each copy gets its own `start`/`end` range above the source (line and column, which
    // locate a diagnostic, stay): passes that key facts by span see one function per range.
    // An external generic's body comes from another file: the stride covers it too.
    let ext_end = ctx.externals.values().map(max_span_end_fn).max().unwrap_or(0);
    let stride = max_span_end(p).max(ext_end) as u64 + 1;
    let mut copy_count: u64 = 0;
    for _round in 0..256 {
        let Some(env) = make_env(p) else { return Ok(Outcome::Abort) };
        env.generic_calls.borrow_mut().clear();
        // A copy that does not type-check is an error of that instantiation.
        infer_program_with_env(p, &env)?;
        let calls = std::mem::take(&mut *env.generic_calls.borrow_mut());
        let mut keys: Vec<_> = calls.into_iter().collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        let mut renames: HashMap<String, Vec<(u32, u32, String, String)>> = HashMap::new();
        let mut new_fns: Vec<(String, FnDef)> = Vec::new();
        for ((caller, s, e), (callee, args)) in keys {
            let Some(g) = generics.get(&callee) else { continue };
            if own.contains_key(&caller) {
                // Inside a generic original, which goes away.
                continue;
            }
            let Some(args) = args else { return Ok(Outcome::Erase(callee)) };
            let key = (callee.clone(), args.iter().map(mangle_ty).collect::<Vec<_>>());
            let name = if let Some(n) = copies.get(&key) {
                n.clone()
            } else if let Some(n) = ctx.existing.get(&key).filter(|_| !own.contains_key(&callee)) {
                // The defining module already has this copy: call it.
                copies.insert(key, n.clone());
                n.clone()
            } else {
                let mut n = mangled_name(&callee, &args);
                while taken.contains(&n) {
                    n.push('_');
                }
                taken.insert(n.clone());
                copies.insert(key.clone(), n.clone());
                if own.contains_key(&callee) {
                    made.push((key, n.clone()));
                }
                let mut copy = instantiate(g, &n, &args);
                copy_count += 1;
                shift_spans(&mut copy, span_base(stride, copy_count));
                new_fns.push((callee.clone(), copy));
                n
            };
            renames.entry(caller).or_default().push((s, e, callee, name));
        }
        if renames.is_empty() && new_fns.is_empty() {
            if ctx.remove_originals {
                p.items.retain(|i| !matches!(i, Item::Fn(f) if own.contains_key(&f.name)));
            }
            return Ok(Outcome::Done(made));
        }
        for item in &mut p.items {
            if let Item::Fn(f) = item {
                if let Some(rs) = renames.get(&f.name) {
                    for (s, e, from, to) in rs {
                        rename_call_block(&mut f.body, (*s, *e), from, to);
                    }
                }
            }
        }
        // Each copy goes right after its generic, so the output order is stable.
        for (callee, nf) in new_fns {
            let prefix = format!("{callee}__");
            let pos = p
                .items
                .iter()
                .rposition(|i| matches!(i, Item::Fn(f) if f.name == callee || f.name.starts_with(&prefix)))
                .map(|i| i + 1)
                .unwrap_or(p.items.len());
            p.items.insert(pos, Item::Fn(nf));
        }
    }
    Ok(Outcome::Abort)
}

/// `f` with its type parameters replaced by `Unknown` inside the body.
pub fn erase_body(f: &FnDef) -> FnDef {
    let unknown = Type::Named { name: "Unknown".into(), args: vec![], span: Span::default() };
    let sub: HashMap<String, Type> =
        type_params_of(f).into_iter().map(|t| (t, unknown.clone())).collect();
    let mut g = f.clone();
    subst_block(&mut g.body, &sub);
    g
}

fn instantiate(g: &FnDef, name: &str, args: &[Type]) -> FnDef {
    let tps = type_params_of(g);
    let sub: HashMap<String, Type> = tps.into_iter().zip(args.iter().cloned()).collect();
    let mut f = g.clone();
    f.name = name.to_string();
    f.type_params.retain(|p| p.starts_with('\''));
    for p in &mut f.params {
        p.ty = subst(&p.ty, &sub);
    }
    f.ret = subst(&f.ret, &sub);
    subst_block(&mut f.body, &sub);
    f
}

pub fn subst(t: &Type, sub: &HashMap<String, Type>) -> Type {
    match t {
        Type::Named { name, args, span } => {
            if args.is_empty() {
                if let Some(r) = sub.get(name) {
                    return with_span(r, *span);
                }
            }
            Type::Named { name: name.clone(), args: args.iter().map(|a| subst(a, sub)).collect(), span: *span }
        }
        Type::Qualified { qual, name, args, span } => Type::Qualified {
            qual: qual.clone(),
            name: name.clone(),
            args: args.iter().map(|a| subst(a, sub)).collect(),
            span: *span,
        },
        Type::Ref { mutable, lifetime, inner, span } => Type::Ref {
            mutable: *mutable,
            lifetime: lifetime.clone(),
            inner: Box::new(subst(inner, sub)),
            span: *span,
        },
        Type::Fn { params, ret, effects, span } => Type::Fn {
            params: params.iter().map(|p| subst(p, sub)).collect(),
            ret: Box::new(subst(ret, sub)),
            effects: effects.clone(),
            span: *span,
        },
        other => other.clone(),
    }
}

fn with_span(t: &Type, span: Span) -> Type {
    match t {
        Type::Named { name, args, .. } => Type::Named { name: name.clone(), args: args.clone(), span },
        Type::Qualified { qual, name, args, .. } => {
            Type::Qualified { qual: qual.clone(), name: name.clone(), args: args.clone(), span }
        }
        Type::Ref { mutable, lifetime, inner, .. } => {
            Type::Ref { mutable: *mutable, lifetime: lifetime.clone(), inner: inner.clone(), span }
        }
        other => other.clone(),
    }
}

pub(crate) fn subst_block(b: &mut Block, sub: &HashMap<String, Type>) {
    walk_block(b, &mut |e| {
        if let Expr::Call { type_args, .. } = e {
            for ta in type_args.iter_mut() {
                if let TypeArg::Type(t) = ta {
                    *t = subst(t, sub);
                }
            }
        }
    }, &mut |st| {
        if let Stmt::Let { ty: Some(t), .. } = st {
            *t = subst(t, sub);
        }
    });
}

fn rename_call_block(b: &mut Block, at: (u32, u32), from: &str, to: &str) {
    walk_block(b, &mut |e| {
        if let Expr::Call { func, span, .. } = e {
            if (span.start, span.end) == at {
                if let Expr::Ident { name, .. } = &mut **func {
                    if name == from {
                        *name = to.to_string();
                    }
                }
            }
        }
    }, &mut |_| {});
}

/// Visits every expression (pre-order) and statement of `b`.
pub fn walk_block(b: &mut Block, fe: &mut dyn FnMut(&mut Expr), fs: &mut dyn FnMut(&mut Stmt)) {
    for st in &mut b.stmts {
        fs(st);
        match st {
            Stmt::Break { .. } => {}
            Stmt::Let { init, .. } => walk_expr(init, fe, fs),
            Stmt::Expr(e) => walk_expr(e, fe, fs),
            Stmt::Assign { target, value, .. } => {
                walk_expr(target, fe, fs);
                walk_expr(value, fe, fs);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    walk_expr(v, fe, fs);
                }
            }
            Stmt::While { cond, body, .. } => {
                walk_expr(cond, fe, fs);
                walk_block(body, fe, fs);
            }
        }
    }
    if let Some(t) = &mut b.tail {
        walk_expr(t, fe, fs);
    }
}

pub fn walk_expr(e: &mut Expr, fe: &mut dyn FnMut(&mut Expr), fs: &mut dyn FnMut(&mut Stmt)) {
    fe(e);
    match e {
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } | Expr::Ident { .. } => {}
        Expr::Call { func, args, .. } => {
            walk_expr(func, fe, fs);
            for a in args {
                walk_expr(a, fe, fs);
            }
        }
        Expr::Lambda { body, .. } => walk_expr(body, fe, fs),
        Expr::Binary { left, right, .. } => {
            walk_expr(left, fe, fs);
            walk_expr(right, fe, fs);
        }
        Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } => walk_expr(expr, fe, fs),
        Expr::Block(b) => walk_block(b, fe, fs),
        Expr::On { body, .. } => walk_block(body, fe, fs),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    walk_expr(c, fe, fs);
                }
            }
        }
        Expr::Field { base, .. } => walk_expr(base, fe, fs),
        Expr::Match { scrutinee, arms, .. } => {
            walk_expr(scrutinee, fe, fs);
            for a in arms {
                walk_expr(&mut a.body, fe, fs);
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            walk_expr(cond, fe, fs);
            walk_block(then_block, fe, fs);
            for ei in elsifs {
                walk_expr(&mut ei.cond, fe, fs);
                walk_block(&mut ei.body, fe, fs);
            }
            if let Some(eb) = else_block {
                walk_block(eb, fe, fs);
            }
        }
    }
}

/// Names called directly (`f(…)`) anywhere in `prog`.
pub fn called_names(prog: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut p = prog.clone();
    for item in &mut p.items {
        if let Item::Fn(f) = item {
            walk_block(&mut f.body, &mut |e| {
                if let Expr::Call { func, .. } = e {
                    if let Expr::Ident { name, .. } = &**func {
                        out.insert(name.clone());
                    }
                }
            }, &mut |_| {});
        }
    }
    out
}

fn span_base(stride: u64, k: u64) -> u64 {
    ((1u64 << 31) + k * stride) % (1u64 << 32)
}

pub(crate) fn max_span_end(p: &Program) -> u32 {
    fn go(v: &serde_json::Value, m: &mut u64) {
        match v {
            serde_json::Value::Object(o) => {
                if is_span(o) {
                    if let Some(e) = o.get("end").and_then(|x| x.as_u64()) {
                        *m = (*m).max(e);
                    }
                }
                for x in o.values() {
                    go(x, m);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| go(x, m)),
            _ => {}
        }
    }
    let mut m = 0u64;
    if let Ok(v) = serde_json::to_value(p) {
        go(&v, &mut m);
    }
    m.min(u32::MAX as u64 / 2) as u32
}

fn max_span_end_fn(f: &FnDef) -> u32 {
    let p = Program { schema_version: 0, items: vec![Item::Fn(f.clone())], type_defs: vec![] };
    max_span_end(&p)
}

/// Removes the generic originals that no module of the graph calls any more. Repeats until
/// nothing changes: a removed original may have been the last caller of another one.
pub fn sweep_generics(progs: &mut [&mut Program]) {
    loop {
        let mut called: HashSet<String> = HashSet::new();
        for p in progs.iter() {
            called.extend(called_names(p));
        }
        let mut removed = false;
        for p in progs.iter_mut() {
            let before = p.items.len();
            p.items.retain(|i| !matches!(i, Item::Fn(f) if !type_params_of(f).is_empty() && !called.contains(&f.name)));
            removed |= p.items.len() != before;
        }
        if !removed {
            return;
        }
    }
}

fn is_span(o: &serde_json::Map<String, serde_json::Value>) -> bool {
    o.len() == 4 && o.contains_key("start") && o.contains_key("end") && o.contains_key("line") && o.contains_key("col")
}

/// Adds `delta` to `start` and `end` of every span of `f` (modulo 2^32).
pub(crate) fn shift_spans(f: &mut FnDef, delta: u64) {
    fn go(v: &mut serde_json::Value, d: u64) {
        match v {
            serde_json::Value::Object(o) => {
                if is_span(o) {
                    for k in ["start", "end"] {
                        if let Some(x) = o.get(k).and_then(|x| x.as_u64()) {
                            o.insert(k.into(), serde_json::Value::from((x + d) % (1u64 << 32)));
                        }
                    }
                    return;
                }
                for x in o.values_mut() {
                    go(x, d);
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| go(x, d)),
            _ => {}
        }
    }
    let Ok(mut v) = serde_json::to_value(&*f) else { return };
    go(&mut v, delta);
    if let Ok(g) = serde_json::from_value(v) {
        *f = g;
    }
}

// ---- generic structs ----

thread_local! {
    /// Instances of generic structs made in this compilation: (struct, mangled types) → instance.
    /// Inference treats `G[tipos]` and its instance as one type (`canonical_instance`).
    static INSTANCES: std::cell::RefCell<HashMap<(String, Vec<String>), String>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Forgets the instances of a previous compilation.
pub fn reset_instances() {
    INSTANCES.with(|m| m.borrow_mut().clear());
}

/// The instance type of `G[tipos]`, when one was made (`Box[String]` → `Box__String`).
pub fn canonical_instance(t: &Type) -> Option<Type> {
    let Type::Named { name, args, span } = t else { return None };
    if args.is_empty() {
        return None;
    }
    let key = (name.clone(), args.iter().map(mangle_ty).collect::<Vec<_>>());
    INSTANCES
        .with(|m| m.borrow().get(&key).cloned())
        .map(|n| Type::Named { name: n, args: vec![], span: *span })
}

/// What one module's struct monomorphization sees of the rest of the graph (`MonoCtx` for structs).
#[derive(Default)]
pub struct StructCtx {
    /// Generic structs of the other modules, by name.
    pub externals: HashMap<String, StructDef>,
    /// Instances already made in the module that defines their generic: (struct, mangled types) →
    /// instance name.
    pub existing: HashMap<(String, Vec<String>), String>,
    /// Every type name of the graph, instances included.
    pub taken: HashSet<String>,
    /// Remove the generic structs nobody names any more (a single file); a graph sweeps later.
    pub remove_originals: bool,
}

/// Single-file entry, after `monomorphize_program`.
pub fn monomorphize_structs_program(prog: &mut Program) -> DiagResult<()> {
    reset_instances();
    let mut ctx = StructCtx { remove_originals: true, ..Default::default() };
    monomorphize_structs(prog, &mut ctx, &|p: &Program| {
        let mut env = crate::infer::type_env_of_program(p);
        env.consts = crate::consteval::eval_consts(p, &HashMap::new()).unwrap_or_default();
        Some(env)
    })?;
    Ok(())
}

/// Every `G[tipos concretos]` of a generic struct `G` becomes the struct `G__tipos` (SPEC
/// «Genéricos»), and `G(..)` built with those types calls its constructor. Runs after the
/// functions are monomorphized, so a copy's types are already concrete. Returns the instances
/// made of this module's own generic structs.
pub fn monomorphize_structs(
    prog: &mut Program,
    ctx: &mut StructCtx,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
) -> DiagResult<Vec<((String, Vec<String>), String)>> {
    let own: HashMap<String, StructDef> = prog
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Struct(s) if !s.type_params.is_empty() => Some((s.name.clone(), s.clone())),
            _ => None,
        })
        .collect();
    let mut generics = own.clone();
    for (n, s) in &ctx.externals {
        generics.entry(n.clone()).or_insert_with(|| s.clone());
    }
    if generics.is_empty() {
        return Ok(Vec::new());
    }
    let Some(env) = make_env(prog) else { return Ok(Vec::new()) };
    env.generic_ctors.borrow_mut().clear();
    if infer_program_with_env(prog, &env).is_err() {
        return Ok(Vec::new());
    }
    let ctors = std::mem::take(&mut *env.generic_ctors.borrow_mut());
    for i in &prog.items {
        match i {
            Item::Struct(s) => {
                ctx.taken.insert(s.name.clone());
            }
            Item::Enum(e) => {
                ctx.taken.insert(e.name.clone());
            }
            _ => {}
        }
    }
    let mut inst = Instancer { generics: &generics, own: &own, ctx, made: Vec::new(), new_structs: Vec::new(), names: HashMap::new() };
    // The constructions, in a stable order (fn, then span).
    let mut keys: Vec<_> = ctors.into_iter().collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    let mut renames: HashMap<String, Vec<(u32, u32, String, String)>> = HashMap::new();
    for ((caller, s, e), (g, args)) in keys {
        let Some(args) = args else { continue };
        if !generics.contains_key(&g) {
            continue;
        }
        let name = inst.instance(&g, &args);
        renames.entry(caller).or_default().push((s, e, g, name));
    }
    for item in &mut prog.items {
        match item {
            Item::Fn(f) if type_params_of(f).is_empty() => {
                for p in &mut f.params {
                    p.ty = inst.rewrite(&p.ty);
                }
                f.ret = inst.rewrite(&f.ret);
                if let Some(rs) = renames.get(&f.name) {
                    for (s, e, from, to) in rs {
                        rename_call_block(&mut f.body, (*s, *e), from, to);
                    }
                }
                let mut body = std::mem::replace(&mut f.body, Block { stmts: vec![], tail: None, span: f.span });
                rewrite_block_types(&mut body, &mut inst);
                f.body = body;
            }
            Item::Struct(s) if s.type_params.is_empty() => {
                for fld in &mut s.fields {
                    fld.ty = inst.rewrite(&fld.ty);
                }
            }
            Item::Enum(en) if en.type_params.is_empty() => {
                for v in &mut en.variants {
                    for t in &mut v.fields {
                        *t = inst.rewrite(t);
                    }
                }
            }
            Item::Const(c) => {
                if let Some(t) = &c.ty {
                    c.ty = Some(inst.rewrite(t));
                }
            }
            _ => {}
        }
    }
    let new_structs = std::mem::take(&mut inst.new_structs);
    let made = std::mem::take(&mut inst.made);
    // Each instance goes right after its generic and the instances of it made before.
    for (g, sd) in new_structs {
        let prefix = format!("{g}__");
        let pos = prog
            .items
            .iter()
            .rposition(|i| matches!(i, Item::Struct(s) if s.name == g || s.name.starts_with(&prefix)))
            .map(|i| i + 1)
            .unwrap_or(prog.items.len());
        prog.items.insert(pos, Item::Struct(sd));
    }
    if ctx.remove_originals {
        sweep_generic_structs(&mut [prog]);
    }
    Ok(made)
}

struct Instancer<'a> {
    generics: &'a HashMap<String, StructDef>,
    own: &'a HashMap<String, StructDef>,
    ctx: &'a mut StructCtx,
    made: Vec<((String, Vec<String>), String)>,
    new_structs: Vec<(String, StructDef)>,
    names: HashMap<(String, Vec<String>), String>,
}

impl Instancer<'_> {
    /// The instance of `g` for `args` (concrete), made when it does not exist yet.
    fn instance(&mut self, g: &str, args: &[Type]) -> String {
        let key = (g.to_string(), args.iter().map(mangle_ty).collect::<Vec<_>>());
        if let Some(n) = self.names.get(&key) {
            return n.clone();
        }
        if !self.own.contains_key(g) {
            if let Some(n) = self.ctx.existing.get(&key) {
                INSTANCES.with(|m| m.borrow_mut().insert(key.clone(), n.clone()));
                self.names.insert(key, n.clone());
                return n.clone();
            }
        }
        let mut n = mangled_name(g, args);
        while self.ctx.taken.contains(&n) {
            n.push('_');
        }
        self.ctx.taken.insert(n.clone());
        self.names.insert(key.clone(), n.clone());
        INSTANCES.with(|m| m.borrow_mut().insert(key.clone(), n.clone()));
        if self.own.contains_key(g) {
            self.made.push((key, n.clone()));
        }
        let def = self.generics[g].clone();
        let sub: HashMap<String, Type> = def.type_params.iter().cloned().zip(args.iter().cloned()).collect();
        let mut sd = def.clone();
        sd.name = n.clone();
        sd.type_params.clear();
        let fields: Vec<Type> = def.fields.iter().map(|f| subst(&f.ty, &sub)).collect();
        for (fld, t) in sd.fields.iter_mut().zip(fields) {
            fld.ty = self.rewrite(&t);
        }
        self.new_structs.push((g.to_string(), sd));
        n
    }

    /// `t` with every `G[tipos concretos]` replaced by its instance.
    fn rewrite(&mut self, t: &Type) -> Type {
        match t {
            Type::Named { name, args, span } => {
                if !args.is_empty() && self.generics.contains_key(name) && args.iter().all(is_concrete) {
                    let n = self.instance(name, args);
                    return Type::Named { name: n, args: vec![], span: *span };
                }
                Type::Named { name: name.clone(), args: args.iter().map(|a| self.rewrite(a)).collect(), span: *span }
            }
            Type::Qualified { qual, name, args, span } => Type::Qualified {
                qual: qual.clone(),
                name: name.clone(),
                args: args.iter().map(|a| self.rewrite(a)).collect(),
                span: *span,
            },
            Type::Ref { mutable, lifetime, inner, span } => Type::Ref {
                mutable: *mutable,
                lifetime: lifetime.clone(),
                inner: Box::new(self.rewrite(inner)),
                span: *span,
            },
            Type::Fn { params, ret, effects, span } => Type::Fn {
                params: params.iter().map(|p| self.rewrite(p)).collect(),
                ret: Box::new(self.rewrite(ret)),
                effects: effects.clone(),
                span: *span,
            },
            other => other.clone(),
        }
    }
}

fn rewrite_block_types(b: &mut Block, inst: &mut Instancer<'_>) {
    let cell = std::cell::RefCell::new(inst);
    walk_block(
        b,
        &mut |e| {
            if let Expr::Call { type_args, .. } = e {
                for ta in type_args.iter_mut() {
                    if let TypeArg::Type(t) = ta {
                        *t = cell.borrow_mut().rewrite(t);
                    }
                }
            }
        },
        &mut |st| {
            if let Stmt::Let { ty: Some(t), .. } = st {
                *t = cell.borrow_mut().rewrite(t);
            }
        },
    );
}

/// Removes the generic structs that no type of the graph names any more.
pub fn sweep_generic_structs(progs: &mut [&mut Program]) {
    let mut named: HashSet<String> = HashSet::new();
    for p in progs.iter() {
        if let Ok(v) = serde_json::to_value(&**p) {
            collect_type_names(&v, &mut named);
        }
    }
    for p in progs.iter_mut() {
        p.items.retain(|i| !matches!(i, Item::Struct(s) if !s.type_params.is_empty() && !named.contains(&s.name)));
    }
}

/// Every name a `Named` or `Qualified` type mentions, outside the generic struct definitions
/// themselves, plus every called name (a constructor names its struct).
fn collect_type_names(v: &serde_json::Value, out: &mut HashSet<String>) {
    match v {
        serde_json::Value::Object(o) => {
            // A generic struct's own definition does not keep it alive.
            if let Some(serde_json::Value::Object(s)) = o.get("Struct") {
                if s.get("type_params").and_then(|t| t.as_array()).is_some_and(|a| !a.is_empty()) {
                    return;
                }
            }
            for key in ["Named", "Qualified"] {
                if let Some(serde_json::Value::Object(n)) = o.get(key) {
                    if let Some(name) = n.get("name").and_then(|x| x.as_str()) {
                        out.insert(name.to_string());
                    }
                }
            }
            if let Some(serde_json::Value::Object(c)) = o.get("Call") {
                if let Some(serde_json::Value::Object(f)) = c.get("func") {
                    if let Some(serde_json::Value::Object(id)) = f.get("Ident") {
                        if let Some(name) = id.get("name").and_then(|x| x.as_str()) {
                            out.insert(name.to_string());
                        }
                    }
                }
            }
            for x in o.values() {
                collect_type_names(x, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_type_names(x, out)),
        _ => {}
    }
}
