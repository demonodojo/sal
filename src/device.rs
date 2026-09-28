use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};

pub fn check_devices(prog: &Program) -> DiagResult<()> {
    let mut string_frames: HashSet<String> = HashSet::new();
    for item in &prog.items {
        if let Item::Frame(f) = item {
            if f.columns.iter().any(|c| c.elem == ColumnElem::String) {
                string_frames.insert(f.name.clone());
            }
        }
    }
    for item in &prog.items {
        if let Item::Fn(f) = item {
            check_fn_devices(f, &string_frames)?;
        }
    }
    Ok(())
}

fn check_fn_devices(f: &FnDef, string_frames: &HashSet<String>) -> DiagResult<()> {
    let mut env: HashMap<String, Type> = HashMap::new();
    for p in &f.params {
        env.insert(p.name.clone(), p.ty.clone());
    }
    check_block_devices(&f.body, &mut env, None, string_frames)
}

fn check_block_devices(
    b: &Block,
    env: &mut HashMap<String, Type>,
    expected: Option<Place>,
    string_frames: &HashSet<String>,
) -> DiagResult<()> {
    for st in &b.stmts {
        check_stmt_devices(st, env, expected.clone(), string_frames)?;
    }
    if let Some(t) = &b.tail {
        check_expr_devices(t, env, expected, string_frames)?;
    }
    Ok(())
}

fn check_stmt_devices(
    st: &Stmt,
    env: &mut HashMap<String, Type>,
    expected: Option<Place>,
    string_frames: &HashSet<String>,
) -> DiagResult<()> {
    match st {
        Stmt::Let { name, ty, init, .. } => {
            let ity = check_expr_devices(init, env, expected, string_frames)?;
            env.insert(name.clone(), ty.clone().unwrap_or(ity));
            Ok(())
        }
        Stmt::Expr(e) => {
            check_expr_devices(e, env, expected, string_frames)?;
            Ok(())
        }
        Stmt::Assign { target, value, .. } => {
            let vty = check_expr_devices(value, env, expected.clone(), string_frames)?;
            if let Expr::Ident { name, .. } = target {
                env.insert(name.clone(), vty);
            } else {
                check_expr_devices(target, env, expected, string_frames)?;
            }
            Ok(())
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                check_expr_devices(v, env, expected, string_frames)?;
            }
            Ok(())
        }
        Stmt::While { cond, body, .. } => {
            check_expr_devices(cond, env, expected.clone(), string_frames)?;
            for st in &body.stmts {
                check_stmt_devices(st, env, expected.clone(), string_frames)?;
            }
            if let Some(t) = &body.tail {
                check_expr_devices(t, env, expected, string_frames)?;
            }
            Ok(())
        }
    }
}

fn check_expr_devices(
    e: &Expr,
    env: &HashMap<String, Type>,
    expected: Option<Place>,
    string_frames: &HashSet<String>,
) -> DiagResult<Type> {
    match e {
        Expr::Int { span, .. } => Ok(named("Int", *span)),
        Expr::Float { span, .. } => Ok(named("Float", *span)),
        Expr::Bool { span, .. } => Ok(named("Bool", *span)),
        Expr::String { span, .. } => {
            let ty = named("String", *span);
            reject_string_off_cpu(&ty, expected.as_ref(), *span)?;
            Ok(ty)
        }
        Expr::Ident { name, span } => {
            let ty = env
                .get(name)
                .cloned()
                .unwrap_or_else(|| named("Unknown", *span));
            if let Some(exp) = &expected {
                if let Some(place) = type_place(&ty) {
                    if !places_compatible(&place, exp) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EPlace,
                            format!("tensor `{name}` is on {place:?}, expected {exp:?}"),
                            *span,
                        )
                        .with_hint("use `to` to transfer")]);
                    }
                }
            }
            reject_string_off_cpu(&ty, expected.as_ref(), *span)?;
            Ok(ty)
        }
        Expr::On {
            place,
            kernel,
            kernel_index,
            body,
            span,
        } => {
            if *kernel && matches!(place, Place::Tpu) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EDevice,
                    "TPU does not support `on tpu kernel`",
                    *span,
                )]);
            }
            if kernel_index.is_some() {
                if matches!(place, Place::Cpu | Place::Tpu) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EDevice,
                        "kernel index requires `on gpu`",
                        *span,
                    )]);
                }
            }
            let mut local = env.clone();
            check_block_devices(body, &mut local, Some(place.clone()), string_frames)?;
            let _ = span;
            Ok(named("Unit", body.span))
        }
        Expr::To { place, expr, span } => {
            let ty = check_expr_devices(expr, env, None, string_frames)?;
            if is_string_type(&ty) && !matches!(place, Place::Cpu) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EPlace,
                    "String lives on cpu",
                    *span,
                )
                .with_hint("use `str_bytes` then `to` for device data")]);
            }
            if let Type::Named { name, .. } = &ty {
                if string_frames.contains(name) && !matches!(place, Place::Cpu) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EPlace,
                        "frame with String columns lives on cpu",
                        *span,
                    )]);
                }
            }
            Ok(with_place(ty, place.clone(), *span))
        }
        Expr::Call {
            func,
            args,
            span,
            ..
        } => {
            check_expr_devices(func, env, expected.clone(), string_frames)?;
            let mut arg_tys = Vec::new();
            for a in args {
                arg_tys.push(check_expr_devices(a, env, expected.clone(), string_frames)?);
            }
            if let Some(exp) = &expected {
                for (a, ty) in args.iter().zip(arg_tys.iter()) {
                    if let Some(place) = type_place(ty) {
                        if !places_compatible(&place, exp) {
                            return Err(vec![Diagnostic::new(
                                ErrorCode::EPlace,
                                "tensor place does not match `on` block",
                                a.span(),
                            )
                            .with_hint("use `to` to transfer")]);
                        }
                    }
                }
            }
            // matmul across mismatched places
            if let Expr::Ident { name, .. } = func.as_ref() {
                if name == "matmul" && arg_tys.len() == 2 {
                    if let (Some(pa), Some(pb)) = (type_place(&arg_tys[0]), type_place(&arg_tys[1]))
                    {
                        if !places_compatible(&pa, &pb) {
                            return Err(vec![Diagnostic::new(
                                ErrorCode::EPlace,
                                "matmul tensors must share place",
                                *span,
                            )
                            .with_hint("use `to` to transfer")]);
                        }
                    }
                }
            }
            Ok(arg_tys.first().cloned().unwrap_or_else(|| named("Unknown", *span)))
        }
        Expr::Lambda {
            params,
            body,
            span,
        } => check_lambda_devices(params, body, env, expected, *span, string_frames),
        Expr::Binary { left, right, span, .. } => {
            check_expr_devices(left, env, expected.clone(), string_frames)?;
            check_expr_devices(right, env, expected, string_frames)?;
            Ok(named("Int", *span))
        }
        Expr::Unary { expr, .. } => check_expr_devices(expr, env, expected, string_frames),
        Expr::Block(b) => {
            let mut local = env.clone();
            check_block_devices(b, &mut local, expected, string_frames)?;
            Ok(named("Unit", b.span))
        }
        Expr::TensorLit { rows, span, .. } => {
            for row in rows {
                for c in row {
                    check_expr_devices(c, env, expected.clone(), string_frames)?;
                }
            }
            let ty = Type::Tensor {
                elem: TensorElem::F32,
                dims: vec![],
                place: Place::Cpu,
                span: *span,
            };
            if let Some(exp) = &expected {
                if !places_compatible(&Place::Cpu, exp) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EPlace,
                        "tensor literal lives on cpu",
                        *span,
                    )
                    .with_hint("use `to` to transfer")]);
                }
            }
            Ok(ty)
        }
        Expr::Field { base, span, .. } => {
            check_expr_devices(base, env, expected, string_frames)?;
            Ok(named("Unknown", *span))
        }
        Expr::Match {
            scrutinee,
            arms,
            span,
            ..
        } => {
            check_expr_devices(scrutinee, env, expected.clone(), string_frames)?;
            for arm in arms {
                let mut local = env.clone();
                bind_pattern_names(&arm.pattern, &mut local);
                check_expr_devices(&arm.body, &local, expected.clone(), string_frames)?;
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
            check_expr_devices(cond, env, expected.clone(), string_frames)?;
            let mut then_env = env.clone();
            check_block_devices(then_block, &mut then_env, expected.clone(), string_frames)?;
            for arm in elsifs {
                let mut arm_env = env.clone();
                check_expr_devices(&arm.cond, &arm_env, expected.clone(), string_frames)?;
                check_block_devices(&arm.body, &mut arm_env, expected.clone(), string_frames)?;
            }
            if let Some(else_block) = else_block {
                let mut else_env = env.clone();
                check_block_devices(else_block, &mut else_env, expected, string_frames)?;
            }
            Ok(named("Int", *span))
        }
        Expr::Try { expr, .. } => check_expr_devices(expr, env, expected, string_frames),
    }
}

/// `x => body`: bind scalar param; check body under the same place expectation.
fn check_lambda_devices(
    params: &[String],
    body: &Expr,
    env: &HashMap<String, Type>,
    expected: Option<Place>,
    span: crate::span::Span,
    string_frames: &HashSet<String>,
) -> DiagResult<Type> {
    if params.len() != 1 {
        return Ok(named("Unknown", span));
    }
    let mut local = env.clone();
    let param_ty = named("Float", span);
    local.insert(params[0].clone(), param_ty.clone());
    let body_ty = check_expr_devices(body, &local, expected, string_frames)?;
    Ok(Type::Fn {
        params: vec![param_ty],
        ret: Box::new(body_ty),
        effects: vec![],
        span,
    })
}

fn is_string_type(ty: &Type) -> bool {
    matches!(ty, Type::Named { name, .. } if name == "String")
}

fn reject_string_off_cpu(
    ty: &Type,
    expected: Option<&Place>,
    span: crate::span::Span,
) -> DiagResult<()> {
    if !is_string_type(ty) {
        return Ok(());
    }
    if let Some(exp) = expected {
        if !places_compatible(&Place::Cpu, exp) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EPlace,
                "String lives on cpu",
                span,
            )
            .with_hint("use `str_bytes` then `to` for device data")]);
        }
    }
    Ok(())
}

fn type_place(ty: &Type) -> Option<Place> {
    match ty {
        Type::Tensor { place, .. } => Some(place.clone()),
        _ => None,
    }
}

fn places_compatible(a: &Place, b: &Place) -> bool {
    match (a, b) {
        (Place::Cpu, Place::Cpu) | (Place::Gpu, Place::Gpu) | (Place::Tpu, Place::Tpu) => true,
        (Place::Param(x), Place::Param(y)) => x == y,
        // Place params unify with concrete places at call sites; inside `on p` they match.
        (Place::Param(_), _) | (_, Place::Param(_)) => true,
        _ => false,
    }
}

fn with_place(ty: Type, place: Place, span: crate::span::Span) -> Type {
    match ty {
        Type::Tensor { elem, dims, .. } => Type::Tensor {
            elem,
            dims,
            place,
            span,
        },
        other => other,
    }
}

fn named(name: &str, span: crate::span::Span) -> Type {
    Type::Named {
        name: name.into(),
        args: vec![],
        span,
    }
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
