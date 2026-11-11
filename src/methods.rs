//! Methods: `method T.name(params)` is sugar for `fn T__name(self: T, params)`
//! and `p.name(args)` for `T__name(p, args)`, `T` being the inferred type of `p`.
//! `static T.name(params)` is `fn T__name(params)`, called as `T.name(args)`;
//! `init T(params)` is `fn T__new(params) -> T`, called as `T.new(args)`.
//!
//! Two passes, both before ownership, effects, drops and lowering (which only
//! ever see direct calls):
//! - `desugar_method_defs` renames each `method` to `T__name` and prepends
//!   `self: T`. It runs after `qualify`, so `T` is already the final name.
//! - `resolve_method_calls` rewrites the calls `infer` resolved (the side
//!   channel is `TypeEnv::method_calls`, keyed by the span of the call).
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{DiagResult, Diagnostic, ErrorCode};
use crate::infer::{infer_program_with_env, type_env_of_program, TypeEnv};
use crate::span::Span;

/// Methods of the built-in types: `(type, method)` → the builtin they call with
/// the receiver as first argument. `xs.push(v)` is exactly `list_push(xs, v)`,
/// so every pass that knows the builtin by name keeps working.
pub fn builtin_method(ty: &str, method: &str) -> Option<&'static str> {
    Some(match (ty, method) {
        ("String", "len") => "str_len",
        ("String", "eq") => "str_eq",
        ("String", "contains") => "str_contains",
        ("String", "slice") => "str_slice",
        ("String", "char") => "str_char",
        ("String", "hash") => "str_hash",
        ("String", "concat") => "str_concat",
        ("String", "skip") => "str_skip",
        ("String", "to_int") => "str_as_int",
        ("String", "index_of") => "str_index_of",
        ("String", "index_from") => "str_index_from",
        ("String", "dup") => "strdup",
        ("String", "clone") => "strdup",
        ("List", "clone") => "list_clone",
        ("Dict", "clone") => "dict_clone",
        ("Bytes", "clone") => "bytes_clone",
        ("Int", "clone") => "clone_copy",
        ("Float", "clone") => "clone_copy",
        ("Bool", "clone") => "clone_copy",
        ("List", "len") => "list_len",
        ("List", "push") => "list_push",
        ("List", "get") => "list_get",
        ("List", "get_mut") => "list_get_mut",
        ("List", "set") => "list_set",
        ("List", "pop") => "list_pop",
        ("Dict", "get") => "dict_get",
        ("Dict", "put") => "dict_put",
        ("Dict", "has") => "dict_has",
        ("Dict", "len") => "dict_len",
        ("Dict", "remove") => "dict_remove",
        ("Dict", "keys") => "dict_keys",
        ("Dict", "values") => "dict_values",
        ("Bytes", "len") => "bytes_len",
        ("Bytes", "get_u8") => "bytes_get_u8",
        ("Bytes", "get_u16") => "bytes_get_u16",
        ("Bytes", "get_u32") => "bytes_get_u32",
        ("Bytes", "get_i64") => "bytes_get_i64",
        ("Bytes", "set_u8") => "bytes_set_u8",
        ("Bytes", "set_u16") => "bytes_set_u16",
        ("Bytes", "set_u32") => "bytes_set_u32",
        ("Bytes", "set_i64") => "bytes_set_i64",
        ("Bytes", "fill") => "bytes_fill",
        ("Bytes", "copy") => "bytes_copy",
        ("Bytes", "ptr") => "bytes_ptr",
        ("Bytes", "to_str") => "bytes_to_str",
        ("Bytes", "write_str") => "str_bytes_copy",
        ("Int", "to_str") => "int_to_str",
        ("Int", "to_float") => "int_to_float",
        ("Float", "to_int") => "float_to_int",
        ("Float", "to_str") => "f64_bits_to_str",
        ("Float", "sqrt") => "float_sqrt",
        ("Float", "abs") => "float_abs",
        ("Float", "floor") => "float_floor",
        ("Float", "ceil") => "float_ceil",
        ("Float", "sin") => "float_sin",
        ("Float", "cos") => "float_cos",
        ("Float", "exp") => "float_exp",
        ("Float", "log") => "float_log",
        ("Float", "pow") => "float_pow",
        _ => return None,
    })
}

/// Does the program contain a call of the form `expr.name(…)`? Those are the
/// only calls that need resolving, whether the method is user-defined or a
/// built-in one (`xs.push(v)`).
/// Constructors of the built-in types, `Type.name(args)`: the builtin they
/// call and how many type arguments they take (`List.new[Int]()`).
pub fn builtin_static(ty: &str, name: &str) -> Option<(&'static str, usize)> {
    Some(match (ty, name) {
        ("List", "new") => ("list_new", 1),
        ("Dict", "new") => ("dict_new", 2),
        ("Bytes", "new") => ("bytes_new", 0),
        ("String", "from_char") => ("char_to_str", 0),
        ("Bytes", "view_mem") => ("bytes_view_mem", 0),
        _ => return None,
    })
}

pub fn has_field_calls(prog: &Program) -> bool {
    fn blk(b: &Block) -> bool {
        b.stmts.iter().any(|st| match st {
            Stmt::Break { .. } => false,
            Stmt::Let { init, .. } => ex(init),
            Stmt::Expr(e) => ex(e),
            Stmt::Assign { target, value, .. } => ex(target) || ex(value),
            Stmt::Return { value, .. } => value.as_ref().is_some_and(ex),
            Stmt::While { cond, body, .. } => ex(cond) || blk(body),
        }) || b.tail.as_deref().is_some_and(ex)
    }
    fn ex(e: &Expr) -> bool {
        match e {
            Expr::Call { func, args, .. } => {
                matches!(**func, Expr::Field { .. }) || ex(func) || args.iter().any(ex)
            }
            Expr::Field { base, .. } => ex(base),
            Expr::Binary { left, right, .. } => ex(left) || ex(right),
            Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } => ex(expr),
            Expr::Block(b) => blk(b),
            Expr::On { body, .. } => blk(body),
            Expr::TensorLit { rows, .. } => rows.iter().flatten().any(ex),
            Expr::Match { scrutinee, arms, .. } => ex(scrutinee) || arms.iter().any(|a| ex(&a.body)),
            Expr::If { cond, then_block, elsifs, else_block, .. } => {
                ex(cond)
                    || blk(then_block)
                    || elsifs.iter().any(|a| ex(&a.cond) || blk(&a.body))
                    || else_block.as_ref().is_some_and(blk)
            }
            Expr::Lambda { body, .. } => ex(body),
            _ => false,
        }
    }
    prog.items.iter().any(|i| matches!(i, Item::Fn(f) if blk(&f.body)))
}

pub fn has_methods(prog: &Program) -> bool {
    prog.items
        .iter()
        .any(|i| matches!(i, Item::Fn(f) if f.receiver.is_some()))
}

/// `method T.m` → `fn T__m(self: T, …)`; `static T.m` → `fn T__m(…)`;
/// `init T` → `fn T__new(…) -> T`. The receiver has to be a non-generic
/// `struct`, `enum` or `frame` defined in this module.
pub fn desugar_method_defs(prog: &mut Program) -> DiagResult<()> {
    let mut types: HashSet<String> = HashSet::new();
    let mut fn_names: HashSet<String> = HashSet::new();
    for item in &prog.items {
        match item {
            Item::Struct(s) if s.type_params.is_empty() => {
                types.insert(s.name.clone());
            }
            Item::Enum(e) if e.type_params.is_empty() => {
                types.insert(e.name.clone());
            }
            Item::Frame(f) if f.type_params.is_empty() => {
                types.insert(f.name.clone());
            }
            Item::Fn(f) if f.receiver.is_none() => {
                fn_names.insert(f.name.clone());
            }
            _ => {}
        }
    }
    let err = |msg: String, span: Span| Err(vec![Diagnostic::new(ErrorCode::EType, msg, span)]);
    let mut seen: HashSet<String> = HashSet::new();
    for item in &mut prog.items {
        let Item::Fn(f) = item else { continue };
        let Some(recv) = f.receiver.take() else { continue };
        if !types.contains(&recv) {
            return err(
                format!(
                    "method receiver `{recv}` must be a non-generic struct, enum or frame defined in this module"
                ),
                f.span,
            );
        }
        if recv.contains("__") || f.name.contains("__") {
            return err(
                format!("`__` is reserved: method `{recv}.{}`", f.name),
                f.span,
            );
        }
        let mangled = format!("{recv}__{}", f.name);
        if fn_names.contains(&mangled) || !seen.insert(mangled.clone()) {
            return err(format!("method `{recv}.{}` is already defined", f.name), f.span);
        }
        if f.method_kind == MethodKind::Method {
            let named = Type::Named { name: recv, args: vec![], span: f.span };
            let ty = match f.self_ref {
                1 => Type::Ref {
                    mutable: false,
                    lifetime: None,
                    inner: Box::new(named),
                    span: f.span,
                },
                2 => Type::Ref {
                    mutable: true,
                    lifetime: None,
                    inner: Box::new(named),
                    span: f.span,
                },
                _ => named,
            };
            f.params.insert(
                0,
                Param {
                    name: "self".into(),
                    ty,
                    mode: f.self_mode,
                    span: f.span,
                },
            );
        }
        f.name = mangled;
    }
    Ok(())
}

/// Infer `prog` once and rewrite every `p.m(args)` it resolved into
/// `T__m(p, args)`. Errors are left to the regular inference that follows:
/// what resolved before an error is rewritten, the rest keeps its `Call{Field}`
/// and fails there with the real diagnostic.
pub fn resolve_methods(prog: &mut Program, env: &TypeEnv) {
    env.method_calls.borrow_mut().clear();
    let _ = infer_program_with_env(prog, env);
    let resolved = std::mem::take(&mut *env.method_calls.borrow_mut());
    if resolved.is_empty() {
        return;
    }
    for item in &mut prog.items {
        if let Item::Fn(f) = item {
            block(&mut f.body, &resolved);
        }
    }
}

/// Single-file entry: desugar the definitions, then resolve the calls.
pub fn prepare_program(prog: &mut Program) -> DiagResult<()> {
    if !has_methods(prog) && !has_field_calls(prog) && !crate::protocols::has_protocols(prog) {
        return Ok(());
    }
    desugar_method_defs(prog)?;
    if crate::protocols::has_protocols(prog) {
        let types = type_env_of_program(prog);
        crate::protocols::check_program_protocols(prog, &types)?;
    }
    if has_methods(prog) || has_field_calls(prog) {
        resolve_methods(prog, &type_env_of_program(prog));
    }
    Ok(())
}

/// Call span → (`T__m`, whether it was a static call `T.m(…)`).
pub type Resolved = HashMap<(u32, u32), (String, bool)>;

fn block(b: &mut Block, r: &Resolved) {
    for st in &mut b.stmts {
        match st {
            Stmt::Break { .. } => {}
            Stmt::Let { init, .. } => expr(init, r),
            Stmt::Expr(e) => expr(e, r),
            Stmt::Assign { target, value, .. } => {
                expr(target, r);
                expr(value, r);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    expr(v, r);
                }
            }
            Stmt::While { cond, body, .. } => {
                expr(cond, r);
                block(body, r);
            }
        }
    }
    if let Some(t) = &mut b.tail {
        expr(t, r);
    }
}

fn expr(e: &mut Expr, r: &Resolved) {
    match e {
        Expr::Call { func, args, span, .. } => {
            expr(func, r);
            for a in args.iter_mut() {
                expr(a, r);
            }
            if let (Expr::Field { base, .. }, Some((name, is_static))) =
                (&mut **func, r.get(&(span.start, span.end)))
            {
                let mut name = name.clone();
                if !*is_static {
                    let mut recv = std::mem::replace(
                        &mut **base,
                        Expr::Bool { value: false, span: *span },
                    );
                    // `xs.get_mut(i)` is `list_get(&mut xs, i)`: the element comes as `&mut T`.
                    if name == "list_get_mut" {
                        name = "list_get".to_string();
                        if !matches!(recv, Expr::Unary { op: UnOp::RefMut, .. }) {
                            let sp = recv.span();
                            recv = Expr::Unary { op: UnOp::RefMut, expr: Box::new(recv), span: sp };
                        }
                    }
                    args.insert(0, recv);
                }
                **func = Expr::Ident { name, span: *span };
            }
        }
        Expr::Field { base, .. } => expr(base, r),
        Expr::Binary { left, right, .. } => {
            expr(left, r);
            expr(right, r);
        }
        Expr::Unary { expr: inner, .. }
        | Expr::To { expr: inner, .. }
        | Expr::Try { expr: inner, .. } => expr(inner, r),
        Expr::Block(b) => block(b, r),
        Expr::On { body, .. } => block(body, r),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    expr(c, r);
                }
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            expr(scrutinee, r);
            for arm in arms {
                expr(&mut arm.body, r);
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            expr(cond, r);
            block(then_block, r);
            for arm in elsifs {
                expr(&mut arm.cond, r);
                block(&mut arm.body, r);
            }
            if let Some(b) = else_block {
                block(b, r);
            }
        }
        Expr::Lambda { body, .. } => expr(body, r),
        _ => {}
    }
}
