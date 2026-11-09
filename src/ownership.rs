use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::string_expr::{add_chain_leftmost, is_string_type, StringEnv};
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::span::Span;
use crate::typed::ExprTypeEntry;

/// What the tracker knows beyond the syntax of one fn: the inferred type of each
/// expression, each user fn's parameter modes, and the names that build a value
/// (struct and enum constructors), whose arguments move into it.
#[derive(Default)]
struct Sig {
    tparams: Vec<String>,
    params: Vec<Type>,
}

#[derive(Default)]
struct Typed {
    types: HashMap<(u32, u32), Type>,
    modes: HashMap<String, Vec<ParamMode>>,
    sigs: HashMap<String, Sig>,
    ctors: HashSet<String>,
    structs: HashMap<String, StructDef>,
}

thread_local! {
    static TYPED: RefCell<Typed> = RefCell::new(Typed::default());
    /// Set while checking moves. Inference of `borrow`/`take` (still used by
    /// `sal fmt`) keeps the old rule.
    static STRICT_MOVES: Cell<bool> = const { Cell::new(false) };
}

struct StrictMoves;

impl StrictMoves {
    fn enter() -> Self {
        STRICT_MOVES.with(|c| c.set(true));
        StrictMoves
    }
}

impl Drop for StrictMoves {
    fn drop(&mut self) {
        STRICT_MOVES.with(|c| c.set(false));
    }
}

fn strict_moves() -> bool {
    STRICT_MOVES.with(|c| c.get())
}

fn load_program(prog: &Program) {
    TYPED.with(|t| {
        let mut t = t.borrow_mut();
        t.modes.clear();
        t.sigs.clear();
        t.ctors.clear();
        t.types.clear();
        t.structs = collect_structs(prog);
        for item in &prog.items {
            match item {
                Item::Fn(f) => {
                    t.modes.insert(f.name.clone(), f.params.iter().map(|p| p.mode).collect());
                    t.sigs.insert(
                        f.name.clone(),
                        Sig {
                            tparams: f.type_params.clone(),
                            params: f.params.iter().map(|p| p.ty.clone()).collect(),
                        },
                    );
                }
                Item::Struct(sd) => {
                    t.ctors.insert(sd.name.clone());
                }
                Item::Enum(e) => {
                    for v in &e.variants {
                        if !v.fields.is_empty() {
                            t.ctors.insert(v.name.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

fn load_fn_types(entries: &[ExprTypeEntry]) {
    TYPED.with(|t| {
        let mut t = t.borrow_mut();
        t.types.clear();
        for e in entries {
            t.types.insert((e.span_start, e.span_end), e.ty.clone());
        }
    });
}

/// The inferred type of `e` when the syntactic guess `guess` is `Unknown`.
fn typed_or(e: &Expr, guess: Type) -> Type {
    let unknown = matches!(&guess, Type::Named { name, .. } if name == "Unknown");
    if !unknown {
        return guess;
    }
    let sp = e.span();
    TYPED.with(|t| t.borrow().types.get(&(sp.start, sp.end)).cloned()).unwrap_or(guess)
}

fn call_moves_arg(name: &str, i: usize, nargs: usize) -> bool {
    if i >= nargs {
        return false;
    }
    match name {
        "list_push" => return i == 1,
        "list_set" | "dict_put" => return i == 2,
        // Free the shell of a container: the argument is consumed (the caller no longer releases it).
        "list_free_shell" | "enum_free_shell" => return i == 0,
        _ => {}
    }
    TYPED.with(|t| {
        let t = t.borrow();
        if t.ctors.contains(name) {
            return true;
        }
        // A by-value non-Copy parameter moves. `&T` and `&mut T` do not.
        // A bare type parameter stays on the inferred mode: it may be `Int`.
        if strict_moves() {
            // `keep` is still the arena mode: the caller retains the container.
            if t.modes.get(name).and_then(|m| m.get(i)) == Some(&ParamMode::Keep) {
                return false;
            }
            if let Some(sig) = t.sigs.get(name) {
                if let Some(ty) = sig.params.get(i) {
                    if !matches!(ty, Type::Ref { .. }) && !is_copy_type_ext(ty, &t.structs) {
                        let type_param = match ty {
                            Type::Named { name: n, .. } | Type::Qualified { name: n, .. } => {
                                sig.tparams.iter().any(|p| p == n)
                            }
                            _ => false,
                        };
                        if !type_param {
                            return true;
                        }
                    } else {
                        return false;
                    }
                }
            }
        }
        t.modes.get(name).is_some_and(|m| m.get(i) == Some(&ParamMode::Take))
    })
}

/// Whether argument `i` of `name` takes the value (the borrow checker uses this
/// so a move under a live loan is `E_BORROW`). Borrowing builtins and unknown
/// callees do not.
pub(crate) fn operand_is_moved(name: &str, i: usize, nargs: usize) -> bool {
    if is_borrow_fn(name) {
        false
    } else if is_move_fn(name) {
        true
    } else {
        call_moves_arg(name, i, nargs)
    }
}

pub fn check_ownership(prog: &Program, infer: &crate::infer::InferOutput) -> DiagResult<()> {
    let modded = infer_param_modes(prog);
    let _strict = StrictMoves::enter();
    load_program(&modded);
    for item in &modded.items {
        if let Item::Fn(f) = item {
            let entries = infer
                .expr_types_by_fn
                .iter()
                .find(|(fd, _)| fd.name == f.name)
                .map(|(_, e)| e.as_slice())
                .unwrap_or(&[]);
            load_fn_types(entries);
            check_fn_moves(f)?;
        }
    }
    crate::borrowck::check_borrows(prog, infer)?;
    crate::borrowck::publish_moved_locals(prog, infer);
    check_arena_params(&modded, infer)
}

/// Infer `borrow` / `take` on each parameter from the function body.
///
/// - Trivial types (`Int`, `Float`, `Bool`, and structs of only those) stay `Inferred`
///   (formatter prints no mode).
/// - Unique params only read (borrow-fn args, field reads, interpolation) → `Borrow`.
/// - Unique params moved/consumed (`to`, move-fn, bind, return) → `Take`.
/// - A call to an unknown function uses the argument; it does not move a String handle.
/// - Unused unique params are treated as `Take` (implicit drop).
/// - Explicit `Borrow` / `Take` in the source AST are preserved.
pub fn infer_param_modes(prog: &Program) -> Program {
    let structs = collect_structs(prog);
    let mut out = prog.clone();
    // A fn that hands a parameter to a `take` position of another fn takes it too. Each
    // round re-infers from the source modes, reading the callee modes of the previous
    // round, until nothing changes.
    for _ in 0..16 {
        load_program(&out);
        let mut next = prog.clone();
        for item in &mut next.items {
            if let Item::Fn(f) = item {
                infer_fn_modes(f, &structs);
            }
        }
        let done = fn_modes(&next) == fn_modes(&out);
        out = next;
        if done {
            break;
        }
    }
    out
}

fn fn_modes(prog: &Program) -> Vec<ParamMode> {
    prog.items
        .iter()
        .filter_map(|i| if let Item::Fn(f) = i { Some(f) } else { None })
        .flat_map(|f| f.params.iter().map(|p| p.mode))
        .collect()
}

fn collect_structs(prog: &Program) -> HashMap<String, StructDef> {
    let mut structs = HashMap::new();
    for item in &prog.items {
        if let Item::Struct(s) = item {
            structs.insert(s.name.clone(), s.clone());
        }
    }
    structs
}

fn infer_fn_modes(f: &mut FnDef, structs: &HashMap<String, StructDef>) {
    let mut env: HashMap<String, Type> = HashMap::new();
    for p in &f.params {
        env.insert(p.name.clone(), p.ty.clone());
    }
    let param_names: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    let mut moved: HashSet<String> = HashSet::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut frozen = HashSet::new();
    let _ = track_block(
        &f.body,
        &mut env,
        &mut moved,
        &param_names,
        &mut taken,
        &mut used,
        &mut frozen,
    );
    for p in &mut f.params {
        if p.mode != ParamMode::Inferred {
            continue;
        }
        if is_copy_type_ext(&p.ty, structs) {
            continue;
        }
        // SPEC «Memoria»: a reference is never owned by the callee: `&T` and `&mut T` borrow.
        // Inside `@arena`, `&mut List` / `&mut Dict` is the writable container parameter (`keep`).
        if let Type::Ref { mutable, inner, .. } = &p.ty {
            let container = matches!(inner.as_ref(), Type::Named { name, .. } if name == "List" || name == "Dict");
            p.mode = if f.arena && *mutable && container { ParamMode::Keep } else { ParamMode::Borrow };
            continue;
        }
        // A by-value parameter moves (no inferred `borrow`): the caller gives the value up.
        p.mode = ParamMode::Take;
    }
}

fn check_fn_moves(f: &FnDef) -> DiagResult<()> {
    let mut env: HashMap<String, Type> = HashMap::new();
    for p in &f.params {
        env.insert(p.name.clone(), p.ty.clone());
    }
    let mut moved: HashSet<String> = HashSet::new();
    check_block(&f.body, &mut env, &mut moved)
}

fn check_block(
    b: &Block,
    env: &mut HashMap<String, Type>,
    moved: &mut HashSet<String>,
) -> DiagResult<()> {
    let _ = track_block(
        b,
        env,
        moved,
        &HashSet::new(),
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut HashSet::new(),
    )?;
    Ok(())
}

fn track_block(
    b: &Block,
    env: &mut HashMap<String, Type>,
    moved: &mut HashSet<String>,
    params: &HashSet<String>,
    taken: &mut HashSet<String>,
    used: &mut HashSet<String>,
    frozen: &mut HashSet<String>,
) -> DiagResult<bool> {
    let mut diverged = false;
    for st in &b.stmts {
        if diverged {
            break;
        }
        diverged = track_stmt(st, env, moved, params, taken, used, frozen)?;
    }
    if diverged {
        return Ok(true);
    }
    if let Some(t) = &b.tail {
        let ty = track_expr(t, env, moved, params, taken, used, frozen)?;
        // Tail value escapes the block (function result or block expr) → move unique.
        consume_if_unique(t, &ty, env, moved, params, taken);
    }
    Ok(false)
}

fn track_stmt(
    st: &Stmt,
    env: &mut HashMap<String, Type>,
    moved: &mut HashSet<String>,
    params: &HashSet<String>,
    taken: &mut HashSet<String>,
    used: &mut HashSet<String>,
    frozen: &mut HashSet<String>,
) -> DiagResult<bool> {
    match st {
        Stmt::Break { .. } => Ok(false),
        Stmt::Let {
            name,
            ty,
            init,
            frozen: is_frozen,
            ..
        } => {
            let ity = track_expr(init, env, moved, params, taken, used, frozen)?;
            consume_if_unique(init, &ity, env, moved, params, taken);
            let bound = ty.clone().unwrap_or(ity);
            env.insert(name.clone(), bound);
            revive(moved, name);
            if *is_frozen {
                frozen.insert(name.clone());
            }
            Ok(false)
        }
        Stmt::Expr(e) => {
            track_expr(e, env, moved, params, taken, used, frozen)?;
            Ok(false)
        }
        Stmt::Assign { target, value, span } => {
            if let Some(name) = frozen_assign_name(target) {
                if frozen.contains(name) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("`{name}` is @frozen and cannot be mutated"),
                        *span,
                    )]);
                }
            }
            let vty = track_expr(value, env, moved, params, taken, used, frozen)?;
            consume_if_unique(value, &vty, env, moved, params, taken);
            if let Expr::Ident { name, .. } = target {
                // `x = x + ...` appends in place: the old `x` is moved into the result.
                if is_string_type(&vty) && matches!(value, Expr::Binary { op: BinOp::Add, .. }) {
                    if let Expr::Ident { name: leaf, .. } = add_chain_leftmost(value) {
                        if leaf == name {
                            consume_if_unique(add_chain_leftmost(value), &vty, env, moved, params, taken);
                        }
                    }
                }
                env.insert(name.clone(), vty);
                revive(moved, name);
            } else if let Expr::Field { base, field, .. } = target {
                match base.as_ref() {
                    // `p.f = v` puts a field back.
                    Expr::Ident { name, span: bsp } => {
                        if moved.contains(name) {
                            return Err(moved_err(name, *bsp));
                        }
                        moved.remove(&format!("{name}.{field}"));
                    }
                    _ => {
                        track_expr(target, env, moved, params, taken, used, frozen)?;
                    }
                }
            } else {
                track_expr(target, env, moved, params, taken, used, frozen)?;
                let _ = span;
            }
            Ok(false)
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                let ty = track_expr(v, env, moved, params, taken, used, frozen)?;
                consume_if_unique(v, &ty, env, moved, params, taken);
            }
            Ok(true)
        }
        Stmt::While { cond, body, .. } => {
            track_expr(cond, env, moved, params, taken, used, frozen)?;
            let mut body_moved = moved.clone();
            let body_div = track_block(
                body,
                env,
                &mut body_moved,
                params,
                taken,
                used,
                frozen,
            )?;
            if !body_div {
                if strict_moves() {
                    // The value is still live on the back edge. The next iteration
                    // moves it again: that use is `E_MOVED`.
                    let mut again = body_moved.clone();
                    track_expr(cond, env, &mut again, params, taken, used, frozen)?;
                    track_block(body, env, &mut again, params, taken, used, frozen)?;
                }
                *moved = body_moved;
            }
            Ok(false)
        }
    }
}

fn frozen_assign_name(target: &Expr) -> Option<&str> {
    match target {
        Expr::Ident { name, .. } => Some(name),
        Expr::Field { base, .. } => match base.as_ref() {
            Expr::Ident { name, .. } => Some(name),
            _ => None,
        },
        _ => None,
    }
}

fn track_expr(
    e: &Expr,
    env: &HashMap<String, Type>,
    moved: &mut HashSet<String>,
    params: &HashSet<String>,
    taken: &mut HashSet<String>,
    used: &mut HashSet<String>,
    frozen: &mut HashSet<String>,
) -> DiagResult<Type> {
    match e {
        Expr::Int { span, .. } => Ok(named("Int", *span)),
        Expr::Float { span, .. } => Ok(named("Float", *span)),
        Expr::Bool { span, .. } => Ok(named("Bool", *span)),
        Expr::String { parts, span, .. } => {
            for part in parts {
                if let StringPart::Interp(name) = part {
                    if moved.contains(name) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EMoved,
                            format!("use of moved value `{name}`"),
                            *span,
                        )]);
                    }
                    if params.contains(name) {
                        used.insert(name.clone());
                    }
                }
            }
            Ok(named("String", *span))
        }
        Expr::Ident { name, span } => {
            if is_moved(moved, name) {
                return Err(moved_err(name, *span));
            }
            if params.contains(name) {
                used.insert(name.clone());
            }
            Ok(env
                .get(name)
                .cloned()
                .unwrap_or_else(|| named("Unknown", *span)))
        }
        Expr::Call { func, args, span, .. } => {
            track_expr(func, env, moved, params, taken, used, frozen)?;
            let mut arg_tys = Vec::new();
            for a in args {
                arg_tys.push(track_expr(a, env, moved, params, taken, used, frozen)?);
            }
            if let Expr::Ident { name, .. } = func.as_ref() {
                if is_move_fn(name) {
                    for (a, ty) in args.iter().zip(arg_tys.iter()) {
                        consume_if_unique(a, ty, env, moved, params, taken);
                    }
                } else if (0..args.len()).any(|i| call_moves_arg(name, i, args.len())) {
                    for (i, (a, ty)) in args.iter().zip(arg_tys.iter()).enumerate() {
                        if call_moves_arg(name, i, args.len()) {
                            consume_if_unique(a, ty, env, moved, params, taken);
                        }
                    }
                } else if !is_borrow_fn(name) {
                    // Unknown callees: count args as uses only (selfhost passes String handles).
                    for a in args {
                        if let Expr::Ident { name: arg_name, .. } = a {
                            if params.contains(arg_name) {
                                used.insert(arg_name.clone());
                            }
                        }
                    }
                }
                // borrow_fn: args already counted as uses, not taken
                if returns_owned_string(name) {
                    return Ok(named("String", *span));
                }
            }
            Ok(typed_or(e, named("Unknown", *span)))
        }
        Expr::Lambda {
            params: lp,
            body,
            span,
        } => use_lambda(lp, body, env, moved, params, taken, used, *span),
        Expr::Binary {
            op,
            left,
            right,
            span,
            ..
        } => {
            let lt = track_expr(left, env, moved, params, taken, used, frozen)?;
            track_expr(right, env, moved, params, taken, used, frozen)?;
            // String `+` copies (str_concat) or appends onto a temporary: neither
            // operand is consumed. Only `x = x + ...` moves `x` (see track_stmt).
            if *op == BinOp::Add && is_string_type(&lt) {
                Ok(named("String", *span))
            } else {
                Ok(named("Int", *span))
            }
        }
        Expr::Unary { op, expr, span } => {
            let ty = track_expr(expr, env, moved, params, taken, used, frozen)?;
            match op {
                UnOp::Ref => Ok(Type::Ref {
                    mutable: false,
                    lifetime: None,
                    inner: Box::new(ty),
                    span: *span,
                }),
                UnOp::RefMut => Ok(Type::Ref {
                    mutable: true,
                    lifetime: None,
                    inner: Box::new(ty),
                    span: *span,
                }),
                UnOp::Deref => match ty {
                    Type::Ref { inner, .. } => Ok(*inner),
                    other => Ok(other),
                },
                UnOp::Neg | UnOp::Not => Ok(ty),
            }
        }
        Expr::Block(b) => {
            let mut local = env.clone();
            track_block(b, &mut local, moved, params, taken, used, frozen)?;
            Ok(named("Unit", b.span))
        }
        Expr::On { body, .. } => {
            let mut local = env.clone();
            track_block(body, &mut local, moved, params, taken, used, frozen)?;
            Ok(named("Unit", body.span))
        }
        Expr::To { expr, span, .. } => {
            let ty = track_expr(expr, env, moved, params, taken, used, frozen)?;
            consume_if_unique(expr, &ty, env, moved, params, taken);
            Ok(ty_with_span(ty, *span))
        }
        Expr::TensorLit { rows, span, .. } => {
            for row in rows {
                for c in row {
                    track_expr(c, env, moved, params, taken, used, frozen)?;
                }
            }
            Ok(Type::Tensor {
                elem: TensorElem::F32,
                dims: vec![],
                place: Place::Cpu,
                span: *span,
            })
        }
        Expr::Field { base, field, span } => {
            if let Expr::Ident { name, span: bsp } = base.as_ref() {
                // Reading a field of a partly moved value is fine unless that field left.
                if moved.contains(name) || moved.contains(&format!("{name}.{field}")) {
                    return Err(moved_err(name, *bsp));
                }
                if params.contains(name) {
                    used.insert(name.clone());
                }
            } else {
                track_expr(base, env, moved, params, taken, used, frozen)?;
            }
            Ok(typed_or(e, named("Unknown", *span)))
        }
        Expr::Match {
            scrutinee, arms, span, ..
        } => {
            track_expr(scrutinee, env, moved, params, taken, used, frozen)?;
            // Arms are alternatives: a move in one does not move the value in another.
            let branch_base = moved.clone();
            let mut merged = branch_base.clone();
            for arm in arms {
                let mut local = env.clone();
                bind_pattern_names(&arm.pattern, &mut local);
                let mut br_moved = branch_base.clone();
                let diverged = match &arm.body {
                    Expr::Block(b) => {
                        track_block(b, &mut local, &mut br_moved, params, taken, used, frozen)?
                    }
                    other => {
                        track_expr(other, &local, &mut br_moved, params, taken, used, frozen)?;
                        false
                    }
                };
                if !diverged {
                    merged.extend(br_moved);
                }
            }
            *moved = merged;
            Ok(typed_or(e, named("Unknown", *span)))
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            span,
            ..
        } => {
            track_expr(cond, env, moved, params, taken, used, frozen)?;
            let branch_base = moved.clone();
            let mut merged = branch_base.clone();
            let run_branch = |block: &Block,
                                  env: &HashMap<String, Type>,
                                  taken: &mut HashSet<String>,
                                  used: &mut HashSet<String>,
                                  frozen: &mut HashSet<String>|
             -> DiagResult<HashSet<String>> {
                let mut br_moved = branch_base.clone();
                let mut br_env = env.clone();
                let mut diverged = false;
                for st in &block.stmts {
                    if track_stmt(st, &mut br_env, &mut br_moved, params, taken, used, frozen)? {
                        diverged = true;
                    }
                }
                if let Some(t) = &block.tail {
                    track_expr(t, &br_env, &mut br_moved, params, taken, used, frozen)?;
                }
                // A branch that returns never reaches the code after the `if`.
                Ok(if diverged { branch_base.clone() } else { br_moved })
            };
            merged.extend(run_branch(then_block, env, taken, used, frozen)?);
            for arm in elsifs {
                track_expr(&arm.cond, env, moved, params, taken, used, frozen)?;
                merged.extend(run_branch(&arm.body, env, taken, used, frozen)?);
            }
            if let Some(else_block) = else_block {
                merged.extend(run_branch(else_block, env, taken, used, frozen)?);
            }
            // Moved in any branch that falls through: moved afterwards.
            *moved = merged;
            Ok(typed_or(e, named("Unknown", *span)))
        }
        Expr::Try { expr, .. } => track_expr(expr, env, moved, params, taken, used, frozen),
    }
}

/// `x => body`: bind `x` locally; analyze body for moves (captures and param).
fn use_lambda(
    params: &[String],
    body: &Expr,
    env: &HashMap<String, Type>,
    moved: &mut HashSet<String>,
    fn_params: &HashSet<String>,
    taken: &mut HashSet<String>,
    used: &mut HashSet<String>,
    span: Span,
) -> DiagResult<Type> {
    if params.len() != 1 {
        return Ok(named("Unknown", span));
    }
    let mut local = env.clone();
    // Fresh binding — do not treat the param name as a use of an outer value.
    let param_ty = named("Float", span);
    local.insert(params[0].clone(), param_ty.clone());
    let mut lambda_frozen = HashSet::new();
    let body_ty = track_expr(
        body,
        &local,
        moved,
        fn_params,
        taken,
        used,
        &mut lambda_frozen,
    )?;
    Ok(Type::Fn {
        params: vec![param_ty],
        ret: Box::new(body_ty),
        effects: vec![],
        span,
    })
}

/// `name` or any of its fields (`name.f`) was moved out.
fn is_moved(moved: &HashSet<String>, name: &str) -> bool {
    moved.contains(name) || moved.iter().any(|m| m.strip_prefix(name).is_some_and(|r| r.starts_with('.')))
}

/// A rebinding of `name` revives it and every field moved out of it.
fn revive(moved: &mut HashSet<String>, name: &str) {
    moved.retain(|m| m != name && !m.strip_prefix(name).is_some_and(|r| r.starts_with('.')));
}

fn moved_err(name: &str, span: Span) -> Vec<Diagnostic> {
    vec![Diagnostic::new(ErrorCode::EMoved, format!("use of moved value `{name}`"), span)]
}

fn consume_if_unique(
    e: &Expr,
    ty: &Type,
    env: &HashMap<String, Type>,
    moved: &mut HashSet<String>,
    params: &HashSet<String>,
    taken: &mut HashSet<String>,
) {
    if is_copy_type(ty) || TYPED.with(|t| is_copy_type_ext(ty, &t.borrow().structs)) {
        return;
    }
    if let Expr::Ident { name, .. } = e {
        moved.insert(name.clone());
        if params.contains(name) {
            taken.insert(name.clone());
        }
    }
    // Partial move: `p.f` of a non-Copy field leaves `p` without that field.
    // A field of a reference is still a copy of the handle: moving a non-Copy
    // out of `&T` is a later `E_BORROW`, and the walkers pass that handle on.
    if let Expr::Field { base, field, .. } = e {
        if let Expr::Ident { name, .. } = base.as_ref() {
            if matches!(env.get(name), Some(Type::Ref { .. })) {
                return;
            }
            moved.insert(format!("{name}.{field}"));
            if params.contains(name) {
                taken.insert(name.clone());
            }
        }
    }
}

/// Builtins whose result is a fresh heap `String` (one owner, moved on use).
fn returns_owned_string(name: &str) -> bool {
    matches!(
        name,
        "strdup" | "str_concat" | "int_to_str" | "char_to_str" | "str_slice" | "read_file"
    )
}

fn is_copy_type(ty: &Type) -> bool {
    match ty {
        Type::Named { name, .. } | Type::Qualified { name, .. } => {
            matches!(name.as_str(), "Int" | "Float" | "Bool" | "Unit" | "Unknown")
        }
        Type::Tensor { .. } | Type::Fn { .. } => false,
        Type::Ref { mutable, .. } => !mutable,
    }
}

fn is_copy_type_ext(ty: &Type, structs: &HashMap<String, StructDef>) -> bool {
    is_copy_type_ext_rec(ty, structs, &mut HashSet::new())
}

fn is_copy_type_ext_rec(
    ty: &Type,
    structs: &HashMap<String, StructDef>,
    visiting: &mut HashSet<String>,
) -> bool {
    match ty {
        Type::Named { name, .. } | Type::Qualified { name, .. } => {
            if matches!(name.as_str(), "Int" | "Float" | "Bool" | "Unit" | "Unknown") {
                return true;
            }
            if !visiting.insert(name.clone()) {
                return false;
            }
            let ok = structs.get(name).is_some_and(|def| {
                def.fields
                    .iter()
                    .all(|f| is_copy_type_ext_rec(&f.ty, structs, visiting))
            });
            visiting.remove(name);
            ok
        }
        Type::Tensor { .. } | Type::Fn { .. } => false,
        Type::Ref { mutable, .. } => !mutable,
    }
}

fn is_move_fn(name: &str) -> bool {
    matches!(name, "save" | "reshape" | "transpose" | "to_cpu" | "to_gpu")
}

fn is_borrow_fn(name: &str) -> bool {
    matches!(
        name,
        "matmul"
            | "qmatmul"
            | "attention"
            | "relu"
            | "silu"
            | "rms_norm"
            | "rope"
            | "softmax"
            | "map"
            | "reduce"
            | "argmax"
            | "print"
            | "print_str"
            | "eprint_str"
            | "getenv"
            | "mkdir_p"
            | "str_eq"
            | "str_contains"
            | "str_index_of"
            | "str_index_from"
            | "str_len"
            | "str_char"
            | "str_skip"
            | "str_hash"
            | "map_get"
            | "map_put"
            | "ir_text"
            | "lex_src"
            | "str_slice"
            | "argc"
            | "write_file"
            | "clang"
            | "clang_obj"
            | "link_objs"
            | "gated_print_str"
            | "vec_push"
            | "vec_get"
            | "vec_set"
            | "vec_len"
            | "vec_free"
            | "list_free"
            | "dict_free"
            | "enum_free"
            | "enum_owns"
            | "enum_box_id"
            | "bytes_free"
            | "list_get"
            | "list_set"
            | "list_pop"
            | "dict_has"
            | "dict_len"
            | "dict_remove"
            | "dict_keys"
            | "dict_values"
            | "list_len"
            | "list_box_id"
    )
}

fn bind_pattern_names(pat: &Pattern, env: &mut HashMap<String, Type>) {
    match pat {
        Pattern::Wild(_) | Pattern::Int(_, _) | Pattern::Str(_, _) | Pattern::Regex(_, _) => {}
        Pattern::Ident(name, span) => {
            env.insert(name.clone(), named("Unknown", *span));
        }
        Pattern::Variant { args, .. } => {
            for a in args {
                bind_pattern_names(a, env);
            }
        }
    }
}

fn named(name: &str, span: Span) -> Type {
    Type::Named {
        name: name.into(),
        args: vec![],
        span,
    }
}

fn is_list_or_dict(ty: &Type) -> bool {
    match ty {
        Type::Named { name, .. } | Type::Qualified { name, .. } => name == "List" || name == "Dict",
        _ => false,
    }
}

fn type_exportable(
    ty: &Type,
    structs: &HashMap<String, StructDef>,
    enums: &HashMap<String, EnumDef>,
    depth: u32,
) -> bool {
    if depth > 16 {
        return false;
    }
    match ty {
        Type::Tensor { .. } | Type::Fn { .. } | Type::Ref { .. } => false,
        Type::Named { name, args, .. } | Type::Qualified { name, args, .. } => match name.as_str() {
            "Int" | "Float" | "Bool" | "Unit" | "String" | "List" | "Dict" | "Bytes" => args
                .iter()
                .all(|a| type_exportable(a, structs, enums, depth + 1)),
            "any" | "Tensor" => false,
            other if enums.contains_key(other) => {
                args.iter().all(|a| type_exportable(a, structs, enums, depth + 1))
            }
            other => {
                let Some(st) = structs.get(other) else {
                    return false;
                };
                if !st.type_params.is_empty() || !args.is_empty() {
                    return false;
                }
                st.fields
                    .iter()
                    .all(|f| type_exportable(&f.ty, structs, enums, depth + 1))
            }
        },
    }
}

fn ident_name(e: &Expr) -> Option<&str> {
    match e {
        Expr::Ident { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

fn collect_alias_pairs(b: &Block, out: &mut Vec<(String, String)>) {
    for st in &b.stmts {
        match st {
            Stmt::Let { name, init, .. } => {
                if let Some(rhs) = ident_name(init) {
                    out.push((name.clone(), rhs.to_string()));
                }
                collect_alias_pairs_expr(init, out);
            }
            Stmt::Assign { target, value, .. } => {
                if let (Some(lhs), Some(rhs)) = (ident_name(target), ident_name(value)) {
                    out.push((lhs.to_string(), rhs.to_string()));
                }
                collect_alias_pairs_expr(target, out);
                collect_alias_pairs_expr(value, out);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    collect_alias_pairs_expr(v, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                collect_alias_pairs_expr(cond, out);
                collect_alias_pairs(body, out);
            }
            Stmt::Expr(e) => collect_alias_pairs_expr(e, out),
            Stmt::Break { .. } => {}
        }
    }
    if let Some(t) = &b.tail {
        collect_alias_pairs_expr(t, out);
    }
}

fn collect_alias_pairs_expr(e: &Expr, out: &mut Vec<(String, String)>) {
    match e {
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } | Expr::Ident { .. } => {}
        Expr::Call { func, args, .. } => {
            collect_alias_pairs_expr(func, out);
            for a in args {
                collect_alias_pairs_expr(a, out);
            }
        }
        Expr::Lambda { body, .. } => collect_alias_pairs_expr(body, out),
        Expr::Binary { left, right, .. } => {
            collect_alias_pairs_expr(left, out);
            collect_alias_pairs_expr(right, out);
        }
        Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } => {
            collect_alias_pairs_expr(expr, out);
        }
        Expr::Block(b) | Expr::On { body: b, .. } => collect_alias_pairs(b, out),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    collect_alias_pairs_expr(c, out);
                }
            }
        }
        Expr::Field { base, .. } => collect_alias_pairs_expr(base, out),
        Expr::Match { scrutinee, arms, .. } => {
            collect_alias_pairs_expr(scrutinee, out);
            for arm in arms {
                collect_alias_pairs_expr(&arm.body, out);
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            collect_alias_pairs_expr(cond, out);
            collect_alias_pairs(then_block, out);
            for elsif in elsifs {
                collect_alias_pairs_expr(&elsif.cond, out);
                collect_alias_pairs(&elsif.body, out);
            }
            if let Some(b) = else_block {
                collect_alias_pairs(b, out);
            }
        }
    }
}

fn bind_aliases(f: &FnDef) -> HashMap<String, String> {
    let mut root: HashMap<String, String> = HashMap::new();
    for p in &f.params {
        root.insert(p.name.clone(), p.name.clone());
    }
    let mut pairs = Vec::new();
    collect_alias_pairs(&f.body, &mut pairs);
    let mut changed = true;
    while changed {
        changed = false;
        for (lhs, rhs) in &pairs {
            if root.contains_key(lhs) {
                continue;
            }
            if let Some(r) = root.get(rhs).cloned() {
                root.insert(lhs.clone(), r);
                changed = true;
            }
        }
    }
    root
}

fn param_index(f: &FnDef, name: &str) -> Option<usize> {
    f.params.iter().position(|p| p.name == name)
}

fn mutates_container(name: &str) -> bool {
    matches!(
        name,
        "list_push"
            | "list_set"
            | "list_pop"
            | "dict_put"
            | "dict_remove"
            | "str_append"
            | "bytes_set_u8"
            | "bytes_set_u32"
            | "bytes_fill"
            | "bytes_fill_u32"
            | "bytes_copy"
    )
}

fn walk_calls(
    b: &Block,
    f: &mut dyn FnMut(&str, &[Expr], Span) -> DiagResult<()>,
) -> DiagResult<()> {
    for st in &b.stmts {
        match st {
            Stmt::Let { init, .. } => walk_calls_expr(init, f)?,
            Stmt::Expr(e) => walk_calls_expr(e, f)?,
            Stmt::Assign { target, value, .. } => {
                walk_calls_expr(target, f)?;
                walk_calls_expr(value, f)?;
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    walk_calls_expr(v, f)?;
                }
            }
            Stmt::While { cond, body, .. } => {
                walk_calls_expr(cond, f)?;
                walk_calls(body, f)?;
            }
            Stmt::Break { .. } => {}
        }
    }
    if let Some(t) = &b.tail {
        walk_calls_expr(t, f)?;
    }
    Ok(())
}

fn walk_calls_expr(
    e: &Expr,
    f: &mut dyn FnMut(&str, &[Expr], Span) -> DiagResult<()>,
) -> DiagResult<()> {
    match e {
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } | Expr::Ident { .. } => {}
        Expr::Call { func, args, span, .. } => {
            for a in args {
                walk_calls_expr(a, f)?;
            }
            if let Expr::Ident { name, .. } = func.as_ref() {
                f(name, args, *span)?;
            } else {
                walk_calls_expr(func, f)?;
            }
        }
        Expr::Lambda { body, .. } => walk_calls_expr(body, f)?,
        Expr::Binary { left, right, .. } => {
            walk_calls_expr(left, f)?;
            walk_calls_expr(right, f)?;
        }
        Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } => walk_calls_expr(expr, f)?,
        Expr::Block(b) | Expr::On { body: b, .. } => walk_calls(b, f)?,
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    walk_calls_expr(c, f)?;
                }
            }
        }
        Expr::Field { base, .. } => walk_calls_expr(base, f)?,
        Expr::Match { scrutinee, arms, .. } => {
            walk_calls_expr(scrutinee, f)?;
            for arm in arms {
                walk_calls_expr(&arm.body, f)?;
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            walk_calls_expr(cond, f)?;
            walk_calls(then_block, f)?;
            for elsif in elsifs {
                walk_calls_expr(&elsif.cond, f)?;
                walk_calls(&elsif.body, f)?;
            }
            if let Some(b) = else_block {
                walk_calls(b, f)?;
            }
        }
    }
    Ok(())
}

fn mark_mutates(
    f: &FnDef,
    aliases: &HashMap<String, String>,
    modes: &HashMap<String, Vec<ParamMode>>,
    mutates: &HashMap<String, Vec<bool>>,
    local: &mut [bool],
) {
    let _ = walk_calls(&f.body, &mut |name, args, _span| {
        if mutates_container(name) {
            if let Some(i) = args
                .first()
                .and_then(ident_name)
                .and_then(|n| aliases.get(n))
                .and_then(|root| param_index(f, root))
            {
                local[i] = true;
            }
        }
        if let Some(sig) = modes.get(name) {
            for (i, arg) in args.iter().enumerate() {
                let Some(src) = ident_name(arg)
                    .and_then(|n| aliases.get(n))
                    .and_then(|root| param_index(f, root))
                else {
                    continue;
                };
                let mode = sig.get(i).copied().unwrap_or(ParamMode::Inferred);
                let dest_mut = mutates.get(name).and_then(|m| m.get(i)).copied().unwrap_or(false);
                if mode == ParamMode::Take || mode == ParamMode::Keep || dest_mut {
                    local[src] = true;
                }
            }
        }
        Ok(())
    });
}

fn check_keep_uses(
    f: &FnDef,
    aliases: &HashMap<String, String>,
    modes: &HashMap<String, Vec<ParamMode>>,
    mutates: &HashMap<String, Vec<bool>>,
    env: &StringEnv,
) -> DiagResult<()> {
    walk_calls(&f.body, &mut |name, args, span| {
        if matches!(name, "list_push" | "list_set" | "dict_put") {
            let keep = args
                .first()
                .and_then(ident_name)
                .and_then(|n| aliases.get(n))
                .and_then(|root| f.params.iter().find(|p| &p.name == root))
                .is_some_and(|p| p.mode == ParamMode::Keep);
            if keep {
                for arg in args.iter().skip(1) {
                    if let Some(ty) = env.expr_type(arg) {
                        if !type_exportable(ty, &env.structs, &env.enums, 0) {
                            return Err(vec![Diagnostic::new(
                                ErrorCode::EType,
                                "@arena keep cannot store this value",
                                span,
                            )]);
                        }
                    }
                }
            }
        }
        if let Some(sig) = modes.get(name) {
            for (i, arg) in args.iter().enumerate() {
                let Some(root) = ident_name(arg).and_then(|n| aliases.get(n)) else {
                    continue;
                };
                let Some(p) = f.params.iter().find(|p| &p.name == root) else {
                    continue;
                };
                if p.mode != ParamMode::Keep {
                    continue;
                }
                let mode = sig.get(i).copied().unwrap_or(ParamMode::Inferred);
                let dest_mut = mutates.get(name).and_then(|m| m.get(i)).copied().unwrap_or(false);
                if mode == ParamMode::Take || (dest_mut && mode != ParamMode::Keep) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("keep parameter `{}` cannot be passed here", p.name),
                        span,
                    )]);
                }
            }
        }
        Ok(())
    })
}

fn check_arena_params(prog: &Program, infer: &crate::infer::InferOutput) -> DiagResult<()> {
    let env = StringEnv::from_program(prog).with_infer(infer);
    let fns: Vec<&FnDef> = prog
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    // Nothing to check when no function is `@arena` and no parameter is `keep`.
    // The selfhost mirror skips the same way: walking every function there
    // copies the whole program and exhausts memory while compiling itself.
    if !fns.iter().any(|f| f.arena || f.params.iter().any(|p| p.mode == ParamMode::Keep)) {
        return Ok(());
    }
    let modes: HashMap<String, Vec<ParamMode>> = fns
        .iter()
        .map(|f| (f.name.clone(), f.params.iter().map(|p| p.mode).collect()))
        .collect();
    let mut mutates: HashMap<String, Vec<bool>> = fns
        .iter()
        .map(|f| (f.name.clone(), vec![false; f.params.len()]))
        .collect();
    loop {
        let mut changed = false;
        for f in &fns {
            let aliases = bind_aliases(f);
            let mut local = vec![false; f.params.len()];
            mark_mutates(f, &aliases, &modes, &mutates, &mut local);
            if local != mutates[&f.name] {
                mutates.insert(f.name.clone(), local);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for f in &fns {
        for p in &f.params {
            if p.mode == ParamMode::Keep && !f.arena {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "keep parameter requires @arena",
                    p.span,
                )]);
            }
            let container_ty = match &p.ty {
                Type::Ref { inner, .. } => inner.as_ref(),
                t => t,
            };
            if p.mode == ParamMode::Keep && !is_list_or_dict(container_ty) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("keep parameter `{}` must be a List or Dict", p.name),
                    p.span,
                )]);
            }
        }
        if !f.arena {
            continue;
        }
        let aliases = bind_aliases(f);
        let flags = &mutates[&f.name];
        for (i, p) in f.params.iter().enumerate() {
            if p.mode != ParamMode::Keep && (flags[i] || p.mode == ParamMode::Take) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("@arena parameter `{}` is read-only", p.name),
                    p.span,
                )]);
            }
        }
        check_keep_uses(f, &aliases, &modes, &mutates, &env)?;
    }
    Ok(())
}

fn ty_with_span(mut ty: Type, span: Span) -> Type {
    match &mut ty {
        Type::Named { span: s, .. }
        | Type::Qualified { span: s, .. }
        | Type::Tensor { span: s, .. }
        | Type::Fn { span: s, .. }
        | Type::Ref { span: s, .. } => {
            *s = span;
        }
    }
    ty
}
