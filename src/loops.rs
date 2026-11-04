//! `break`: leaves the innermost `while`.
//!
//! `break` is a statement of a loop body, or of an `if` / `elsif` / `else` block
//! nested in it. This pass runs before type checking (right after interpolation)
//! and rewrites it with a flag, so no later pass ever sees a `Stmt::Break`:
//!
//! ```text
//! while cond                     __brk1 = 0
//!     a                          while __brk1 == 0
//!     if c                           if cond
//!         break         ==>              a
//!     b                                  if c
//!                                            __brk1 = 1
//!                                        if __brk1 == 0
//!                                            b
//!                                    else
//!                                        __brk1 = 1
//! ```
//!
//! The flag is tested before `cond`, so a `break` never evaluates `cond` again.
//! A `break` outside a loop, in a position other than a statement of a loop
//! body or of its `if` blocks (a `match` arm, `on`, a lambda, a `let` value…),
//! or followed by more code in its block is `E_TYPE`.
use crate::ast::*;
use crate::diag::{DiagResult, Diagnostic, ErrorCode};
use crate::span::Span;

pub fn desugar_break(prog: &mut Program) -> DiagResult<()> {
    for item in &mut prog.items {
        if let Item::Fn(f) = item {
            let mut cx = Cx { next: 0 };
            block(&mut f.body, None, &mut cx)?;
        }
    }
    Ok(())
}

struct Cx {
    next: u32,
}

fn misplaced(span: Span) -> Vec<Diagnostic> {
    vec![Diagnostic::new(
        ErrorCode::EType,
        "`break` has to be a statement of a `while` body (or of an `if` block inside it)",
        span,
    )]
}

/// A synthetic span that cannot equal the span of a real node (those are never
/// empty), one per node type so the span-keyed type table stays unambiguous.
fn synth(at: u32, like: Span) -> Span {
    Span { start: at, end: at, line: like.line, col: like.col }
}

fn ident(name: &str, span: Span) -> Expr {
    Expr::Ident { name: name.to_string(), span }
}

fn int(value: i64, span: Span) -> Expr {
    Expr::Int { value, span }
}

/// `flag == 0`.
fn flag_clear(flag: &str, at: Span) -> Expr {
    Expr::Binary {
        op: BinOp::Eq,
        left: Box::new(ident(flag, synth(at.start + 1, at))),
        right: Box::new(int(0, synth(at.start + 1, at))),
        span: synth(at.start, at),
    }
}

fn set_flag(flag: &str, value: i64, span: Span) -> Stmt {
    Stmt::Assign { target: ident(flag, span), value: int(value, span), span }
}

fn if_stmt(cond: Expr, then_block: Block, else_block: Option<Block>, at: Span) -> Stmt {
    Stmt::Expr(Expr::If {
        cond: Box::new(cond),
        then_block,
        elsifs: Vec::new(),
        else_block,
        span: synth(at.end + 1, at),
    })
}

/// Rewrites `b`, whose enclosing loop (if any) uses `flag`. Returns whether a
/// `break` of that loop can run inside it.
fn block(b: &mut Block, flag: Option<&str>, cx: &mut Cx) -> DiagResult<bool> {
    let stmts = std::mem::take(&mut b.stmts);
    let mut tail = b.tail.take();
    let mut out: Vec<Stmt> = Vec::new();
    let mut iter = stmts.into_iter();
    let mut broke = false;

    while let Some(st) = iter.next() {
        let (sts, may_break, after_break) = stmt(st, flag, cx)?;
        out.extend(sts);
        if !may_break {
            continue;
        }
        broke = true;
        let rest_stmts: Vec<Stmt> = iter.by_ref().collect();
        if after_break {
            // A bare `break`: nothing may follow it.
            if let Some(next) = rest_stmts.first() {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "unreachable code after `break`",
                    stmt_span(next),
                )]);
            }
            if let Some(t) = &tail {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "unreachable code after `break`",
                    t.span(),
                )]);
            }
            break;
        }
        // An `if` that may break: what follows runs only if it did not.
        let flag = flag.expect("a break was lowered");
        if rest_stmts.is_empty() && tail.is_none() {
            break;
        }
        let at = last_span(&out);
        let mut rest = Block { stmts: rest_stmts, tail: tail.take(), span: b.span };
        // The tail of a guarded block is never a value (the `if` has no `else`).
        if let Some(t) = rest.tail.take() {
            rest.stmts.push(Stmt::Expr(*t));
        }
        block(&mut rest, Some(flag), cx)?;
        out.push(if_stmt(flag_clear(flag, at), rest, None, at));
        break;
    }

    if let Some(t) = tail.as_mut() {
        if !broke {
            expr(t, cx)?;
        }
    }
    b.stmts = out;
    b.tail = tail;
    Ok(broke)
}

fn last_span(out: &[Stmt]) -> Span {
    out.last().map(stmt_span).unwrap_or_default()
}

fn stmt_span(st: &Stmt) -> Span {
    match st {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Break { span }
        | Stmt::While { span, .. } => *span,
        Stmt::Expr(e) => e.span(),
    }
}

/// Rewrites one statement into one or more. The booleans say whether it can run
/// a `break` of the enclosing loop, and whether it is a bare `break`.
fn stmt(st: Stmt, flag: Option<&str>, cx: &mut Cx) -> DiagResult<(Vec<Stmt>, bool, bool)> {
    match st {
        Stmt::Break { span } => match flag {
            Some(f) => Ok((vec![set_flag(f, 1, span)], true, true)),
            None => Err(misplaced(span)),
        },
        Stmt::While { cond, body, span } => {
            let mut cond = cond;
            expr(&mut cond, cx)?;
            let mut body = body;
            cx.next += 1;
            let f = format!("__brk{}", cx.next);
            // The tail of a loop body is discarded: make it a statement, so a
            // trailing `if … break` is handled like any other.
            if let Some(t) = body.tail.take() {
                body.stmts.push(Stmt::Expr(*t));
            }
            if !block(&mut body, Some(&f), cx)? {
                return Ok((vec![Stmt::While { cond, body, span }], false, false));
            }
            // `let f = 0` + `while f == 0 { if cond { body } else { f = 1 } }`.
            let guarded = Block { stmts: vec![if_stmt(cond, body, Some(Block {
                stmts: vec![set_flag(&f, 1, synth(span.end + 2, span))],
                tail: None,
                span,
            }), span)], tail: None, span };
            let new_loop = Stmt::While { cond: flag_clear(&f, span), body: guarded, span };
            let init = Stmt::Let {
                name: f,
                ty: None,
                init: int(0, synth(span.start + 1, span)),
                frozen: false,
                stack: false,
                span,
            };
            Ok((vec![init, new_loop], false, false))
        }
        Stmt::Expr(Expr::If { cond, mut then_block, mut elsifs, mut else_block, span }) => {
            let mut cond = cond;
            expr(&mut cond, cx)?;
            let mut broke = false;
            broke |= block_in_if(&mut then_block, flag, cx)?;
            for arm in &mut elsifs {
                expr(&mut arm.cond, cx)?;
                broke |= block_in_if(&mut arm.body, flag, cx)?;
            }
            if let Some(eb) = else_block.as_mut() {
                broke |= block_in_if(eb, flag, cx)?;
            }
            if broke {
                // A statement `if` has no value: branch tails become statements
                // so every branch is `Unit`.
                unit_tail(&mut then_block);
                for arm in &mut elsifs {
                    unit_tail(&mut arm.body);
                }
                if let Some(eb) = else_block.as_mut() {
                    unit_tail(eb);
                }
            }
            Ok((vec![Stmt::Expr(Expr::If { cond, then_block, elsifs, else_block, span })], broke, false))
        }
        Stmt::Let { name, ty, mut init, frozen, stack, span } => {
            expr(&mut init, cx)?;
            Ok((vec![Stmt::Let { name, ty, init, frozen, stack, span }], false, false))
        }
        Stmt::Expr(mut e) => {
            expr(&mut e, cx)?;
            Ok((vec![Stmt::Expr(e)], false, false))
        }
        Stmt::Assign { mut target, mut value, span } => {
            expr(&mut target, cx)?;
            expr(&mut value, cx)?;
            Ok((vec![Stmt::Assign { target, value, span }], false, false))
        }
        Stmt::Return { mut value, span } => {
            if let Some(v) = value.as_mut() {
                expr(v, cx)?;
            }
            Ok((vec![Stmt::Return { value, span }], false, false))
        }
    }
}

fn block_in_if(b: &mut Block, flag: Option<&str>, cx: &mut Cx) -> DiagResult<bool> {
    // Only inside a loop does the branch carry its breaks; outside one the
    // plain walk reports a stray `break`.
    block(b, flag, cx)
}

fn unit_tail(b: &mut Block) {
    if let Some(t) = b.tail.take() {
        b.stmts.push(Stmt::Expr(*t));
    }
}

/// Walks an expression for loops nested in it. A `break` in here is never
/// valid: the blocks start with no enclosing loop.
fn expr(e: &mut Expr, cx: &mut Cx) -> DiagResult<()> {
    match e {
        Expr::Call { func, args, .. } => {
            expr(func, cx)?;
            for a in args {
                expr(a, cx)?;
            }
        }
        Expr::Field { base, .. } => expr(base, cx)?,
        Expr::Binary { left, right, .. } => {
            expr(left, cx)?;
            expr(right, cx)?;
        }
        Expr::Unary { expr: inner, .. } | Expr::To { expr: inner, .. } | Expr::Try { expr: inner, .. } => {
            expr(inner, cx)?
        }
        Expr::Block(b) => {
            block(b, None, cx)?;
        }
        Expr::On { body, .. } => {
            block(body, None, cx)?;
        }
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    expr(c, cx)?;
                }
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            expr(scrutinee, cx)?;
            for arm in arms {
                expr(&mut arm.body, cx)?;
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            expr(cond, cx)?;
            block(then_block, None, cx)?;
            for arm in elsifs {
                expr(&mut arm.cond, cx)?;
                block(&mut arm.body, None, cx)?;
            }
            if let Some(b) = else_block {
                block(b, None, cx)?;
            }
        }
        Expr::Lambda { body, .. } => expr(body, cx)?,
        _ => {}
    }
    Ok(())
}
