use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::string_expr::{add_chain_leftmost, is_string_type};
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::span::Span;

pub fn check_ownership(prog: &Program) -> DiagResult<()> {
    for item in &prog.items {
        if let Item::Fn(f) = item {
            check_fn_moves(f)?;
        }
    }
    Ok(())
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
    for item in &mut out.items {
        if let Item::Fn(f) = item {
            infer_fn_modes(f, &structs);
        }
    }
    out
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
        if taken.contains(&p.name) {
            p.mode = ParamMode::Take;
        } else if used.contains(&p.name) {
            p.mode = ParamMode::Borrow;
        } else {
            // Unused unique: ownership is taken and the value is dropped.
            p.mode = ParamMode::Take;
        }
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
    track_block(
        b,
        env,
        moved,
        &HashSet::new(),
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut HashSet::new(),
    )
}

fn track_block(
    b: &Block,
    env: &mut HashMap<String, Type>,
    moved: &mut HashSet<String>,
    params: &HashSet<String>,
    taken: &mut HashSet<String>,
    used: &mut HashSet<String>,
    frozen: &mut HashSet<String>,
) -> DiagResult<()> {
    for st in &b.stmts {
        track_stmt(st, env, moved, params, taken, used, frozen)?;
    }
    if let Some(t) = &b.tail {
        let ty = track_expr(t, env, moved, params, taken, used, frozen)?;
        // Tail value escapes the block (function result or block expr) → move unique.
        consume_if_unique(t, &ty, moved, params, taken);
    }
    Ok(())
}

fn track_stmt(
    st: &Stmt,
    env: &mut HashMap<String, Type>,
    moved: &mut HashSet<String>,
    params: &HashSet<String>,
    taken: &mut HashSet<String>,
    used: &mut HashSet<String>,
    frozen: &mut HashSet<String>,
) -> DiagResult<()> {
    match st {
        Stmt::Let {
            name,
            ty,
            init,
            frozen: is_frozen,
            ..
        } => {
            let ity = track_expr(init, env, moved, params, taken, used, frozen)?;
            consume_if_unique(init, &ity, moved, params, taken);
            let bound = ty.clone().unwrap_or(ity);
            env.insert(name.clone(), bound);
            moved.remove(name);
            if *is_frozen {
                frozen.insert(name.clone());
            }
        }
        Stmt::Expr(e) => {
            track_expr(e, env, moved, params, taken, used, frozen)?;
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
            consume_if_unique(value, &vty, moved, params, taken);
            if let Expr::Ident { name, .. } = target {
                // `x = x + ...` appends in place: the old `x` is moved into the result.
                if is_string_type(&vty) && matches!(value, Expr::Binary { op: BinOp::Add, .. }) {
                    if let Expr::Ident { name: leaf, .. } = add_chain_leftmost(value) {
                        if leaf == name {
                            consume_if_unique(add_chain_leftmost(value), &vty, moved, params, taken);
                        }
                    }
                }
                env.insert(name.clone(), vty);
                moved.remove(name);
            } else {
                track_expr(target, env, moved, params, taken, used, frozen)?;
                let _ = span;
            }
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                let ty = track_expr(v, env, moved, params, taken, used, frozen)?;
                consume_if_unique(v, &ty, moved, params, taken);
            }
        }
        Stmt::While { cond, body, .. } => {
            track_expr(cond, env, moved, params, taken, used, frozen)?;
            track_block(body, env, moved, params, taken, used, frozen)?;
        }
    }
    Ok(())
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
                        consume_if_unique(a, ty, moved, params, taken);
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
            }
            Ok(named("Unknown", *span))
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
        Expr::Unary { expr, .. } => track_expr(expr, env, moved, params, taken, used, frozen),
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
            consume_if_unique(expr, &ty, moved, params, taken);
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
        Expr::Field { base, span, .. } => {
            track_expr(base, env, moved, params, taken, used, frozen)?;
            Ok(named("Unknown", *span))
        }
        Expr::Match {
            scrutinee, arms, span, ..
        } => {
            track_expr(scrutinee, env, moved, params, taken, used, frozen)?;
            for arm in arms {
                let mut local = env.clone();
                bind_pattern_names(&arm.pattern, &mut local);
                track_expr(&arm.body, &local, moved, params, taken, used, frozen)?;
            }
            Ok(named("Unknown", *span))
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
            let mut then_moved = branch_base.clone();
            let mut then_env = env.clone();
            for st in &then_block.stmts {
                track_stmt(
                    st,
                    &mut then_env,
                    &mut then_moved,
                    params,
                    taken,
                    used,
                    frozen,
                )?;
            }
            if let Some(t) = &then_block.tail {
                track_expr(t, &then_env, &mut then_moved, params, taken, used, frozen)?;
            }
            for arm in elsifs {
                track_expr(&arm.cond, env, moved, params, taken, used, frozen)?;
                let mut arm_moved = branch_base.clone();
                let mut arm_env = env.clone();
                for st in &arm.body.stmts {
                    track_stmt(
                        st,
                        &mut arm_env,
                        &mut arm_moved,
                        params,
                        taken,
                        used,
                        frozen,
                    )?;
                }
                if let Some(t) = &arm.body.tail {
                    track_expr(t, &arm_env, &mut arm_moved, params, taken, used, frozen)?;
                }
            }
            if let Some(else_block) = else_block {
                let mut else_moved = branch_base.clone();
                let mut else_env = env.clone();
                for st in &else_block.stmts {
                    track_stmt(
                        st,
                        &mut else_env,
                        &mut else_moved,
                        params,
                        taken,
                        used,
                        frozen,
                    )?;
                }
                if let Some(t) = &else_block.tail {
                    track_expr(t, &else_env, &mut else_moved, params, taken, used, frozen)?;
                }
            }
            Ok(named("Int", *span))
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

fn consume_if_unique(
    e: &Expr,
    ty: &Type,
    moved: &mut HashSet<String>,
    params: &HashSet<String>,
    taken: &mut HashSet<String>,
) {
    if is_copy_type(ty) {
        return;
    }
    if let Expr::Ident { name, .. } = e {
        moved.insert(name.clone());
        if params.contains(name) {
            taken.insert(name.clone());
        }
    }
}

fn is_copy_type(ty: &Type) -> bool {
    match ty {
        Type::Named { name, .. } | Type::Qualified { name, .. } => {
            matches!(name.as_str(), "Int" | "Float" | "Bool" | "Unit" | "Unknown")
        }
        Type::Tensor { .. } | Type::Fn { .. } => false,
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
    }
}

fn is_move_fn(name: &str) -> bool {
    matches!(name, "save" | "reshape" | "transpose" | "to_cpu" | "to_gpu")
}

fn is_borrow_fn(name: &str) -> bool {
    matches!(
        name,
        "matmul"
            | "relu"
            | "softmax"
            | "map"
            | "reduce"
            | "print"
            | "print_str"
            | "eprint_str"
            | "getenv"
            | "mkdir_p"
            | "str_eq"
            | "str_contains"
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
            | "list_push"
            | "list_get"
            | "list_len"
    )
}

fn bind_pattern_names(pat: &Pattern, env: &mut HashMap<String, Type>) {
    match pat {
        Pattern::Wild(_) | Pattern::Int(_, _) => {}
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

fn ty_with_span(mut ty: Type, span: Span) -> Type {
    match &mut ty {
        Type::Named { span: s, .. }
        | Type::Qualified { span: s, .. }
        | Type::Tensor { span: s, .. }
        | Type::Fn { span: s, .. } => {
            *s = span;
        }
    }
    ty
}
