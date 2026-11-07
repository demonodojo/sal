//! Compile-time values of module constants (SPEC «Constantes»).
//!
//! Runs after qualify, so a name is already the link name. Imported values are
//! passed in; this module only evaluates the constants defined in one program.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode};
use crate::span::Span;

/// A constant's value. `Bool` lowers as `ConstInt` 0/1, the same as a literal.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Bool(bool),
    Float(f64),
    Str(String),
}

impl ConstValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            ConstValue::Int(_) => "Int",
            ConstValue::Bool(_) => "Bool",
            ConstValue::Float(_) => "Float",
            ConstValue::Str(_) => "String",
        }
    }

    pub fn ty(&self, span: Span) -> Type {
        Type::Named {
            name: self.type_name().into(),
            args: vec![],
            span,
        }
    }
}

/// `^[A-Z][A-Z0-9_]*$`.
pub fn is_const_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {
            chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        }
        _ => false,
    }
}

/// The constants defined in `prog`. `visible` holds constants already evaluated
/// (imports, and nothing else). A name defined here shadows nothing: a clash
/// with `visible` is the caller's to reject.
pub fn eval_consts(
    prog: &Program,
    visible: &HashMap<String, ConstValue>,
) -> Result<HashMap<String, ConstValue>, Vec<Diagnostic>> {
    let mut defs: Vec<&ConstDef> = Vec::new();
    let mut seen: HashMap<&str, Span> = HashMap::new();
    for item in &prog.items {
        let Item::Const(c) = item else { continue };
        if seen.insert(c.name.as_str(), c.span).is_some() {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("name `{}` already defined", c.name),
                c.span,
            )]);
        }
        defs.push(c);
    }
    let local: HashSet<&str> = defs.iter().map(|c| c.name.as_str()).collect();
    let mut pending: Vec<&ConstDef> = defs;
    let mut resolved = visible.clone();
    let mut own: HashMap<String, ConstValue> = HashMap::new();
    while !pending.is_empty() {
        let mut later = Vec::new();
        let before = own.len();
        for c in pending {
            let waiting = const_deps(&c.value)
                .into_iter()
                .any(|(n, _)| local.contains(n.as_str()) && !own.contains_key(&n));
            if waiting {
                later.push(c);
                continue;
            }
            let value = eval_expr(&c.value, &resolved)?;
            if let Some(ann) = &c.ty {
                if !annotation_matches(ann, value.type_name()) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!(
                            "constant `{}` has type {}, annotation is {}",
                            c.name,
                            value.type_name(),
                            crate::infer::type_name(ann)
                        ),
                        c.span,
                    )]);
                }
            }
            resolved.insert(c.name.clone(), value.clone());
            own.insert(c.name.clone(), value);
        }
        if own.len() == before {
            let c = later[0];
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("constant cycle involving `{}`", c.name),
                c.span,
            )]);
        }
        pending = later;
    }
    Ok(own)
}

fn annotation_matches(ann: &Type, got: &str) -> bool {
    match ann {
        Type::Named { name, args, .. } if args.is_empty() => name == got,
        _ => false,
    }
}

fn const_deps(e: &Expr) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    walk_deps(e, &mut out);
    out
}

fn walk_deps(e: &Expr, out: &mut Vec<(String, Span)>) {
    match e {
        Expr::Ident { name, span } => out.push((name.clone(), *span)),
        Expr::Binary { left, right, .. } => {
            walk_deps(left, out);
            walk_deps(right, out);
        }
        Expr::Unary { expr, .. } => walk_deps(expr, out),
        _ => {}
    }
}

fn eval_expr(e: &Expr, env: &HashMap<String, ConstValue>) -> Result<ConstValue, Vec<Diagnostic>> {
    match e {
        Expr::Int { value, .. } => Ok(ConstValue::Int(*value)),
        Expr::Float { value, .. } => Ok(ConstValue::Float(*value)),
        Expr::Bool { value, .. } => Ok(ConstValue::Bool(*value)),
        Expr::String { value, parts, span } => {
            if !parts.is_empty() {
                return Err(not_const(*span));
            }
            Ok(ConstValue::Str(value.clone()))
        }
        Expr::Ident { name, span } => env.get(name).cloned().ok_or_else(|| not_const(*span)),
        Expr::Unary { op, expr, span } => {
            let v = eval_expr(expr, env)?;
            match op {
                UnOp::Neg => match v {
                    ConstValue::Int(n) => Ok(ConstValue::Int(n.wrapping_neg())),
                    ConstValue::Float(n) => Ok(ConstValue::Float(-n)),
                    _ => Err(not_const(*span)),
                },
                UnOp::Not => match v {
                    ConstValue::Bool(b) => Ok(ConstValue::Bool(!b)),
                    _ => Err(not_const(*span)),
                },
            }
        }
        Expr::Binary { op, left, right, span } => eval_bin(*op, left, right, *span, env),
        _ => Err(not_const(e.span())),
    }
}

fn eval_bin(
    op: BinOp,
    left: &Expr,
    right: &Expr,
    span: Span,
    env: &HashMap<String, ConstValue>,
) -> Result<ConstValue, Vec<Diagnostic>> {
    if matches!(op, BinOp::And | BinOp::Or) {
        let l = eval_expr(left, env)?;
        let lb = truth(&l).ok_or_else(|| not_const(span))?;
        if (op == BinOp::And && !lb) || (op == BinOp::Or && lb) {
            return Ok(ConstValue::Bool(lb));
        }
        let r = eval_expr(right, env)?;
        let rb = truth(&r).ok_or_else(|| not_const(span))?;
        return Ok(ConstValue::Bool(if op == BinOp::And { lb && rb } else { lb || rb }));
    }
    let l = eval_expr(left, env)?;
    let r = eval_expr(right, env)?;
    match op {
        BinOp::Add => match (&l, &r) {
            (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(a.wrapping_add(*b))),
            (ConstValue::Str(a), ConstValue::Str(b)) => Ok(ConstValue::Str(format!("{a}{b}"))),
            _ => Err(not_const(span)),
        },
        BinOp::Sub => ints(l, r, span, i64::wrapping_sub),
        BinOp::Mul => ints(l, r, span, i64::wrapping_mul),
        BinOp::Div => div(l, r, span, i64::wrapping_div),
        BinOp::Mod => div(l, r, span, i64::wrapping_rem),
        BinOp::Eq => cmp(l, r, span, |a, b| a == b),
        BinOp::Ne => cmp(l, r, span, |a, b| a != b),
        BinOp::Lt => cmp(l, r, span, |a, b| a < b),
        BinOp::Le => cmp(l, r, span, |a, b| a <= b),
        BinOp::Gt => cmp(l, r, span, |a, b| a > b),
        BinOp::Ge => cmp(l, r, span, |a, b| a >= b),
        _ => Err(not_const(span)),
    }
}

fn truth(v: &ConstValue) -> Option<bool> {
    match v {
        ConstValue::Bool(b) => Some(*b),
        ConstValue::Int(n) => Some(*n != 0),
        _ => None,
    }
}

fn ints(
    l: ConstValue,
    r: ConstValue,
    span: Span,
    f: fn(i64, i64) -> i64,
) -> Result<ConstValue, Vec<Diagnostic>> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(f(a, b))),
        _ => Err(not_const(span)),
    }
}

fn div(
    l: ConstValue,
    r: ConstValue,
    span: Span,
    f: fn(i64, i64) -> i64,
) -> Result<ConstValue, Vec<Diagnostic>> {
    match (l, r) {
        (ConstValue::Int(_), ConstValue::Int(0)) => Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "division by zero",
            span,
        )]),
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(f(a, b))),
        _ => Err(not_const(span)),
    }
}

fn cmp(
    l: ConstValue,
    r: ConstValue,
    span: Span,
    f: fn(i64, i64) -> bool,
) -> Result<ConstValue, Vec<Diagnostic>> {
    match (l, r) {
        (ConstValue::Int(a), ConstValue::Int(b)) => Ok(ConstValue::Int(if f(a, b) { 1 } else { 0 })),
        _ => Err(not_const(span)),
    }
}

fn not_const(span: Span) -> Vec<Diagnostic> {
    vec![Diagnostic::new(
        ErrorCode::EType,
        "not a constant expression",
        span,
    )]
}
