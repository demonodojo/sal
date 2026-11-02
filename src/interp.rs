//! String interpolation: `"a{x}b"` is `"a" + x + "b"`.
//!
//! The parser keeps the structured `parts` on the literal (formatters need
//! them); this pass, run before type checking, replaces each interpolated
//! literal with a left-associated `+` chain that starts with a string literal.
//! That chain concatenates (copying `x`) and so needs the `alloc` effect, and
//! `x` has to be a `String` like any other right operand of `+`.
use crate::ast::*;
use crate::span::Span;

pub fn expand_interpolation(prog: &mut Program) {
    for item in &mut prog.items {
        if let Item::Fn(f) = item {
            block(&mut f.body);
        }
    }
}

fn block(b: &mut Block) {
    for st in &mut b.stmts {
        match st {
            Stmt::Let { init, .. } => expr(init),
            Stmt::Expr(e) => expr(e),
            Stmt::Assign { target, value, .. } => {
                expr(target);
                expr(value);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    expr(v);
                }
            }
            Stmt::While { cond, body, .. } => {
                expr(cond);
                block(body);
            }
        }
    }
    if let Some(t) = &mut b.tail {
        expr(t);
    }
}

fn expr(e: &mut Expr) {
    match e {
        Expr::String { parts, span, .. } if !parts.is_empty() => {
            let parts = std::mem::take(parts);
            *e = chain(parts, *span);
        }
        Expr::Call { func, args, .. } => {
            expr(func);
            for a in args {
                expr(a);
            }
        }
        Expr::Field { base, .. } => expr(base),
        Expr::Binary { left, right, .. } => {
            expr(left);
            expr(right);
        }
        Expr::Unary { expr: inner, .. }
        | Expr::To { expr: inner, .. }
        | Expr::Try { expr: inner, .. } => expr(inner),
        Expr::Block(b) => block(b),
        Expr::On { body, .. } => block(body),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    expr(c);
                }
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            expr(scrutinee);
            for arm in arms {
                expr(&mut arm.body);
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            expr(cond);
            block(then_block);
            for arm in elsifs {
                expr(&mut arm.cond);
                block(&mut arm.body);
            }
            if let Some(b) = else_block {
                block(b);
            }
        }
        Expr::Lambda { body, .. } => expr(body),
        _ => {}
    }
}

fn chain(parts: Vec<StringPart>, sp: Span) -> Expr {
    let lit = |value: String| Expr::String { value, parts: Vec::new(), span: sp };
    let add = |left: Expr, right: Expr| Expr::Binary {
        op: BinOp::Add,
        left: Box::new(left),
        right: Box::new(right),
        span: sp,
    };
    let mut acc: Option<Expr> = None;
    for part in parts {
        let next = match part {
            StringPart::Lit(t) => lit(t),
            StringPart::Interp(name) => Expr::Ident { name, span: sp },
        };
        acc = Some(match acc {
            Some(left) => add(left, next),
            // A chain must start with a literal so `+` is a concatenation
            // that copies, not an alias of the interpolated variable.
            None => match next {
                s @ Expr::String { .. } => s,
                other => add(lit(String::new()), other),
            },
        });
    }
    acc.expect("non-empty parts")
}

/// `Option` and `Result` are part of the language: a program that uses them
/// without declaring them gets the standard definitions appended (at the end,
/// so spans of the user's code do not move).
pub fn with_prelude_enums(src: &str) -> std::borrow::Cow<'_, str> {
    let mut extra = String::new();
    for (name, def) in [
        ("Result", "\nenum Result[T, E]\n    Ok(T)\n    Err(E)\n"),
        ("Option", "\nenum Option[T]\n    None\n    Some(T)\n"),
    ] {
        let uses = src.contains(&format!("{name}["));
        let declares = src.contains(&format!("enum {name}"));
        if uses && !declares {
            extra.push_str(def);
        }
    }
    if extra.is_empty() {
        std::borrow::Cow::Borrowed(src)
    } else {
        let mut out = src.trim_end_matches('\n').to_string();
        out.push('\n');
        out.push_str(&extra);
        std::borrow::Cow::Owned(out)
    }
}
