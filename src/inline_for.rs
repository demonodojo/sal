//! `inline for f in T.fields`: a loop over the fields of a struct, unrolled at compile time
//! (SPEC «Reflexión», `inline for`).
//!
//! This pass runs before type checking, before `desugar_break`, and replaces the statement with
//! one block per field of `T`, in declaration order. Inside the body of each copy, `f.name` is
//! the field name and `f.ty` the canonical text of its type (both `STRING` literals), and `x.[f]`
//! is the field itself (`x.name`): a place like any other field, so it reads, borrows and is
//! assigned with the rules of its real type. Each copy is the branch of an `if true`, so its
//! locals do not clash with those of the next field and are released at its end, and each copy is
//! typed with that field's type. A `break` in the body outside a `while` of its own is `E_TYPE`.
//!
//! Every copy gets spans of its own (the inferred types are kept per span): the copies of one
//! body would otherwise overwrite each other's types.
//!
//! `T` is a struct of this module without type parameters. Anything else (an enum, a generic
//! struct, a type of another module, `Self`), a bare `f`, `f.x` with `x` other than `name`/`ty`,
//! and `x.[g]` outside an `inline for` over `g` are `E_TYPE`. `inline for` in a protocol body is
//! `E_TYPE` too: that body is checked once, with an abstract `Self`.
use crate::ast::*;
use crate::diag::{DiagResult, Diagnostic, ErrorCode};
use crate::span::Span;

struct Cx {
    /// (struct name, fields as (name, type text)); `None` for a generic struct.
    structs: Vec<(String, Option<Vec<(String, String)>>)>,
    stride: u64,
    copies: u64,
}

fn err(msg: impl Into<String>, span: Span) -> Vec<Diagnostic> {
    vec![Diagnostic::new(ErrorCode::EType, msg, span)]
}

/// True when some function body of `prog` holds an `inline for` or an `x.[f]`.
pub fn has_inline_for(prog: &Program) -> bool {
    serde_json::to_string(prog)
        .map(|s| s.contains("\"InlineFor\"") || s.contains("\"field\":\"["))
        .unwrap_or(false)
}

pub fn unroll_inline_for(prog: &mut Program) -> DiagResult<()> {
    if !has_inline_for(prog) {
        return Ok(());
    }
    let structs = prog
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Struct(s) => Some((
                s.name.clone(),
                if s.type_params.is_empty() {
                    Some(s.fields.iter().map(|f| (f.name.clone(), crate::fmt::type_text(&f.ty))).collect())
                } else {
                    None
                },
            )),
            _ => None,
        })
        .collect();
    // Room for the synthetic spans of `loops.rs` (up to `end + 2`) inside each copy's range, and
    // for the two of the `if true` around it at offsets 0 and 1 of that range.
    let stride = crate::specialize::max_span_end(prog) as u64 + 3;
    let mut cx = Cx { structs, stride, copies: 0 };
    for item in &mut prog.items {
        match item {
            Item::Fn(f) => block(&mut f.body, &[], &mut cx)?,
            Item::Protocol(p) => {
                for m in &p.methods {
                    if let Some(body) = &m.body {
                        let v = serde_json::to_string(body).unwrap_or_default();
                        if v.contains("\"InlineFor\"") {
                            return Err(err("`inline for` is not allowed in a protocol body", m.span));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A loop variable in scope: its name and, once a copy is made, the field it stands for.
type Bound = (String, String, String);

fn block(b: &mut Block, vars: &[Bound], cx: &mut Cx) -> DiagResult<()> {
    let mut out = Vec::with_capacity(b.stmts.len());
    for st in std::mem::take(&mut b.stmts) {
        match st {
            Stmt::InlineFor { var, ty, body, span } => {
                if vars.iter().any(|(v, _, _)| *v == var) {
                    return Err(err(format!("`{var}` is already a loop variable here"), span));
                }
                let fields = match cx.structs.iter().find(|(n, _)| *n == ty) {
                    Some((_, Some(fs))) => fs.clone(),
                    Some((_, None)) => {
                        return Err(err(format!("`inline for` over the generic struct {ty}"), span));
                    }
                    None => {
                        return Err(err(
                            format!("`inline for` needs a struct of this module, found {ty}"),
                            span,
                        ));
                    }
                };
                if let Some(sp) = loose_break(&body) {
                    return Err(err("`break` in an `inline for` body (outside a `while` of its own)", sp));
                }
                for (fname, fty) in fields {
                    let mut copy = body.clone();
                    cx.copies += 1;
                    let base = (cx.copies * cx.stride) % (1u64 << 32);
                    shift_block(&mut copy, base);
                    let mut inner = vars.to_vec();
                    inner.push((var.clone(), fname, fty));
                    block(&mut copy, &inner, cx)?;
                    // `if true` around the copy: a scope of its own, whose locals are released
                    // at its end like those of any `if` branch.
                    let at = |o: u64| Span { start: (base + o) as u32, end: (base + o) as u32, ..copy.span };
                    let (if_sp, true_sp) = (at(0), at(1));
                    out.push(Stmt::Expr(Expr::If {
                        cond: Box::new(Expr::Bool { value: true, span: true_sp }),
                        then_block: copy,
                        elsifs: Vec::new(),
                        else_block: None,
                        span: if_sp,
                    }));
                }
            }
            mut st => {
                stmt(&mut st, vars, cx)?;
                out.push(st);
            }
        }
    }
    b.stmts = out;
    if let Some(t) = &mut b.tail {
        expr(t, vars, cx)?;
    }
    Ok(())
}

/// A `break` of the body that is not inside a `while` of the body itself (it would be ambiguous:
/// the `inline for` or the `while` around it).
fn loose_break(b: &Block) -> Option<Span> {
    b.stmts.iter().find_map(|st| match st {
        Stmt::Break { span } => Some(*span),
        Stmt::Expr(Expr::If { then_block, elsifs, else_block, .. }) => loose_break(then_block)
            .or_else(|| elsifs.iter().find_map(|a| loose_break(&a.body)))
            .or_else(|| else_block.as_ref().and_then(loose_break)),
        Stmt::Expr(Expr::Block(inner)) => loose_break(inner),
        Stmt::InlineFor { body, .. } => loose_break(body),
        _ => None,
    })
}

fn stmt(st: &mut Stmt, vars: &[Bound], cx: &mut Cx) -> DiagResult<()> {
    match st {
        Stmt::Let { init, .. } => expr(init, vars, cx),
        Stmt::Expr(e) => expr(e, vars, cx),
        Stmt::Assign { target, value, .. } => {
            expr(target, vars, cx)?;
            expr(value, vars, cx)
        }
        Stmt::Return { value, .. } => match value {
            Some(v) => expr(v, vars, cx),
            None => Ok(()),
        },
        Stmt::Break { .. } => Ok(()),
        Stmt::While { cond, body, .. } => {
            expr(cond, vars, cx)?;
            block(body, vars, cx)
        }
        Stmt::InlineFor { span, .. } => Err(err("internal: nested `inline for` not unrolled", *span)),
    }
}

fn expr(e: &mut Expr, vars: &[Bound], cx: &mut Cx) -> DiagResult<()> {
    // `f.name`, `f.ty` and `x.[f]` first: they replace the node.
    if let Expr::Field { base, field, span } = e {
        if let Expr::Ident { name, .. } = &**base {
            if let Some((_, fname, fty)) = vars.iter().rev().find(|(v, _, _)| v == name) {
                let value = match field.as_str() {
                    "name" => fname.clone(),
                    "ty" => fty.clone(),
                    other => {
                        return Err(err(
                            format!("a field of `inline for` has `.name` and `.ty`, not `.{other}`"),
                            *span,
                        ));
                    }
                };
                *e = Expr::String { value, parts: Vec::new(), span: *span };
                return Ok(());
            }
        }
        if let Some(v) = field.strip_prefix('[').and_then(|f| f.strip_suffix(']')) {
            let Some((_, fname, _)) = vars.iter().rev().find(|(n, _, _)| n == v) else {
                return Err(err(format!("`.[{v}]` outside an `inline for` over `{v}`"), *span));
            };
            *field = fname.clone();
        }
    }
    match e {
        Expr::Ident { name, span } => {
            if vars.iter().any(|(v, _, _)| v == name) {
                return Err(err(
                    format!("`{name}` is a field of `inline for`: use `{name}.name`, `{name}.ty` or `x.[{name}]`"),
                    *span,
                ));
            }
            Ok(())
        }
        Expr::Unit { .. } | Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } => Ok(()),
        Expr::Call { func, args, .. } => {
            expr(func, vars, cx)?;
            args.iter_mut().try_for_each(|a| expr(a, vars, cx))
        }
        Expr::Lambda { body, .. } => expr(body, vars, cx),
        Expr::Binary { left, right, .. } => {
            expr(left, vars, cx)?;
            expr(right, vars, cx)
        }
        Expr::Unary { expr: x, .. } | Expr::To { expr: x, .. } | Expr::Try { expr: x, .. } => expr(x, vars, cx),
        Expr::Block(b) | Expr::On { body: b, .. } => block(b, vars, cx),
        Expr::TensorLit { rows, .. } => rows.iter_mut().flatten().try_for_each(|x| expr(x, vars, cx)),
        Expr::Field { base, .. } => expr(base, vars, cx),
        Expr::Match { scrutinee, arms, .. } => {
            expr(scrutinee, vars, cx)?;
            arms.iter_mut().try_for_each(|a| expr(&mut a.body, vars, cx))
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            expr(cond, vars, cx)?;
            block(then_block, vars, cx)?;
            for arm in elsifs.iter_mut() {
                expr(&mut arm.cond, vars, cx)?;
                block(&mut arm.body, vars, cx)?;
            }
            match else_block {
                Some(b) => block(b, vars, cx),
                None => Ok(()),
            }
        }
    }
}

/// Adds `delta` to `start` and `end` of every span of `b` (modulo 2^32), like
/// `specialize::shift_spans` does for a whole function.
fn shift_block(b: &mut Block, delta: u64) {
    fn go(v: &mut serde_json::Value, d: u64) {
        match v {
            serde_json::Value::Object(o) => {
                let is_span = o.len() == 4
                    && ["start", "end", "line", "col"].iter().all(|k| o.contains_key(*k));
                if is_span {
                    for k in ["start", "end"] {
                        if let Some(x) = o.get(k).and_then(|x| x.as_u64()) {
                            o.insert(k.into(), serde_json::Value::from((x + d) % (1u64 << 32)));
                        }
                    }
                    return;
                }
                o.values_mut().for_each(|x| go(x, d));
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| go(x, d)),
            _ => {}
        }
    }
    let Ok(mut v) = serde_json::to_value(&*b) else { return };
    go(&mut v, delta);
    if let Ok(c) = serde_json::from_value(v) {
        *b = c;
    }
}
