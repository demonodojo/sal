//! Scope-end `free` insertion for owned `String` locals.
//!
//! Runs on the checked AST right before lowering. A local is dropped when it
//! is bound once (`let` or first assignment) to a fresh heap string, is never reassigned (except
//! the in-place `x = x + …`) and never escapes: it is only read by borrowing
//! builtins and by `+`. Everything else keeps the old behaviour (no free), so
//! handle-typed code (`str_as_int`, `vec_push`, user calls) stays sound.
//!
//! Frees go at the end of the block that declares the local and before every
//! `return` in its scope. A block value is bound to a temporary first so the
//! free runs after the value is computed.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::span::Span;
use crate::string_expr::add_chain_leftmost;

/// Variant name -> per-field "may own heap memory" flags (a field typed `Int`,
/// `Float` or `Bool` is Copy; anything else, including a type parameter, may not be).
type Variants = HashMap<String, Vec<bool>>;

pub fn insert_drops(prog: &Program) -> Program {
    let mut variants = Variants::new();
    for item in &prog.items {
        if let Item::Enum(e) = item {
            for v in &e.variants {
                let owned = v.fields.iter().map(|t| !is_copy_scalar(t)).collect();
                variants.insert(v.name.clone(), owned);
            }
        }
    }
    let mut out = prog.clone();
    for item in &mut out.items {
        if let Item::Fn(f) = item {
            drop_fn(f, &variants);
        }
    }
    out
}

/// Builtins whose `String` arguments are only read.
fn borrows_args(name: &str) -> bool {
    matches!(
        name,
        "str_len"
            | "str_eq"
            | "str_char"
            | "str_contains"
            | "str_skip"
            | "str_hash"
            | "str_slice"
            | "str_concat"
            | "str_to_f64_bits"
            | "print_str"
            | "eprint_str"
            | "write_file"
            | "mkdir_p"
            | "path_readable"
            | "getenv"
            | "map_get"
            | "read_file"
            | "list_len"
            | "list_get"
            | "list_pop"
            | "dict_get"
            | "dict_has"
            | "bytes_len"
            | "bytes_get_u8"
            | "bytes_get_u16"
            | "bytes_get_u32"
            | "bytes_get_i64"
            | "bytes_set_u8"
            | "bytes_set_u16"
            | "bytes_set_u32"
            | "bytes_set_i64"
            | "bytes_fill"
            | "bytes_fill_u32"
            | "bytes_copy"
            | "str_bytes_copy"
    )
}

/// Builtins that return a fresh heap string nobody else owns.
fn returns_fresh(name: &str) -> bool {
    matches!(
        name,
        "int_to_str" | "char_to_str" | "str_slice" | "strdup" | "read_file" | "str_concat"
    )
}

/// How a fresh local is released: `free` (String), `list_free`, `bytes_free`,
/// `dict_free`, or `enum_free` (the box of a variant with fields; `variants`
/// maps every variant name to whether it has fields).
///
/// The flag says the container owns its elements: it is only released when every
/// value stored in it is a fresh owned value (see `is_fresh_value`).
fn fresh_kind(init: &Expr, variants: &Variants) -> Option<(&'static str, bool)> {
    match init {
        Expr::String { .. } => Some(("free", false)),
        Expr::Call { func, type_args, .. } => {
            let Expr::Ident { name, .. } = func.as_ref() else {
                return None;
            };
            if returns_fresh(name) {
                Some(("free", false))
            } else if variants.get(name.as_str()).is_some_and(|f| !f.is_empty()) {
                Some(("enum_free", false))
            } else if name == "bytes_new" {
                Some(("bytes_free", false))
            } else if name == "list_new" && list_elem_args(type_args) {
                Some(("list_free", list_owns_elems(type_args)))
            } else if name == "dict_new" && dict_args(type_args) {
                Some(("dict_free", dict_owns_values(type_args)))
            } else {
                None
            }
        }
        Expr::Binary { op: BinOp::Add, .. } if add_is_string_chain(init) => Some(("free", false)),
        _ => None,
    }
}

fn list_owns_elems(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, .. })] if name == "String")
}

fn dict_owns_values(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [_, TypeArg::Type(Type::Named { name, .. })] if name == "String")
}

/// A value whose ownership is certainly the container's once stored: a string
/// literal, a fresh-string call or a `+` string chain. A name or a call to a
/// user function may be someone else's value, so storing it taints the container.
pub(crate) fn is_fresh_value(e: &Expr) -> bool {
    match e {
        Expr::String { .. } => true,
        Expr::Call { func, .. } => {
            matches!(func.as_ref(), Expr::Ident { name, .. } if returns_fresh(name))
        }
        Expr::Binary { op: BinOp::Add, .. } => add_is_string_chain(e),
        _ => false,
    }
}

/// `list_new[T]()` with `T` Int, Float, Bool or String (or no type argument,
/// which is Int). A String list owns its elements and `list_free` releases them.
fn list_elem_args(type_args: &[TypeArg]) -> bool {
    match type_args {
        [] => true,
        [TypeArg::Type(Type::Named { name, args, .. })] => {
            args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool" | "String")
        }
        _ => false,
    }
}

/// `dict_new[K, V]()` with `K` Int or String and `V` Int, Float, Bool or String.
fn dict_args(type_args: &[TypeArg]) -> bool {
    let scalar = |t: &TypeArg, names: &[&str]| match t {
        TypeArg::Type(Type::Named { name, args, .. }) => {
            args.is_empty() && names.contains(&name.as_str())
        }
        _ => false,
    };
    match type_args {
        [k, v] => scalar(k, &["Int", "String"]) && scalar(v, &["Int", "Float", "Bool", "String"]),
        _ => false,
    }
}

/// A `+` chain is a string concatenation (so its result is a fresh string) when
/// some operand is syntactically a string: a literal or a fresh-string call.
/// A string right operand forces a string left operand (`E_TYPE` otherwise).
fn add_is_string_chain(e: &Expr) -> bool {
    match e {
        Expr::Binary { op: BinOp::Add, left, right, .. } => {
            add_is_string_chain(left) || is_string_operand(right)
        }
        other => is_string_operand(other),
    }
}

fn is_string_operand(e: &Expr) -> bool {
    match e {
        Expr::String { .. } => true,
        Expr::Call { func, .. } => {
            matches!(func.as_ref(), Expr::Ident { name, .. } if returns_fresh(name))
        }
        _ => false,
    }
}

#[derive(Default)]
struct Facts {
    escaped: HashSet<String>,
    bindings: HashMap<String, u32>,
    variants: Variants,
    /// Containers that received a value not known to be fresh.
    tainted: HashSet<String>,
    /// `match x` over a local: the names its variant patterns bind.
    matches: Vec<(String, Vec<String>)>,
}

impl Facts {
    fn bind(&mut self, name: &str) {
        *self.bindings.entry(name.to_string()).or_insert(0) += 1;
    }
}

fn arg(a: &Expr, safe: bool, f: &mut Facts) {
    if let Expr::Ident { name, .. } = a {
        if !safe {
            f.escaped.insert(name.clone());
        }
    } else {
        expr(a, f);
    }
}

fn expr(e: &Expr, f: &mut Facts) {
    match e {
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } => {}
        Expr::Ident { name, .. } => {
            f.escaped.insert(name.clone());
        }
        Expr::Call { func, args, .. } => {
            let borrow = matches!(func.as_ref(), Expr::Ident { name, .. } if borrows_args(name));
            if !matches!(func.as_ref(), Expr::Ident { .. }) {
                expr(func, f);
            }
            for a in args {
                arg(a, borrow, f);
            }
        }
        Expr::Lambda { params, body, .. } => {
            for p in params {
                f.bind(p);
            }
            expr(body, f);
        }
        Expr::Binary { left, right, .. } => {
            arg(left, true, f);
            arg(right, true, f);
        }
        Expr::Unary { expr: x, .. } => arg(x, true, f),
        Expr::Block(b) => block(b, f),
        Expr::On { body, kernel_index, .. } => {
            if let Some(k) = kernel_index {
                for n in &k.names {
                    f.bind(n);
                }
            }
            block(body, f);
        }
        Expr::To { expr: x, .. } => arg(x, false, f),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    arg(c, false, f);
                }
            }
        }
        Expr::Field { base, .. } => arg(base, true, f),
        Expr::Match { scrutinee, arms, .. } => {
            // Variant patterns only read the box; a bare binding aliases the whole value.
            let binds_whole = arms.iter().any(|a| {
                matches!(&a.pattern, Pattern::Ident(n, _) if !f.variants.contains_key(n))
            });
            if let Expr::Ident { name, .. } = scrutinee.as_ref() {
                let mut bound = Vec::new();
                for a in arms {
                    bound_names(&a.pattern, &f.variants, &mut bound);
                }
                f.matches.push((name.clone(), bound));
            }
            arg(scrutinee, !binds_whole, f);
            for a in arms {
                pattern(&a.pattern, f);
                expr(&a.body, f);
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            arg(cond, true, f);
            block(then_block, f);
            for a in elsifs {
                arg(&a.cond, true, f);
                block(&a.body, f);
            }
            if let Some(b) = else_block {
                block(b, f);
            }
        }
        Expr::Try { expr: x, .. } => arg(x, false, f),
    }
}

fn bound_names(p: &Pattern, variants: &Variants, out: &mut Vec<String>) {
    if let Pattern::Variant { name, args, .. } = p {
        let owned = variants.get(name.as_str());
        for (i, a) in args.iter().enumerate() {
            match a {
                Pattern::Ident(n, _) => {
                    // A Copy field cannot alias heap memory; unknown variants stay conservative.
                    if owned.and_then(|o| o.get(i)).copied().unwrap_or(true) {
                        out.push(n.clone());
                    }
                }
                other => bound_names(other, variants, out),
            }
        }
    }
}

/// A payload bound by a pattern aliases a field of the box; if that name escapes,
/// the box can no longer be freed (nor the box of a scrutinee it came from).
fn propagate_payload_escapes(f: &mut Facts) {
    loop {
        let mut changed = false;
        for (scrut, bound) in &f.matches {
            if !f.escaped.contains(scrut) && bound.iter().any(|b| f.escaped.contains(b)) {
                f.escaped.insert(scrut.clone());
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

fn pattern(p: &Pattern, f: &mut Facts) {
    match p {
        Pattern::Wild(_) | Pattern::Int(_, _) => {}
        Pattern::Ident(n, _) => f.bind(n),
        Pattern::Variant { args, .. } => {
            for a in args {
                pattern(a, f);
            }
        }
    }
}

/// `list_push(xs, v)` (also `list_set`, `dict_put`) as a statement, or bound to
/// a `_name`, discards the returned alias of `xs`, so `xs` is only borrowed there.
fn push_stmt(e: &Expr, f: &mut Facts) -> bool {
    let Expr::Call { func, args, .. } = e else {
        return false;
    };
    if !matches!(func.as_ref(), Expr::Ident { name, .. } if matches!(name.as_str(), "list_push" | "list_set" | "dict_put"))
        || args.is_empty()
    {
        return false;
    }
    match &args[0] {
        Expr::Ident { .. } => {}
        inner => {
            if !push_stmt(inner, f) {
                expr(inner, f);
            }
        }
    }
    // The stored value is the last argument (`list_push(xs, v)`, `list_set(xs, i, v)`, `dict_put(d, k, v)`).
    if let (Expr::Ident { name: container, .. }, Some(v)) = (&args[0], args.last()) {
        if args.len() >= 2 && !is_fresh_value(v) {
            f.tainted.insert(container.clone());
        }
    }
    for a in &args[1..] {
        arg(a, false, f);
    }
    true
}

fn block(b: &Block, f: &mut Facts) {
    for st in &b.stmts {
        match st {
            Stmt::Let { name, init, .. } => {
                f.bind(name);
                arg(init, false, f);
            }
            Stmt::Expr(e) => {
                if !push_stmt(e, f) {
                    expr(e, f);
                }
            }
            Stmt::Assign { target, value, .. } => {
                if let Expr::Ident { name, .. } = target {
                    if !is_accumulate(name, value) {
                        f.bind(name);
                    }
                } else {
                    expr(target, f);
                }
                let discard = matches!(target, Expr::Ident { name, .. } if name.starts_with('_'));
                if !(discard && push_stmt(value, f)) {
                    arg(value, false, f);
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    arg(v, false, f);
                }
            }
            Stmt::While { cond, body, .. } => {
                arg(cond, true, f);
                block(body, f);
            }
        }
    }
    if let Some(t) = &b.tail {
        arg(t, false, f);
    }
}

/// `x = x + …`: appends in place, not a new binding.
fn is_accumulate(name: &str, value: &Expr) -> bool {
    matches!(value, Expr::Binary { op: BinOp::Add, .. })
        && matches!(add_chain_leftmost(value), Expr::Ident { name: l, .. } if l == name)
}

struct Ctx {
    facts: Facts,
    params: HashSet<String>,
    live: Vec<(String, &'static str)>,
    tmp: u32,
}

impl Ctx {
    /// The release function for a droppable local, if `name = init` declares one.
    fn candidate(&self, name: &str, ty: &Option<Type>, init: &Expr) -> Option<&'static str> {
        let (kind, owned) = fresh_kind(init, &self.facts.variants)?;
        if owned && self.facts.tainted.contains(name) {
            return None;
        }
        if let Some(t) = ty {
            if kind != "free" || !crate::string_expr::is_string_type(t) {
                return None;
            }
        }
        let ok = !self.params.contains(name)
            && self.facts.bindings.get(name) == Some(&1)
            && !self.facts.escaped.contains(name);
        ok.then_some(kind)
    }
}

fn is_copy_scalar(t: &Type) -> bool {
    matches!(t, Type::Named { name, args, .. } if args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool"))
}

fn drop_fn(f: &mut FnDef, variants: &Variants) {
    let mut facts = Facts { variants: variants.clone(), ..Facts::default() };
    for p in &f.params {
        facts.bind(&p.name);
    }
    block(&f.body, &mut facts);
    propagate_payload_escapes(&mut facts);
    let mut cx = Ctx {
        facts,
        params: f.params.iter().map(|p| p.name.clone()).collect(),
        live: Vec::new(),
        tmp: 0,
    };
    let span = f.body.span;
    let body = std::mem::replace(&mut f.body, Block { stmts: vec![], tail: None, span });
    f.body = xform_block(body, &mut cx);
}

fn free_call(name: &str, release: &str, span: Span) -> Stmt {
    Stmt::Expr(Expr::Call {
        func: Box::new(Expr::Ident { name: release.into(), span }),
        type_args: vec![],
        args: vec![Expr::Ident { name: name.into(), span }],
        span,
    })
}

fn xform_expr(e: Expr, cx: &mut Ctx) -> Expr {
    match e {
        Expr::If { cond, then_block, elsifs, else_block, span } => Expr::If {
            cond,
            then_block: xform_block(then_block, cx),
            elsifs: elsifs
                .into_iter()
                .map(|a| Elsif { cond: a.cond, body: xform_block(a.body, cx), span: a.span })
                .collect(),
            else_block: else_block.map(|b| xform_block(b, cx)),
            span,
        },
        other => other,
    }
}

fn xform_block(b: Block, cx: &mut Ctx) -> Block {
    let mark = cx.live.len();
    let mut stmts = Vec::new();
    let mut diverged = false;
    for st in b.stmts {
        if diverged {
            stmts.push(st);
            continue;
        }
        match st {
            Stmt::Let { name, ty, init, frozen, stack, span } => {
                let cand = cx.candidate(&name, &ty, &init);
                let init = xform_expr(init, cx);
                stmts.push(Stmt::Let { name: name.clone(), ty, init, frozen, stack, span });
                if let Some(k) = cand {
                    cx.live.push((name, k));
                }
            }
            Stmt::Assign { target, value, span } => {
                let decl = match &target {
                    Expr::Ident { name, .. } if !is_accumulate(name, &value) => {
                        cx.candidate(name, &None, &value).map(|k| (name.clone(), k))
                    }
                    _ => None,
                };
                let value = xform_expr(value, cx);
                stmts.push(Stmt::Assign { target, value, span });
                if let Some(d) = decl {
                    cx.live.push(d);
                }
            }
            Stmt::Expr(e) => stmts.push(Stmt::Expr(xform_expr(e, cx))),
            Stmt::While { cond, body, span } => {
                let body = xform_block(body, cx);
                stmts.push(Stmt::While { cond, body, span });
            }
            Stmt::Return { value, span } => {
                if cx.live.is_empty() {
                    stmts.push(Stmt::Return { value, span });
                } else {
                    let value = match value {
                        Some(v) => {
                            cx.tmp += 1;
                            let t = format!("__ret{}", cx.tmp);
                            stmts.push(Stmt::Let {
                                name: t.clone(),
                                ty: None,
                                init: v,
                                frozen: false,
                                stack: false,
                                span,
                            });
                            Some(Expr::Ident { name: t, span })
                        }
                        None => None,
                    };
                    for (n, k) in cx.live.iter().rev() {
                        stmts.push(free_call(n, k, span));
                    }
                    stmts.push(Stmt::Return { value, span });
                }
                diverged = true;
            }
        }
    }
    let mut tail = b.tail.map(|t| Box::new(xform_expr(*t, cx)));
    if !diverged {
        let own: Vec<(String, &'static str)> = cx.live[mark..].to_vec();
        if !own.is_empty() {
            if let Some(t) = tail.take() {
                cx.tmp += 1;
                let name = format!("__ret{}", cx.tmp);
                let span = b.span;
                stmts.push(Stmt::Let {
                    name: name.clone(),
                    ty: None,
                    init: *t,
                    frozen: false,
                    stack: false,
                    span,
                });
                for (n, k) in own.iter().rev() {
                    stmts.push(free_call(n, k, span));
                }
                tail = Some(Box::new(Expr::Ident { name, span }));
            } else {
                for (n, k) in own.iter().rev() {
                    stmts.push(free_call(n, k, b.span));
                }
            }
        }
    }
    cx.live.truncate(mark);
    Block { stmts, tail, span: b.span }
}
