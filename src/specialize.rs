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
        _ => {}
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

/// Single-file entry (`methods::prepare_program`).
pub fn monomorphize_program(prog: &mut Program) -> DiagResult<()> {
    monomorphize(prog, &HashSet::new(), &|p: &Program| {
        let mut env = crate::infer::type_env_of_program(p);
        env.consts = crate::consteval::eval_consts(p, &HashMap::new()).unwrap_or_default();
        Some(env)
    })
}

/// `keep_erased`: generics that stay erased whatever their calls (called from
/// another module). `make_env` builds the type environment of `prog`.
pub fn monomorphize(
    prog: &mut Program,
    keep_erased: &HashSet<String>,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
) -> DiagResult<()> {
    let has_generic = prog
        .items
        .iter()
        .any(|i| matches!(i, Item::Fn(f) if !type_params_of(f).is_empty()));
    if !has_generic {
        return Ok(());
    }
    // A program that does not type-check as written keeps its errors for the
    // regular inference, which reports them in order.
    let Some(env) = make_env(prog) else { return Ok(()) };
    if infer_program_with_env(prog, &env).is_err() {
        return Ok(());
    }
    let mut erased = keep_erased.clone();
    loop {
        let mut p = prog.clone();
        match run(&mut p, &erased, make_env)? {
            Outcome::Done => {
                *prog = p;
                return Ok(());
            }
            Outcome::Erase(g) => {
                if !erased.insert(g) {
                    return Ok(());
                }
            }
            Outcome::Abort => return Ok(()),
        }
    }
}

enum Outcome {
    Done,
    /// This generic has a call that cannot be specialized: start over with it erased.
    Erase(String),
    Abort,
}

fn run(
    p: &mut Program,
    erased: &HashSet<String>,
    make_env: &dyn Fn(&Program) -> Option<TypeEnv>,
) -> DiagResult<Outcome> {
    let generics: HashMap<String, FnDef> = p
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) if !type_params_of(f).is_empty() && !erased.contains(&f.name) => {
                Some((f.name.clone(), f.clone()))
            }
            _ => None,
        })
        .collect();
    if generics.is_empty() {
        return Ok(Outcome::Done);
    }
    let mut copies: HashMap<(String, Vec<String>), String> = HashMap::new();
    let mut taken: HashSet<String> = p
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    // Each copy gets its own `start`/`end` range above the source (line and column, which
    // locate a diagnostic, stay): passes that key facts by span see one function per range.
    let stride = max_span_end(p) as u64 + 1;
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
            if generics.contains_key(&caller) {
                // Inside a generic original, which goes away.
                continue;
            }
            let Some(args) = args else { return Ok(Outcome::Erase(callee)) };
            let key = (callee.clone(), args.iter().map(mangle_ty).collect::<Vec<_>>());
            let name = match copies.get(&key) {
                Some(n) => n.clone(),
                None => {
                    let mut n = mangled_name(&callee, &args);
                    while taken.contains(&n) {
                        n.push('_');
                    }
                    taken.insert(n.clone());
                    copies.insert(key, n.clone());
                    let mut copy = instantiate(g, &n, &args);
                    copy_count += 1;
                    shift_spans(&mut copy, span_base(stride, copy_count));
                    new_fns.push((callee.clone(), copy));
                    n
                }
            };
            renames.entry(caller).or_default().push((s, e, callee, name));
        }
        if renames.is_empty() && new_fns.is_empty() {
            p.items.retain(|i| !matches!(i, Item::Fn(f) if generics.contains_key(&f.name)));
            return Ok(Outcome::Done);
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

fn subst_block(b: &mut Block, sub: &HashMap<String, Type>) {
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

fn max_span_end(p: &Program) -> u32 {
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

fn is_span(o: &serde_json::Map<String, serde_json::Value>) -> bool {
    o.len() == 4 && o.contains_key("start") && o.contains_key("end") && o.contains_key("line") && o.contains_key("col")
}

/// Adds `delta` to `start` and `end` of every span of `f` (modulo 2^32).
fn shift_spans(f: &mut FnDef, delta: u64) {
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
