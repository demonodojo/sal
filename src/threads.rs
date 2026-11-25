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
//!
//! `parallel` (SPEC «Hilos», `parallel`) is rewritten first, in place, into the same builtins:
//!
//! ```text
//! parallel                    let __par0_0 = thread_spawn(f(a))
//!     x = f(a)        →       let __par0_1 = thread_spawn(g(b))
//!     g(b)                    x = thread_join(__par0_0)
//!                             thread_join(__par0_1)
//! ```
//!
//! Each line has to be `f(a…)` or `x = f(a…)` with `f` a name and no type arguments, and no name
//! may be bound by two lines (`E_TYPE`); the rest (moves, loans, effects, the limits of a thread)
//! is what the later passes check for `Thread.spawn`. The inner call keeps the span of the line's
//! call; the spans of the other nodes are laid out by `Spans`.
use crate::ast::*;
use crate::diag::{DiagResult, Diagnostic, ErrorCode};
use crate::span::Span;

pub fn desugar_thread_spawn(prog: &mut Program) -> DiagResult<()> {
    desugar_parallel(prog)?;
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

struct Par {
    /// Number of `parallel` blocks rewritten so far (names the temporaries).
    blocks: usize,
    errs: Vec<Diagnostic>,
}

/// Spans for the nodes of one `parallel` (see the module doc). The borrow checker reads program
/// points from span offsets, and the type tables are keyed by `(start, end)`: a lowered spawn
/// ends where its line's call ends, and every join ends where the last line's call ends, after
/// every argument and before the next statement. Each node takes a `start` of its own from the
/// bytes of the `parallel` text, in order; the only real node that ends where a spawn or a join
/// does is the line's call, whose start is skipped. Callee names get zero-width spans, which no
/// real node has.
struct Spans {
    next: u32,
    next_zero: u32,
    skip: Vec<u32>,
}

impl Spans {
    fn take(&mut self, end: u32, like: Span) -> Span {
        while self.skip.contains(&self.next) {
            self.next += 1;
        }
        let start = self.next;
        self.next += 1;
        Span { start, end, line: like.line, col: like.col }
    }

    fn zero(&mut self, like: Span) -> Span {
        let at = self.next_zero;
        self.next_zero += 1;
        Span { start: at, end: at, line: like.line, col: like.col }
    }
}

pub(crate) fn has_parallel(prog: &Program) -> bool {
    serde_json::to_string(prog).map(|s| s.contains("\"Parallel\"")).unwrap_or(false)
}

fn desugar_parallel(prog: &mut Program) -> DiagResult<()> {
    if !has_parallel(prog) {
        return Ok(());
    }
    let mut par = Par { blocks: 0, errs: Vec::new() };
    for item in &mut prog.items {
        match item {
            Item::Fn(f) => {
                par.blocks = 0;
                block(&mut f.body, &mut par);
            }
            Item::Protocol(p) => {
                for body in p.methods.iter_mut().filter_map(|m| m.body.as_mut()) {
                    par.blocks = 0;
                    block(body, &mut par);
                }
            }
            _ => {}
        }
    }
    if par.errs.is_empty() {
        Ok(())
    } else {
        Err(par.errs)
    }
}

/// Rewrites the `parallel` statements of `b` and of every block nested in it, in source order:
/// within a body, the `n` of `__par<n>_<i>` counts them in that order (`threads.sal` numbers them
/// the same way).
fn block(b: &mut Block, par: &mut Par) {
    let old = std::mem::take(&mut b.stmts);
    for st in old {
        match st {
            Stmt::Parallel { body, span } => expand(body, span, &mut b.stmts, par),
            mut other => {
                stmt(&mut other, par);
                b.stmts.push(other);
            }
        }
    }
    if let Some(t) = &mut b.tail {
        expr(t, par);
    }
}

fn stmt(st: &mut Stmt, par: &mut Par) {
    match st {
        Stmt::Let { init: e, .. } | Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } => expr(e, par),
        Stmt::Assign { target, value, .. } => {
            expr(target, par);
            expr(value, par);
        }
        Stmt::While { cond, body, .. } => {
            expr(cond, par);
            block(body, par);
        }
        Stmt::InlineFor { body, .. } | Stmt::Parallel { body, .. } => block(body, par),
        Stmt::Return { value: None, .. } | Stmt::Break { .. } => {}
    }
}

fn expr(e: &mut Expr, par: &mut Par) {
    match e {
        Expr::Unit { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::String { .. }
        | Expr::Ident { .. } => {}
        Expr::Call { func, args, .. } => {
            expr(func, par);
            for a in args {
                expr(a, par);
            }
        }
        Expr::Lambda { body, .. } => expr(body, par),
        Expr::Binary { left, right, .. } => {
            expr(left, par);
            expr(right, par);
        }
        Expr::Unary { expr: inner, .. } | Expr::To { expr: inner, .. } | Expr::Try { expr: inner, .. } => {
            expr(inner, par)
        }
        Expr::Block(b) | Expr::On { body: b, .. } => block(b, par),
        Expr::TensorLit { rows, .. } => {
            for c in rows.iter_mut().flatten() {
                expr(c, par);
            }
        }
        Expr::Field { base, .. } => expr(base, par),
        Expr::Match { scrutinee, arms, .. } => {
            expr(scrutinee, par);
            for a in arms {
                expr(&mut a.body, par);
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            expr(cond, par);
            block(then_block, par);
            for ei in elsifs {
                expr(&mut ei.cond, par);
                block(&mut ei.body, par);
            }
            if let Some(eb) = else_block {
                block(eb, par);
            }
        }
    }
}

fn line_err(par: &mut Par, msg: &str, span: Span) {
    par.errs.push(Diagnostic::new(ErrorCode::EType, msg, span));
}

/// One checked line: the name it binds (with its span) and its call `f(a…)`.
type Line = (Option<(String, Span)>, Expr);

fn check_line(line: Stmt, bound: &mut Vec<String>, par: &mut Par) -> Option<Line> {
    let (target, call) = match line {
        Stmt::Expr(call @ Expr::Call { .. }) => (None, call),
        Stmt::Assign { target: Expr::Ident { name, span }, value: call @ Expr::Call { .. }, .. } => {
            (Some((name, span)), call)
        }
        other => {
            line_err(par, "a `parallel` line is `f(args…)` or `x = f(args…)`", *crate::fmt::stmt_span(&other));
            return None;
        }
    };
    let Expr::Call { func, type_args, span, .. } = &call else { unreachable!() };
    if !matches!(func.as_ref(), Expr::Ident { .. }) {
        line_err(par, "a `parallel` line calls a fn by its name: `f(args…)`", *span);
        return None;
    }
    if !type_args.is_empty() {
        line_err(par, "a `parallel` line takes no type arguments", *span);
        return None;
    }
    if let Some((name, tspan)) = &target {
        if bound.contains(name) {
            line_err(par, &format!("`{name}` is bound by two lines of one `parallel`"), *tspan);
            return None;
        }
        bound.push(name.clone());
    }
    Some((target, call))
}

fn call(name: &str, func_span: Span, args: Vec<Expr>, span: Span) -> Expr {
    Expr::Call {
        func: Box::new(Expr::Ident { name: name.into(), span: func_span }),
        type_args: Vec::new(),
        args,
        span,
    }
}

fn expand(body: Block, span: Span, out: &mut Vec<Stmt>, par: &mut Par) {
    let n = par.blocks;
    par.blocks += 1;
    let mut stmts: Vec<Stmt> = body.stmts;
    if let Some(t) = body.tail {
        stmts.push(Stmt::Expr(*t));
    }
    let mut bound = Vec::new();
    let mut lines: Vec<Line> = stmts.into_iter().filter_map(|st| check_line(st, &mut bound, par)).collect();
    for (_, c) in &mut lines {
        expr(c, par);
    }
    let Some(last) = lines.last() else {
        return;
    };
    let joins_end = last.1.span().end;
    let mut spans = Spans {
        next: span.start + 1,
        next_zero: span.start + 1,
        skip: lines.iter().map(|(_, c)| c.span().start).collect(),
    };
    let mut joins = Vec::new();
    for (i, (target, inner)) in lines.into_iter().enumerate() {
        let like = inner.span();
        let temp = format!("__par{n}_{i}");
        let let_span = spans.take(like.end, like);
        let spawn_span = spans.take(like.end, like);
        let spawn = call("thread_spawn", spans.zero(like), vec![inner], spawn_span);
        out.push(Stmt::Let { name: temp.clone(), ty: None, init: spawn, frozen: false, stack: false, span: let_span });
        let temp_use = Expr::Ident { name: temp, span: spans.take(joins_end, like) };
        let join = call("thread_join", spans.zero(like), vec![temp_use], spans.take(joins_end, like));
        joins.push(match target {
            Some((name, tspan)) => Stmt::Assign {
                target: Expr::Ident { name, span: tspan },
                value: join,
                span: spans.take(joins_end, like),
            },
            None => Stmt::Expr(join),
        });
    }
    out.extend(joins);
}
