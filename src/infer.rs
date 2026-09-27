use std::collections::HashMap;

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::span::Span;
use crate::string_expr::is_string_type;
use crate::typed::ExprTypeEntry;

pub struct InferOutput {
    pub expr_types_by_fn: Vec<(FnDef, Vec<ExprTypeEntry>)>,
}

#[derive(Default, Clone)]
pub struct TypeEnv {
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    /// Declared function signatures (name → params + return), for user calls.
    pub fns: HashMap<String, (Vec<Type>, Type)>,
}


pub fn infer_program(prog: &Program) -> DiagResult<InferOutput> {
    let mut types = TypeEnv::default();
    for item in &prog.items {
        match item {
            Item::Struct(s) => {
                types.structs.insert(s.name.clone(), s.clone());
            }
            Item::Enum(e) => {
                types.enums.insert(e.name.clone(), e.clone());
            }
            Item::Fn(f) => {
                let params: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
                types.fns.insert(f.name.clone(), (params, f.ret.clone()));
            }
            _ => {}
        }
    }
    infer_program_with_env(prog, &types)
}

pub fn infer_program_with_env(prog: &Program, types: &TypeEnv) -> DiagResult<InferOutput> {
    let mut out = Vec::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            let entries = infer_fn(f, types)?;
            out.push((f.clone(), entries));
        }
    }
    Ok(InferOutput {
        expr_types_by_fn: out,
    })
}

fn infer_fn(f: &FnDef, types: &TypeEnv) -> DiagResult<Vec<ExprTypeEntry>> {
    let mut env: HashMap<String, Type> = HashMap::new();
    for p in &f.params {
        check_type_well_formed(&p.ty)?;
        env.insert(p.name.clone(), p.ty.clone());
    }
    check_type_well_formed(&f.ret)?;
    let mut entries = Vec::new();
    check_block(&f.body, &mut env, &f.ret, types, &mut entries)?;
    Ok(entries)
}

/// Reject illegal tensor element types if they appear in AST (parser also checks).
fn check_type_well_formed(ty: &Type) -> DiagResult<()> {
    match ty {
        Type::Named { name, args, span } => {
            if name == "Tensor" {
                // Named Tensor without going through Type::Tensor — treat elem arg.
                if let Some(Type::Named { name: elem, span: es, .. }) = args.first() {
                    if !matches!(elem.as_str(), "F32" | "F16" | "BF16" | "I8") {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::ETensorelem,
                            format!("`{elem}` is not a tensor element type (Float is f64)"),
                            *es,
                        )]);
                    }
                }
                let _ = span;
            }
            for a in args {
                check_type_well_formed(a)?;
            }
            Ok(())
        }
        Type::Tensor { dims: _, .. } => Ok(()),
        Type::Fn {
            params, ret, ..
        } => {
            for p in params {
                check_type_well_formed(p)?;
            }
            check_type_well_formed(ret)
        }
    }
}

fn check_block(
    b: &Block,
    env: &mut HashMap<String, Type>,
    expected: &Type,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
) -> DiagResult<()> {
    for st in &b.stmts {
        check_stmt(st, env, types, entries)?;
    }
    if let Some(t) = &b.tail {
        let ty = check_expr(t, env, types, entries)?;
        if !types_compatible(&ty, expected) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("expected {}, found {}", type_name(expected), type_name(&ty)),
                t.span(),
            )]);
        }
    }
    Ok(())
}

fn check_stmt(
    st: &Stmt,
    env: &mut HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
) -> DiagResult<()> {
    match st {
        Stmt::Let { name, ty, init, .. } => {
            let ity = check_expr(init, env, types, entries)?;
            if let Some(decl) = ty {
                check_type_well_formed(decl)?;
                if !types_compatible(&ity, decl) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "let binding type mismatch",
                        init.span(),
                    )]);
                }
            }
            env.insert(name.clone(), ty.clone().unwrap_or(ity));
        }
        Stmt::Expr(e) => {
            check_expr(e, env, types, entries)?;
        }
        Stmt::Assign { target, value, .. } => {
            let vty = check_expr(value, env, types, entries)?;
            if let Expr::Ident { name, .. } = target {
                // Bare `name = expr` introduces or rebinds (surface without `let`).
                env.insert(name.clone(), vty);
            } else {
                check_expr(target, env, types, entries)?;
            }
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                check_expr(v, env, types, entries)?;
            }
        }
        Stmt::While { cond, body, .. } => {
            check_expr(cond, env, types, entries)?;
            for st in &body.stmts {
                check_stmt(st, env, types, entries)?;
            }
            if let Some(t) = &body.tail {
                check_expr(t, env, types, entries)?;
            }
        }
    }
    Ok(())
}

fn check_expr(
    e: &Expr,
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
) -> DiagResult<Type> {
    let ty = match e {
        Expr::Int { .. } => Type::Named {
            name: "Int".into(),
            args: vec![],
            span: e.span(),
        },
        Expr::Float { .. } => Type::Named {
            name: "Float".into(),
            args: vec![],
            span: e.span(),
        },
        Expr::Bool { .. } => Type::Named {
            name: "Bool".into(),
            args: vec![],
            span: e.span(),
        },
        Expr::String { .. } => Type::Named {
            name: "String".into(),
            args: vec![],
            span: e.span(),
        },
        Expr::Ident { name, span } => env.get(name).cloned().ok_or_else(|| {
            vec![Diagnostic::new(
                ErrorCode::EType,
                format!("unknown variable `{name}`"),
                *span,
            )]
        })?,
        Expr::Binary {
            op,
            left,
            right,
            span,
        } => {
            let lt = check_expr(left, env, types, entries)?;
            let rt = check_expr(right, env, types, entries)?;
            match op {
                BinOp::Add if is_string_type(&lt) && is_string_type(&rt) => Type::Named {
                    name: "String".into(),
                    args: vec![],
                    span: *span,
                },
                BinOp::Add if is_string_type(&lt) || is_string_type(&rt) => {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "string concatenation requires two String values",
                        *span,
                    )]);
                }
                _ => {
                    if !is_numeric(&lt) || !is_numeric(&rt) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            "numeric operation requires Int or Float",
                            *span,
                        )]);
                    }
                    lt
                }
            }
        }
        Expr::Unary { expr, .. } => check_expr(expr, env, types, entries)?,
        Expr::Call {
            func,
            type_args,
            args,
            span,
        } => infer_call(func, type_args, args, env, types, entries, *span)?,
        Expr::Lambda {
            params,
            body,
            span,
        } => infer_lambda(params, body, None, env, types, entries, *span)?,
        Expr::Block(b) => {
            let mut local = env.clone();
            for st in &b.stmts {
                check_stmt(st, &mut local, types, entries)?;
            }
            if let Some(t) = &b.tail {
                check_expr(t, &local, types, entries)?
            } else {
                Type::Named {
                    name: "Unit".into(),
                    args: vec![],
                    span: b.span,
                }
            }
        }
        Expr::On {
            body,
            kernel_index,
            ..
        } => {
            let mut local = env.clone();
            if let Some(ki) = kernel_index {
                for n in &ki.names {
                    local.insert(
                        n.clone(),
                        Type::Named {
                            name: "Int".into(),
                            args: vec![],
                            span: ki.span,
                        },
                    );
                }
            }
            for st in &body.stmts {
                check_stmt(st, &mut local, types, entries)?;
            }
            if let Some(t) = &body.tail {
                check_expr(t, &local, types, entries)?
            } else {
                Type::Named {
                    name: "Unit".into(),
                    args: vec![],
                    span: body.span,
                }
            }
        }
        Expr::To { place, expr, span } => {
            let ty = check_expr(expr, env, types, entries)?;
            match ty {
                Type::Tensor { elem, dims, .. } => Type::Tensor {
                    elem,
                    dims,
                    place: place.clone(),
                    span: *span,
                },
                other => other,
            }
        }
        Expr::TensorLit { rows, span } => {
            let nrows = rows.len() as u64;
            let ncols = rows.first().map(|r| r.len() as u64).unwrap_or(0);
            for row in rows {
                for c in row {
                    check_expr(c, env, types, entries)?;
                }
            }
            let dims = if nrows == 0 {
                vec![]
            } else if rows.iter().all(|r| r.len() as u64 == ncols) {
                vec![Dim::Static(nrows), Dim::Static(ncols)]
            } else {
                vec![Dim::Static(nrows)]
            };
            Type::Tensor {
                elem: TensorElem::F32,
                dims,
                place: Place::Cpu,
                span: *span,
            }
        }
        Expr::Field { base, field, span } => {
            let base_ty = check_expr(base, env, types, entries)?;
            resolve_field(&base_ty, field, types, *span)?
        }
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => {
            let scrut_ty = check_expr(scrutinee, env, types, entries)?;
            let mut arm_tys = Vec::new();
            for arm in arms {
                let mut local = env.clone();
                bind_pattern(&arm.pattern, &scrut_ty, types, &mut local)?;
                arm_tys.push(check_expr(&arm.body, &local, types, entries)?);
            }
            arm_tys.first().cloned().unwrap_or_else(|| Type::Named {
                name: "Unknown".into(),
                args: vec![],
                span: *span,
            })
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            span,
        } => {
            check_expr(cond, env, types, entries)?;
            let mut then_env = env.clone();
            for st in &then_block.stmts {
                check_stmt(st, &mut then_env, types, entries)?;
            }
            for arm in elsifs {
                check_expr(&arm.cond, env, types, entries)?;
                let mut arm_env = env.clone();
                for st in &arm.body.stmts {
                    check_stmt(st, &mut arm_env, types, entries)?;
                }
                if let Some(t) = &arm.body.tail {
                    check_expr(t, &arm_env, types, entries)?;
                }
            }
            if let Some(else_block) = else_block {
                let then_ty = if let Some(t) = &then_block.tail {
                    check_expr(t, &then_env, types, entries)?
                } else {
                    Type::Named {
                        name: "Int".into(),
                        args: vec![],
                        span: *span,
                    }
                };
                let mut else_env = env.clone();
                for st in &else_block.stmts {
                    check_stmt(st, &mut else_env, types, entries)?;
                }
                if let Some(t) = &else_block.tail {
                    check_expr(t, &else_env, types, entries)?;
                }
                then_ty
            } else {
                if let Some(t) = &then_block.tail {
                    check_expr(t, &then_env, types, entries)?;
                }
                Type::Named {
                    name: "Unit".into(),
                    args: vec![],
                    span: *span,
                }
            }
        }
        Expr::Try { expr, .. } => check_expr(expr, env, types, entries)?,
    };
    entries.push(ExprTypeEntry {
        span_start: e.span().start,
        span_end: e.span().end,
        ty: ty.clone(),
    });
    Ok(ty)
}

fn resolve_field(
    base_ty: &Type,
    field: &str,
    types: &TypeEnv,
    span: Span,
) -> DiagResult<Type> {
    let Type::Named { name, args, .. } = base_ty else {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("type `{base}` has no field `{field}`", base = type_name(base_ty)),
            span,
        )]);
    };
    let Some(def) = types.structs.get(name) else {
        if name == "Unknown" {
            return Ok(Type::Named {
                name: "Unknown".into(),
                args: vec![],
                span,
            });
        }
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("type `{name}` has no field `{field}`"),
            span,
        )]);
    };
    let Some(f) = def.fields.iter().find(|f| f.name == field) else {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("struct `{name}` has no field `{field}`"),
            span,
        )]);
    };
    Ok(subst_type_params(&f.ty, &def.type_params, args, span))
}

fn bind_pattern(
    pat: &Pattern,
    scrut_ty: &Type,
    types: &TypeEnv,
    env: &mut HashMap<String, Type>,
) -> DiagResult<()> {
    match pat {
        Pattern::Wild(_) | Pattern::Int(_, _) => Ok(()),
        Pattern::Ident(name, span) => {
            // Bare idents that name a unit variant of the scrutinee enum are
            // variant patterns (parser has no separate unit-variant form).
            if let Type::Named {
                name: enum_name, ..
            } = scrut_ty
            {
                if let Some(edef) = types.enums.get(enum_name) {
                    if let Some(vdef) = edef.variants.iter().find(|v| v.name == *name) {
                        if !vdef.fields.is_empty() {
                            return Err(vec![Diagnostic::new(
                                ErrorCode::EType,
                                format!(
                                    "variant `{name}` expects {} field(s)",
                                    vdef.fields.len()
                                ),
                                *span,
                            )]);
                        }
                        return Ok(());
                    }
                }
            }
            env.insert(name.clone(), scrut_ty.clone());
            Ok(())
        }
        Pattern::Variant {
            name: variant,
            args,
            span,
        } => {
            let Type::Named {
                name: enum_name,
                args: type_args,
                ..
            } = scrut_ty
            else {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("cannot match variant `{variant}` on non-enum type"),
                    *span,
                )]);
            };
            let Some(edef) = types.enums.get(enum_name) else {
                // Scrutinee is a named type we do not know as an enum — leave unbound.
                return Ok(());
            };
            let Some(vdef) = edef.variants.iter().find(|v| v.name == *variant) else {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("enum `{enum_name}` has no variant `{variant}`"),
                    *span,
                )]);
            };
            if args.len() != vdef.fields.len() {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!(
                        "variant `{variant}` expects {} field(s), found {}",
                        vdef.fields.len(),
                        args.len()
                    ),
                    *span,
                )]);
            }
            for (arg_pat, field_ty) in args.iter().zip(vdef.fields.iter()) {
                let ty = subst_type_params(field_ty, &edef.type_params, type_args, *span);
                bind_pattern(arg_pat, &ty, types, env)?;
            }
            Ok(())
        }
    }
}

fn subst_type_params(ty: &Type, params: &[String], args: &[Type], span: Span) -> Type {
    match ty {
        Type::Named {
            name,
            args: nested,
            ..
        } => {
            if let Some(i) = params.iter().position(|p| p == name) {
                if let Some(a) = args.get(i) {
                    return ty_with_span(a.clone(), span);
                }
            }
            Type::Named {
                name: name.clone(),
                args: nested
                    .iter()
                    .map(|a| subst_type_params(a, params, args, span))
                    .collect(),
                span,
            }
        }
        Type::Tensor {
            elem,
            dims,
            place,
            ..
        } => Type::Tensor {
            elem: *elem,
            dims: dims.clone(),
            place: place.clone(),
            span,
        },
        Type::Fn {
            params: fps,
            ret,
            effects,
            ..
        } => Type::Fn {
            params: fps
                .iter()
                .map(|p| subst_type_params(p, params, args, span))
                .collect(),
            ret: Box::new(subst_type_params(ret, params, args, span)),
            effects: effects.clone(),
            span,
        },
    }
}

fn ty_with_span(mut ty: Type, span: Span) -> Type {
    match &mut ty {
        Type::Named { span: s, .. } | Type::Tensor { span: s, .. } | Type::Fn { span: s, .. } => {
            *s = span;
        }
    }
    ty
}

fn infer_call(
    func: &Expr,
    type_args: &[TypeArg],
    args: &[Expr],
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    // Only direct named calls; lambdas are `Expr::Lambda`, not calls.
    let name = match func {
        Expr::Ident { name, .. } => name.as_str(),
        _ => {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "only direct calls supported",
                span,
            )])
        }
    };
    for a in type_args {
        if let TypeArg::Type(ty) = a {
            check_type_well_formed(ty)?;
        }
    }
    match name {
        "relu" | "softmax" | "reshape" | "transpose" => {
            let mut arg_tys = Vec::new();
            for a in args {
                arg_tys.push(check_expr(a, env, types, entries)?);
            }
            if let Some(t) = arg_tys.first() {
                Ok(t.clone())
            } else {
                Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "missing tensor arg",
                    span,
                )])
            }
        }
        "matmul" => {
            let mut arg_tys = Vec::new();
            for a in args {
                arg_tys.push(check_expr(a, env, types, entries)?);
            }
            if arg_tys.len() != 2 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "matmul expects two tensors",
                    span,
                )]);
            }
            matmul_result_type(&arg_tys[0], &arg_tys[1], span)
        }
        "load" => {
            if type_args.is_empty() {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "load requires tensor type arguments",
                    span,
                )]);
            }
            let mut arg_tys = Vec::new();
            for a in args {
                arg_tys.push(check_expr(a, env, types, entries)?);
            }
            let _ = arg_tys;
            if let Some(TypeArg::Type(Type::Tensor { .. })) = type_args.first() {
                match &type_args[0] {
                    TypeArg::Type(ty) => Ok(ty.clone()),
                    TypeArg::Dim(_) => unreachable!(),
                }
            } else {
                load_tensor_type(type_args, span)
            }
        }
        "map" | "reduce" => infer_map_like(args, env, types, entries, span),
        "print" | "print_str" | "eprint_str" | "argc" | "str_eq" | "str_contains" | "str_len"
        | "str_char" | "str_skip" | "str_hash" | "map_get" | "map_put" | "not" | "free" | "copy_file" | "copy_self" | "gated_print_str"
        | "gated_copy_self" | "write_file" | "path_readable" | "mkdir_p" | "vec_push" | "vec_get" | "vec_set"
        | "vec_len" | "vec_free" | "clang" | "clang_obj" | "link_objs" | "exec_compile"
        | "write_png"
        | "gated_exec_compile" | "place_matmul_ai_sum" | "place_launches" => {
            for a in args {
                check_expr(a, env, types, entries)?;
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "argv" | "read_file" | "str_concat" | "str_append" | "str_slice" | "int_to_str"
        | "char_to_str" | "strdup" | "select_str" | "tmp_path" | "exec_capture" | "getenv"
        | "realpath" | "ir_text" => {
            for a in args {
                check_expr(a, env, types, entries)?;
            }
            Ok(Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            })
        }
        "vec_new" | "image_new" | "map_new" | "lex_src" => {
            for a in args {
                check_expr(a, env, types, entries)?;
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "list_new" => {
            for a in args {
                check_expr(a, env, types, entries)?;
            }
            Ok(list_type(
                Type::Named {
                    name: "Int".into(),
                    args: vec![],
                    span,
                },
                span,
            ))
        }
        "list_push" => infer_list_push(args, env, types, entries, span),
        "list_get" => infer_list_get(args, env, types, entries, span),
        "list_len" => {
            if let Some(a) = args.first() {
                check_expr(a, env, types, entries)?;
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "dict_new" => infer_dict_new(type_args, span),
        "dict_put" => infer_dict_put(args, env, types, entries, span),
        "dict_get" => infer_dict_get(args, env, types, entries, span),
        _ => {
            for a in args {
                check_expr(a, env, types, entries)?;
            }
            if let Some((_params, ret)) = types.fns.get(name) {
                Ok(ret.clone())
            } else {
                Ok(Type::Named {
                    name: "Unknown".into(),
                    args: vec![],
                    span,
                })
            }
        }
    }
}

/// One-parameter lambda `x => body`.
fn infer_lambda(
    params: &[String],
    body: &Expr,
    param_ty_hint: Option<Type>,
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if params.len() != 1 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "lambda expects one parameter and a body",
            span,
        )]);
    }
    let param_name = params[0].clone();
    let param_ty = param_ty_hint.unwrap_or_else(|| Type::Named {
        name: "Float".into(),
        args: vec![],
        span,
    });
    let mut local = env.clone();
    local.insert(param_name, param_ty.clone());
    let body_ty = check_expr(body, &local, types, entries)?;
    Ok(Type::Fn {
        params: vec![param_ty],
        ret: Box::new(body_ty),
        effects: vec![],
        span,
    })
}

fn infer_map_like(
    args: &[Expr],
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.is_empty() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "missing tensor arg",
            span,
        )]);
    }
    let tensor_ty = check_expr(&args[0], env, types, entries)?;
    let elem_ty = tensor_elem_scalar(&tensor_ty, args[0].span());
    if let Some(cb) = args.get(1) {
        if let Expr::Lambda {
            params,
            body,
            span: lspan,
        } = cb
        {
            let fty = infer_lambda(params, body, Some(elem_ty), env, types, entries, *lspan)?;
            entries.push(ExprTypeEntry {
                span_start: fty.span().start,
                span_end: fty.span().end,
                ty: fty,
            });
        } else {
            check_expr(cb, env, types, entries)?;
        }
    }
    for a in args.iter().skip(2) {
        check_expr(a, env, types, entries)?;
    }
    Ok(tensor_ty)
}

fn tensor_elem_scalar(ty: &Type, span: Span) -> Type {
    let name = match ty {
        Type::Tensor {
            elem: TensorElem::I8,
            ..
        } => "Int",
        Type::Tensor { .. } => "Float",
        _ => "Float",
    };
    Type::Named {
        name: name.into(),
        args: vec![],
        span,
    }
}

fn load_tensor_type(type_args: &[TypeArg], span: Span) -> DiagResult<Type> {
    let elem = match type_args.first() {
        Some(TypeArg::Type(Type::Named { name, span: es, .. })) => match name.as_str() {
            "F32" => TensorElem::F32,
            "F16" => TensorElem::F16,
            "BF16" => TensorElem::BF16,
            "I8" => TensorElem::I8,
            "Float" | "float" | "f64" => {
                return Err(vec![Diagnostic::new(
                    ErrorCode::ETensorelem,
                    format!("`{name}` is not a tensor element type (Float is f64)"),
                    *es,
                )]);
            }
            other => {
                return Err(vec![Diagnostic::new(
                    ErrorCode::ETensorelem,
                    format!("`{other}` is not a tensor element type"),
                    *es,
                )]);
            }
        },
        _ => {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "load requires element type",
                span,
            )]);
        }
    };
    let mut dims = Vec::new();
    for a in type_args.iter().skip(1) {
        match a {
            TypeArg::Dim(d) => dims.push(d.clone()),
            TypeArg::Type(Type::Named { name, .. }) if name == "?" => dims.push(Dim::Dynamic),
            TypeArg::Type(Type::Named { name, .. }) => {
                if let Ok(n) = name.parse::<u64>() {
                    dims.push(Dim::Static(n));
                }
            }
            _ => {}
        }
    }
    Ok(Type::Tensor {
        elem,
        dims,
        place: Place::Cpu,
        span,
    })
}

fn matmul_result_type(a: &Type, b: &Type, span: Span) -> DiagResult<Type> {
    let (a_dims, elem, place) = tensor_dims(a).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EShape,
            "matmul left must be tensor",
            span,
        )]
    })?;
    let (b_dims, elem2, place2) = tensor_dims(b).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EShape,
            "matmul right must be tensor",
            span,
        )]
    })?;
    if elem != elem2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "matmul element mismatch",
            span,
        )]);
    }
    if !place_compatible(&place, &place2) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EPlace,
            "matmul tensors must share place",
            span,
        )]);
    }
    if a_dims.len() < 2 || b_dims.len() < 2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "matmul requires rank >= 2",
            span,
        )]);
    }
    let k1 = &a_dims[a_dims.len() - 1];
    let k2 = &b_dims[b_dims.len() - 2];
    if let (Dim::Static(x), Dim::Static(y)) = (k1, k2) {
        if x != y {
            return Err(vec![Diagnostic::new(
                ErrorCode::EShape,
                format!("inner dimensions {x} and {y} do not match"),
                span,
            )]);
        }
    }
    let mut out_dims = a_dims[..a_dims.len() - 1].to_vec();
    out_dims.extend_from_slice(&b_dims[b_dims.len() - 1..]);
    Ok(Type::Tensor {
        elem,
        dims: out_dims,
        place,
        span,
    })
}

fn tensor_dims(t: &Type) -> Option<(Vec<Dim>, TensorElem, Place)> {
    match t {
        Type::Tensor {
            elem,
            dims,
            place,
            ..
        } => Some((dims.clone(), *elem, place.clone())),
        _ => None,
    }
}

fn place_compatible(a: &Place, b: &Place) -> bool {
    match (a, b) {
        (Place::Cpu, Place::Cpu) | (Place::Gpu, Place::Gpu) | (Place::Tpu, Place::Tpu) => true,
        (Place::Param(x), Place::Param(y)) => x == y,
        (Place::Param(_), _) | (_, Place::Param(_)) => true,
        _ => false,
    }
}

fn is_numeric(t: &Type) -> bool {
    matches!(
        t,
        Type::Named { name, .. } if name == "Int" || name == "Float"
    )
}

fn types_compatible(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Tensor { .. }, Type::Tensor { .. }) => true,
        (
            Type::Named {
                name: na,
                args: aa,
                ..
            },
            Type::Named {
                name: nb,
                args: ab,
                ..
            },
        ) => {
            if na == "Unknown" || nb == "Unknown" {
                return true;
            }
            if na != nb {
                if (na == "Int" && nb == "String") || (na == "String" && nb == "Int") {
                    return true;
                }
                return false;
            }
            if aa.len() != ab.len() {
                return aa.is_empty() || ab.is_empty();
            }
            aa.iter()
                .zip(ab.iter())
                .all(|(x, y)| types_compatible(x, y))
        }
        _ => type_name(a) == type_name(b) || type_name(a) == "Unknown" || type_name(b) == "Unknown",
    }
}

fn list_type(elem: Type, span: Span) -> Type {
    Type::Named {
        name: "List".into(),
        args: vec![elem],
        span,
    }
}

fn list_elem(ty: &Type) -> Option<Type> {
    match ty {
        Type::Named { name, args, span } if name == "List" => {
            if args.len() == 1 {
                Some(args[0].clone())
            } else {
                Some(Type::Named {
                    name: "Int".into(),
                    args: vec![],
                    span: *span,
                })
            }
        }
        _ => None,
    }
}

fn dict_kv(ty: &Type) -> Option<(Type, Type)> {
    match ty {
        Type::Named { name, args, span } if name == "Dict" && args.len() == 2 => {
            Some((args[0].clone(), args[1].clone()))
        }
        Type::Named { name, span, .. } if name == "Dict" => Some((
            Type::Named {
                name: "String".into(),
                args: vec![],
                span: *span,
            },
            Type::Named {
                name: "Int".into(),
                args: vec![],
                span: *span,
            },
        )),
        _ => None,
    }
}

fn is_copy_val_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named { name, .. } if name == "Int" || name == "Float" || name == "Bool"
    )
}

fn is_dict_key_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named { name, .. } if name == "Int" || name == "String"
    )
}

fn option_type(inner: Type, span: Span) -> Type {
    Type::Named {
        name: "Option".into(),
        args: vec![inner],
        span,
    }
}

fn infer_list_push(
    args: &[Expr],
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let xs_ty = if let Some(a) = args.first() {
        check_expr(a, env, types, entries)?
    } else {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "list_push expects a list and an element",
            span,
        )]);
    };
    let elem = list_elem(&xs_ty).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "list_push expects a List",
            span,
        )]
    })?;
    if let Some(x) = args.get(1) {
        let xty = check_expr(x, env, types, entries)?;
        if !types_compatible(&xty, &elem) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!(
                    "list element type mismatch: expected {}, found {}",
                    type_name(&elem),
                    type_name(&xty)
                ),
                x.span(),
            )]);
        }
    }
    Ok(xs_ty)
}

fn infer_list_get(
    args: &[Expr],
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if let Some(a) = args.first() {
        let xs_ty = check_expr(a, env, types, entries)?;
        if let Some(elem) = list_elem(&xs_ty) {
            if args.get(1).is_some() {
                check_expr(&args[1], env, types, entries)?;
            }
            return Ok(elem);
        }
    }
    Err(vec![Diagnostic::new(
        ErrorCode::EType,
        "list_get expects a List",
        span,
    )])
}

fn infer_dict_new(type_args: &[TypeArg], span: Span) -> DiagResult<Type> {
    if type_args.len() >= 2 {
        let k = match &type_args[0] {
            TypeArg::Type(t) => t.clone(),
            TypeArg::Dim(_) => {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "Dict key type must be Int or String",
                    span,
                )])
            }
        };
        let v = match &type_args[1] {
            TypeArg::Type(t) => t.clone(),
            TypeArg::Dim(_) => {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "Dict value type must be copy",
                    span,
                )])
            }
        };
        if !is_dict_key_type(&k) || !is_copy_val_type(&v) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "Dict[K, V]: K is Int or String; V is Int, Float or Bool",
                span,
            )]);
        }
        return Ok(Type::Named {
            name: "Dict".into(),
            args: vec![k, v],
            span,
        });
    }
    Ok(Type::Named {
        name: "Dict".into(),
        args: vec![
            Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            },
            Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            },
        ],
        span,
    })
}

fn infer_dict_put(
    args: &[Expr],
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let dty = if let Some(a) = args.first() {
        check_expr(a, env, types, entries)?
    } else {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "dict_put expects dict, key and value",
            span,
        )]);
    };
    let (kty, vty) = dict_kv(&dty).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "dict_put expects a Dict",
            span,
        )]
    })?;
    if let Some(key) = args.get(1) {
        let kt = check_expr(key, env, types, entries)?;
        if !types_compatible(&kt, &kty) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "dict key type mismatch",
                key.span(),
            )]);
        }
    }
    if let Some(val) = args.get(2) {
        let vt = check_expr(val, env, types, entries)?;
        if !types_compatible(&vt, &vty) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "dict value type mismatch",
                val.span(),
            )]);
        }
    }
    Ok(dty)
}

fn infer_dict_get(
    args: &[Expr],
    env: &HashMap<String, Type>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if !types.enums.contains_key("Option") {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "dict_get requires Option (import std/prelude.sal or define Option)",
            span,
        )]);
    }
    let dty = if let Some(a) = args.first() {
        check_expr(a, env, types, entries)?
    } else {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "dict_get expects dict and key",
            span,
        )]);
    };
    let (kty, vty) = dict_kv(&dty).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "dict_get expects a Dict",
            span,
        )]
    })?;
    if let Some(key) = args.get(1) {
        let kt = check_expr(key, env, types, entries)?;
        if !types_compatible(&kt, &kty) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "dict key type mismatch",
                key.span(),
            )]);
        }
    }
    Ok(option_type(vty, span))
}

pub fn type_name(t: &Type) -> String {
    match t {
        Type::Named { name, .. } => name.clone(),
        Type::Tensor { elem, .. } => format!("Tensor[{elem:?}]"),
        Type::Fn { .. } => "fn".into(),
    }
}
