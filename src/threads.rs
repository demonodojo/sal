//! Threads (SPEC «Hilos»): `Thread.spawn(f, a1, …, an)`.
//!
//! This pass runs before type checking, next to `desugar_break`, and rewrites the call to
//! `thread_spawn(f(a1, …, an))`. The inner call is an ordinary call of `f`: every later pass
//! checks and moves the arguments exactly as for `f(a1, …, an)` (a by-value argument moves
//! into the thread, `&x` lends `x` to it), and the effects of `f` reach the caller. Only the
//! lowering knows that `thread_spawn` does not run the call but hands `f` and its arguments
//! to a new thread (`sal_thread_spawn`). The result is `Thread[R]`, with `R` the result of `f`.
//!
//! A module that defines its own `Thread` keeps its static `Thread.spawn`.
//! The first argument has to name a fn: anything else (a lambda, a local) is `E_TYPE`.
use crate::ast::*;
use crate::diag::{DiagResult, Diagnostic, ErrorCode};

pub fn desugar_thread_spawn(prog: &mut Program) -> DiagResult<()> {
    let own_thread = prog.items.iter().any(|it| match it {
        Item::Struct(s) => s.name == "Thread",
        Item::Enum(e) => e.name == "Thread",
        _ => false,
    });
    if own_thread {
        return Ok(());
    }
    let mut errs = Vec::new();
    for item in &mut prog.items {
        if let Item::Fn(f) = item {
            crate::specialize::walk_block(
                &mut f.body,
                &mut |e: &mut Expr| rewrite(e, &mut errs),
                &mut |_s: &mut Stmt| {},
            );
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

fn is_thread_spawn(func: &Expr) -> bool {
    matches!(func, Expr::Field { base, field, .. }
        if field == "spawn" && matches!(base.as_ref(), Expr::Ident { name, .. } if name == "Thread"))
}

fn rewrite(e: &mut Expr, errs: &mut Vec<Diagnostic>) {
    let Expr::Call { func, type_args, args, span } = e else {
        return;
    };
    if !is_thread_spawn(func) {
        return;
    }
    let span = *span;
    if !type_args.is_empty() {
        errs.push(Diagnostic::new(ErrorCode::EType, "`Thread.spawn` takes no type arguments", span));
        return;
    }
    let target = match args.first() {
        Some(Expr::Ident { name, span: fspan }) => Expr::Ident { name: name.clone(), span: *fspan },
        _ => {
            errs.push(Diagnostic::new(
                ErrorCode::EType,
                "`Thread.spawn` takes the name of a fn first: `Thread.spawn(f, args…)`",
                span,
            ));
            return;
        }
    };
    let rest: Vec<Expr> = args.drain(1..).collect();
    let fspan = target.span();
    // The inner call spans from the fn name to the end of the spawn: distinct from the outer
    // call, so the span-keyed type tables keep both.
    let inner_span = crate::span::Span { start: fspan.start, end: span.end, line: fspan.line, col: fspan.col };
    let inner = Expr::Call { func: Box::new(target), type_args: Vec::new(), args: rest, span: inner_span };
    *e = Expr::Call {
        func: Box::new(Expr::Ident { name: "thread_spawn".into(), span: func.span() }),
        type_args: Vec::new(),
        args: vec![inner],
        span,
    };
}
