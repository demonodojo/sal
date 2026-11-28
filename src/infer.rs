use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::consteval::ConstValue;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::span::Span;
use crate::string_expr::is_string_type as is_string_type_raw;
use crate::typed::ExprTypeEntry;

pub struct InferOutput {
    pub expr_types_by_fn: Vec<(FnDef, Vec<ExprTypeEntry>)>,
    pub protocol_calls: HashMap<(u32, u32), (String, String, u32)>,
    pub any_coercions: HashMap<(u32, u32), (String, String)>,
    /// Constants visible here, keyed by the name after qualify.
    pub consts: HashMap<String, ConstValue>,
}

#[derive(Clone)]
pub struct TypeEnv {
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    /// Enums reached only through an import alias (`alias.f()` returns one). They type the
    /// values and the qualified variants (`alias.Variante`), but their variants are not in scope
    /// unqualified (SPEC: `import … as alias` does not flatten).
    pub opaque_enums: HashMap<String, EnumDef>,
    pub frames: HashMap<String, FrameDef>,
    /// Declared function signatures (name → params + return), for user calls.
    pub fns: HashMap<String, (Vec<Type>, Type)>,
    /// Generic fns whose return type is a bare type parameter: the index of the
    /// first parameter declared with that same type (the call's result type).
    pub generic_ret: HashMap<String, usize>,
    pub target: crate::target::Target,
    /// `p.m(args)` calls resolved while inferring: call span → `T__m`.
    /// `crate::methods::resolve_method_calls` rewrites them to direct calls.
    pub method_calls: std::cell::RefCell<crate::methods::Resolved>,
    /// Mangled names of `static` and `init` fns (`T__name`): they have no `self`.
    pub statics: std::collections::HashSet<String>,
    pub protocols: HashMap<String, ProtocolDef>,
    /// `FnDef` for signature checks (protocols, effects on methods); bodies are empty
    /// (`FnDef::signature`), except in a fn with a callback parameter.
    pub fn_defs: HashMap<String, FnDef>,
    /// Dynamic `x.m()` on `any[P]`: call span → (protocol, method, slot).
    pub protocol_calls: std::cell::RefCell<HashMap<(u32, u32), (String, String, u32)>>,
    /// Argument expr span → box coercion `(type_name, protocol)`.
    pub any_coercions: std::cell::RefCell<HashMap<(u32, u32), (String, String)>>,
    /// Constants visible in this module (own and imported), after qualify.
    pub consts: HashMap<String, crate::consteval::ConstValue>,
    /// Calls to a generic fn seen while inferring, for `specialize::monomorphize`:
    /// (calling fn, call span) → (callee, its type arguments in `type_params` order, or
    /// `None` when some argument type is not concrete).
    pub generic_calls: std::cell::RefCell<crate::specialize::GenericCalls>,
    /// Name of the fn being inferred (the key of `generic_calls`).
    pub current_fn: std::cell::RefCell<String>,
    /// Constructions of a generic struct (`Box(d"a", 2)`), keyed like `generic_calls`.
    pub generic_ctors: std::cell::RefCell<crate::specialize::GenericCalls>,
}

impl TypeEnv {
    /// The enum `name`; an opaque one only when the reference is qualified.
    pub fn enum_named(&self, name: &str, qualified: bool) -> Option<&EnumDef> {
        self.enums
            .get(name)
            .or_else(|| if qualified { self.opaque_enums.get(name) } else { None })
    }
}

impl Default for TypeEnv {
    fn default() -> Self {
        Self {
            structs: HashMap::new(),
            enums: HashMap::new(),
            opaque_enums: HashMap::new(),
            frames: HashMap::new(),
            fns: HashMap::new(),
            generic_ret: HashMap::new(),
            target: crate::target::Target::host(),
            method_calls: Default::default(),
            statics: Default::default(),
            protocols: HashMap::new(),
            fn_defs: HashMap::new(),
            protocol_calls: Default::default(),
            any_coercions: Default::default(),
            consts: HashMap::new(),
            generic_calls: Default::default(),
            current_fn: Default::default(),
            generic_ctors: Default::default(),
        }
    }
}


pub fn infer_program(prog: &Program) -> DiagResult<InferOutput> {
    let mut types = type_env_of_program(prog);
    types.consts = crate::consteval::eval_consts(prog, &HashMap::new())?;
    infer_program_with_env(prog, &types)
}

/// The type environment of one program on its own (no imports).
pub fn type_env_of_program(prog: &Program) -> TypeEnv {
    let mut types = TypeEnv::default();
    for item in &prog.items {
        match item {
            Item::Struct(s) => {
                types.structs.insert(s.name.clone(), s.clone());
            }
            Item::Frame(f) => {
                types.frames.insert(f.name.clone(), f.clone());
                types
                    .structs
                    .insert(f.name.clone(), frame_as_struct(f));
            }
            Item::Enum(e) => {
                types.enums.insert(e.name.clone(), e.clone());
            }
            Item::Fn(f) => register_fn(&mut types, f),
            Item::Protocol(p) => {
                types.protocols.insert(p.name.clone(), p.clone());
            }
            _ => {}
        }
    }
    types
}

/// Add `f` to the callable signatures, noting whether it is a `static`/`init`.
pub fn register_fn(types: &mut TypeEnv, f: &FnDef) {
    types.fns.insert(f.name.clone(), fn_signature(f));
    // A fn that takes a callback keeps its body: `specialize_lambdas` copies it at each call
    // with a lambda, also when the callee comes from another module.
    let takes_callback = f.params.iter().any(|p| matches!(p.ty, Type::Fn { .. }));
    types.fn_defs.insert(f.name.clone(), if takes_callback { f.clone() } else { f.signature() });
    if let Some(i) = generic_ret_index(f) {
        types.generic_ret.insert(f.name.clone(), i);
    }
    if f.method_kind != MethodKind::Method {
        types.statics.insert(f.name.clone());
    }
}

fn declared_lifetimes(params: &[String]) -> HashSet<String> {
    params
        .iter()
        .filter_map(|p| p.strip_prefix('\'').map(|s| s.to_string()))
        .collect()
}

fn lifetime_in_type(ty: &Type, declared: &HashSet<String>, allow_elision: bool) -> DiagResult<()> {
    match ty {
        Type::Ref { lifetime, inner, span, .. } => {
            if let Some(lt) = lifetime {
                if !declared.contains(lt) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("unknown lifetime `'{lt}`"),
                        *span,
                    )]);
                }
            } else if !allow_elision {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "a lifetime in a struct field must be written explicitly",
                    *span,
                )]);
            }
            lifetime_in_type(inner, declared, allow_elision)
        }
        Type::Named { args, .. } | Type::Qualified { args, .. } => {
            for a in args {
                lifetime_in_type(a, declared, allow_elision)?;
            }
            Ok(())
        }
        Type::Fn { params, ret, .. } => {
            for p in params {
                lifetime_in_type(p, declared, allow_elision)?;
            }
            lifetime_in_type(ret, declared, allow_elision)
        }
        Type::Tensor { .. } => Ok(()),
    }
}

/// Input lifetimes of a signature, and the lifetime of `&self` / `&mut self` when there is one.
fn input_lifetimes(f: &FnDef) -> (Vec<String>, Option<String>) {
    let mut inputs = Vec::new();
    let mut self_lt = None;
    let mut n = 0u32;
    for (i, p) in f.params.iter().enumerate() {
        collect_input_lts(&p.ty, i == 0 && p.name == "self", &mut inputs, &mut self_lt, &mut n);
    }
    (inputs, self_lt)
}

fn collect_input_lts(
    ty: &Type,
    is_self: bool,
    inputs: &mut Vec<String>,
    self_lt: &mut Option<String>,
    n: &mut u32,
) {
    match ty {
        Type::Ref { lifetime, inner, .. } => {
            let name = lifetime.clone().unwrap_or_else(|| {
                let fresh = format!("e{n}");
                *n += 1;
                fresh
            });
            if is_self && self_lt.is_none() {
                *self_lt = Some(name.clone());
            }
            if !inputs.contains(&name) {
                inputs.push(name);
            }
            collect_input_lts(inner, false, inputs, self_lt, n);
        }
        Type::Named { args, .. } | Type::Qualified { args, .. } => {
            for a in args {
                collect_input_lts(a, false, inputs, self_lt, n);
            }
        }
        Type::Fn { params, ret, .. } => {
            for p in params {
                collect_input_lts(p, false, inputs, self_lt, n);
            }
            collect_input_lts(ret, false, inputs, self_lt, n);
        }
        Type::Tensor { .. } => {}
    }
}

fn output_elision_ok(ty: &Type, inputs: &[String], self_lt: &Option<String>) -> DiagResult<()> {
    match ty {
        Type::Ref { lifetime, inner, span, .. } => {
            if lifetime.is_none() {
                let ok = self_lt.is_some() || inputs.len() == 1;
                if !ok {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EBorrow,
                        "elided lifetime in the return type has no source",
                        *span,
                    )
                    .with_hint("write the lifetime, or give the function a single input lifetime")]);
                }
            }
            output_elision_ok(inner, inputs, self_lt)
        }
        Type::Named { args, .. } | Type::Qualified { args, .. } => {
            for a in args {
                output_elision_ok(a, inputs, self_lt)?;
            }
            Ok(())
        }
        Type::Fn { params, ret, .. } => {
            for p in params {
                output_elision_ok(p, inputs, self_lt)?;
            }
            output_elision_ok(ret, inputs, self_lt)
        }
        Type::Tensor { .. } => Ok(()),
    }
}

fn check_lifetime_elision(f: &FnDef) -> DiagResult<()> {
    let declared = declared_lifetimes(&f.type_params);
    for p in &f.params {
        lifetime_in_type(&p.ty, &declared, true)?;
    }
    lifetime_in_type(&f.ret, &declared, true)?;
    let (inputs, self_lt) = input_lifetimes(f);
    output_elision_ok(&f.ret, &inputs, &self_lt)
}

pub fn infer_program_with_env(prog: &Program, types: &TypeEnv) -> DiagResult<InferOutput> {
    for item in &prog.items {
        if let Item::Struct(s) = item {
            let declared = declared_lifetimes(&s.lifetimes);
            for field in &s.fields {
                lifetime_in_type(&field.ty, &declared, false)?;
            }
        }
    }
    types.protocol_calls.borrow_mut().clear();
    types.any_coercions.borrow_mut().clear();
    let mut out = Vec::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            *types.current_fn.borrow_mut() = f.name.clone();
            let entries = infer_fn(f, types)?;
            out.push((f.clone(), entries));
        }
    }
    Ok(InferOutput {
        expr_types_by_fn: out,
        protocol_calls: types.protocol_calls.borrow().clone(),
        any_coercions: types.any_coercions.borrow().clone(),
        consts: types.consts.clone(),
    })
}

/// A generic fn is checked once, with its type parameters erased to `Unknown`
/// (every value shares one representation, so there is nothing to specialize).
fn erase_type_params(ty: &Type, params: &[String]) -> Type {
    match ty {
        Type::Named { name, args, span } => {
            if args.is_empty() && params.contains(name) {
                Type::Named { name: "Unknown".into(), args: vec![], span: *span }
            } else {
                Type::Named {
                    name: name.clone(),
                    args: args.iter().map(|a| erase_type_params(a, params)).collect(),
                    span: *span,
                }
            }
        }
        Type::Qualified { qual, name, args, span } => Type::Qualified {
            qual: qual.clone(),
            name: name.clone(),
            args: args.iter().map(|a| erase_type_params(a, params)).collect(),
            span: *span,
        },
        Type::Fn { params: ps, ret, effects, span } => Type::Fn {
            params: ps.iter().map(|p| erase_type_params(p, params)).collect(),
            ret: Box::new(erase_type_params(ret, params)),
            effects: effects.clone(),
            span: *span,
        },
        Type::Ref { mutable, lifetime, inner, span } => Type::Ref {
            mutable: *mutable,
            lifetime: lifetime.clone(),
            inner: Box::new(erase_type_params(inner, params)),
            span: *span,
        },
        other => other.clone(),
    }
}

pub fn generic_ret_index(f: &FnDef) -> Option<usize> {
    let Type::Named { name, args, .. } = &f.ret else { return None };
    if !args.is_empty() || !f.type_params.contains(name) {
        return None;
    }
    f.params.iter().position(|p| {
        matches!(&p.ty, Type::Named { name: n, args, .. } if n == name && args.is_empty())
    })
}

/// Parameter and return types of `f` as callers see them.
pub fn fn_signature(f: &FnDef) -> (Vec<Type>, Type) {
    (
        f.params.iter().map(|p| erase_type_params(&p.ty, &f.type_params)).collect(),
        erase_type_params(&f.ret, &f.type_params),
    )
}

fn infer_fn(f: &FnDef, types: &TypeEnv) -> DiagResult<Vec<ExprTypeEntry>> {
    // A generic body is checked once with its type parameters erased, also where
    // the body names them (`let xs: List[T]`, `List.new[T]()`).
    let erased;
    let f = if crate::specialize::type_params_of(f).is_empty() {
        f
    } else {
        erased = crate::specialize::erase_body(f);
        &erased
    };
    check_lifetime_elision(f)?;
    let mut env: HashMap<String, Type> = HashMap::new();
    let (ptys, ret) = fn_signature(f);
    for (p, ty) in f.params.iter().zip(ptys) {
        if !matches!(ty, Type::Fn { .. }) && contains_fn_type(&ty) {
            return Err(vec![Diagnostic::new(ErrorCode::EType, "function types are only allowed as parameter types", p.span)]);
        }
        check_type_well_formed(&ty, types)?;
        env.insert(p.name.clone(), ty);
    }
    if contains_fn_type(&ret) {
        return Err(vec![Diagnostic::new(ErrorCode::EType, "function types cannot be returned", f.span)]);
    }
    check_type_well_formed(&ret, types)?;
    let mut entries = Vec::new();
    let ctx = ReturnCtx {
        fn_ret: &ret,
        allow_return: true,
        fn_effects: &f.effects,
    };
    check_block(&f.body, &mut env, &ctx, types, &mut entries)?;
    Ok(entries)
}

/// Reject illegal tensor element types and unknown nominal types.
fn check_type_well_formed(ty: &Type, types: &TypeEnv) -> DiagResult<()> {
    match ty {
        Type::Qualified { name, args, span, .. } => {
            check_type_well_formed(
                &Type::Named {
                    name: name.clone(),
                    args: args.clone(),
                    span: *span,
                },
                types,
            )
        }
        Type::Named { name, args, span } => {
            if name == "any" {
                if args.len() != 1 {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "`any` expects one protocol argument: `any[Protocol]`",
                        *span,
                    )]);
                }
                let Type::Named {
                    name: pname,
                    args: pa,
                    span: ps,
                } = &args[0]
                else {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "`any[…]` expects a protocol name",
                        *span,
                    )]);
                };
                if !pa.is_empty() {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("protocol `{pname}` takes no type arguments"),
                        *ps,
                    )]);
                }
                if !types.protocols.contains_key(pname) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("`{pname}` is not a protocol"),
                        *ps,
                    )]);
                }
                return Ok(());
            }
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
            } else if !is_builtin_named_type(name) {
                if let Some(def) = types.structs.get(name) {
                    if args.len() != def.type_params.len() {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!(
                                "struct `{name}` expects {} type argument(s), found {}",
                                def.type_params.len(),
                                args.len()
                            ),
                            *span,
                        )]);
                    }
                } else if let Some(def) = types.enum_named(name, true) {
                    if args.len() != def.type_params.len() {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!(
                                "enum `{name}` expects {} type argument(s), found {}",
                                def.type_params.len(),
                                args.len()
                            ),
                            *span,
                        )]);
                    }
                } else {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("unknown type `{name}`"),
                        *span,
                    )]);
                }
            } else {
                validate_builtin_type_args(name, args, *span)?;
            }
            for a in args {
                check_type_well_formed(a, types)?;
            }
            Ok(())
        }
        Type::Tensor { dims: _, .. } => Ok(()),
        Type::Ref { inner, lifetime, span, .. } => {
            if let Some(name) = lifetime {
                if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("invalid lifetime `'{name}`"),
                        *span,
                    )]);
                }
            }
            check_type_well_formed(inner, types)
        }
        Type::Fn {
            params, ret, ..
        } => {
            for p in params {
                check_type_well_formed(p, types)?;
            }
            check_type_well_formed(ret, types)
        }
    }
}

fn is_builtin_named_type(name: &str) -> bool {
    matches!(
        name,
        "Int" | "Float" | "Bool" | "String" | "Bytes" | "Unit" | "List" | "Dict" | "Unknown"
            | "F32" | "F16" | "BF16" | "I8" | "Thread" | "Semaphore" | "Mutex" | "Channel"
    )
}

fn validate_builtin_type_args(name: &str, args: &[Type], span: Span) -> DiagResult<()> {
    let expected = match name {
        "List" | "Thread" | "Mutex" | "Channel" => Some(1),
        "Dict" => Some(2),
        _ => None,
    };
    if let Some(n) = expected {
        if args.len() != n {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("`{name}` expects {n} type argument(s), found {}", args.len()),
                span,
            )]);
        }
    } else if !args.is_empty() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("`{name}` is not generic"),
            span,
        )]);
    }
    Ok(())
}

struct ReturnCtx<'a> {
    fn_ret: &'a Type,
    allow_return: bool,
    fn_effects: &'a [Effect],
}

fn is_unit_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named { name, args, .. } if name == "Unit" && args.is_empty()
    )
}

fn unit_type(span: Span) -> Type {
    Type::Named {
        name: "Unit".into(),
        args: vec![],
        span,
    }
}

fn stmt_span(st: &Stmt) -> Span {
    match st {
        Stmt::InlineFor { .. } => unreachable!("`inline for` is unrolled before this pass"),
        Stmt::Parallel { .. } => unreachable!("`parallel` is rewritten before this pass"),
        Stmt::Break { span } => *span,
        Stmt::Let { span, .. } => *span,
        Stmt::Expr(e) => e.span(),
        Stmt::Assign { span, .. } => *span,
        Stmt::Return { span, .. } => *span,
        Stmt::While { span, .. } => *span,
    }
}

fn unreachable_after_return(st: &Stmt) -> DiagResult<()> {
    Err(vec![Diagnostic::new(
        ErrorCode::EType,
        "unreachable code after return",
        stmt_span(st),
    )])
}

fn check_block_flow(
    b: &Block,
    env: &mut HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
) -> DiagResult<bool> {
    let mut diverged = false;
    for st in &b.stmts {
        if diverged {
            unreachable_after_return(st)?;
        }
        if check_stmt(st, env, ctx, types, entries)? {
            diverged = true;
        }
    }
    Ok(diverged)
}

fn check_branch_block(
    b: &Block,
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
) -> DiagResult<(bool, Option<Type>)> {
    let mut local = env.clone();
    let diverged = check_block_flow(b, &mut local, ctx, types, entries)?;
    if diverged {
        if b.tail.is_some() {
            let t = b.tail.as_ref().unwrap();
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "unreachable code after return",
                t.span(),
            )]);
        }
        return Ok((true, None));
    }
    let ty = if let Some(t) = &b.tail {
        check_expr(t, &local, ctx, types, entries)?
    } else {
        unit_type(b.span)
    };
    Ok((false, Some(ty)))
}

fn check_block(
    b: &Block,
    env: &mut HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
) -> DiagResult<()> {
    let expected = ctx.fn_ret;
    let diverged = check_block_flow(b, env, &ctx, types, entries)?;
    if let Some(t) = &b.tail {
        if diverged {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "unreachable code after return",
                t.span(),
            )]);
        }
        let ty = check_expr(t, env, &ctx, types, entries)?;
        if let Err(d) = types_compatible_or_any_coerce_owned(&ty, expected, t.span(), ctx, types) {
            return Err(vec![d]);
        }
    } else if !diverged && !is_unit_type(expected) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!(
                "function must return a value of type {}",
                type_name(expected)
            ),
            b.span,
        )]);
    }
    Ok(())
}

fn check_stmt(
    st: &Stmt,
    env: &mut HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
) -> DiagResult<bool> {
    match st {
        Stmt::InlineFor { .. } => unreachable!("`inline for` is unrolled before this pass"),
        Stmt::Parallel { .. } => unreachable!("`parallel` is rewritten before this pass"),
        Stmt::Break { .. } => Ok(false),
        Stmt::Let { name, ty, init, .. } => {
            let ity = check_expr(init, env, ctx, types, entries)?;
            if let Some(decl) = ty {
                if contains_fn_type(decl) {
                    return Err(vec![Diagnostic::new(ErrorCode::EType, "function types are only allowed as parameter types", decl.span())]);
                }
                check_type_well_formed(decl, types)?;
                if let Err(d) = types_compatible_or_any_coerce_owned(&ity, decl, init.span(), ctx, types) {
                    return Err(vec![d]);
                }
            }
            env.insert(name.clone(), ty.clone().unwrap_or(ity));
            Ok(false)
        }
        Stmt::Expr(e) => {
            check_expr(e, env, ctx, types, entries)?;
            Ok(false)
        }
        Stmt::Assign { target, value, .. } => {
            let vty = check_expr(value, env, ctx, types, entries)?;
            if let Expr::Ident { name, span, .. } = target {
                if !env.contains_key(name) && types.consts.contains_key(name) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("`{name}` is a constant and cannot be assigned"),
                        *span,
                    )]);
                }
                if let Some(old) = env.get(name).cloned() {
                    // A local of type `any[P]` takes a new value of a type that claims `P` in a
                    // fresh box, as a `let` with that type does (SPEC «Protocolos»).
                    let old_is_any = matches!(&old, Type::Named { name, .. } if name == "any");
                    let new_is_any = matches!(&vty, Type::Named { name, .. } if name == "any");
                    if old_is_any && !new_is_any {
                        if let Err(d) = types_compatible_or_any_coerce_owned(&vty, &old, value.span(), ctx, types) {
                            return Err(vec![d]);
                        }
                        return Ok(false);
                    }
                    let old = &old;
                    if binding_kinds_clash(old, &vty, name.starts_with('_')) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!(
                                "cannot assign {} to `{name}`, which holds {}",
                                type_name(&vty),
                                type_name(old)
                            ),
                            *span,
                        )]);
                    }
                }
                env.insert(name.clone(), vty);
            } else {
                // `p.a.b = v` and `*r = v`: the value has the place's type (Rust E0308).
                let tty = check_expr(target, env, ctx, types, entries)?;
                if let Err(d) = types_compatible_or_any_coerce_owned(&vty, &tty, value.span(), ctx, types) {
                    return Err(vec![d]);
                }
            }
            Ok(false)
        }
        Stmt::Return { value, span } => {
            if !ctx.allow_return {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "return not allowed here",
                    *span,
                )]);
            }
            match value {
                Some(v) => {
                    let vty = check_expr(v, env, ctx, types, entries)?;
                    let coerced = !types_compatible(&vty, ctx.fn_ret)
                        && match types_compatible_or_any_coerce_owned(&vty, ctx.fn_ret, v.span(), ctx, types) {
                            Ok(()) => true,
                            Err(d) if types.any_coercions.borrow().contains_key(&(v.span().start, v.span().end)) => {
                                return Err(vec![d]);
                            }
                            Err(_) => false,
                        };
                    if !coerced && !types_compatible(&vty, ctx.fn_ret) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!(
                                "return type mismatch: expected {}, found {}",
                                type_name(ctx.fn_ret),
                                type_name(&vty)
                            ),
                            v.span(),
                        )]);
                    }
                }
                None => {
                    if !is_unit_type(ctx.fn_ret) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            "return requires a value",
                            *span,
                        )]);
                    }
                }
            }
            Ok(true)
        }
        Stmt::While { cond, body, .. } => {
            let cty = check_expr(cond, env, ctx, types, entries)?;
            check_condition(cond, &cty)?;
            let _ = check_block_flow(body, env, ctx, types, entries)?;
            if let Some(t) = &body.tail {
                check_expr(t, env, ctx, types, entries)?;
            }
            Ok(false)
        }
    }
}

fn contains_fn_type(ty: &Type) -> bool {
    match ty {
        Type::Fn { .. } => true,
        Type::Named { args, .. } | Type::Qualified { args, .. } => args.iter().any(contains_fn_type),
        Type::Ref { inner, .. } => contains_fn_type(inner),
        Type::Tensor { .. } => false,
    }
}

fn check_expr(
    e: &Expr,
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
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
        Expr::Unit { .. } => Type::Named {
            name: "Unit".into(),
            args: vec![],
            span: e.span(),
        },
        Expr::String { .. } => Type::Named {
            name: "String".into(),
            args: vec![],
            span: e.span(),
        },
        Expr::Ident { name, span } => match env.get(name) {
            Some(t) => t.clone(),
            None if types.consts.contains_key(name) => types.consts[name].ty(*span),
            None => match find_enum_variant_call(name, types) {
                // A payload-free variant used as a value.
                Some((edef, v)) if v.fields.is_empty() => Type::Named {
                    name: edef.name.clone(),
                    args: edef
                        .type_params
                        .iter()
                        .map(|_| Type::Named { name: "Unknown".into(), args: vec![], span: *span })
                        .collect(),
                    span: *span,
                },
                _ => {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("unknown variable `{name}`"),
                        *span,
                    )])
                }
            },
        },
        Expr::Binary {
            op,
            left,
            right,
            span,
        } => {
            let lt = check_expr(left, env, ctx, types, entries)?;
            let rt = check_expr(right, env, ctx, types, entries)?;
            if tensor_dims(&lt).is_none()
                && tensor_dims(&rt).is_some()
                && matches!(op, BinOp::Div | BinOp::Mod | BinOp::Shl | BinOp::Shr)
            {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "a scalar on the left of / % << >> with a tensor is not supported; put the tensor on the left",
                    *span,
                )]);
            }
            if tensor_dims(&lt).is_some() || tensor_dims(&rt).is_some() {
                return infer_tensor_binary(*op, &lt, &rt, *span);
            }
            match op {
                // `&&` and `||` take truth values, as `if` and `while` conditions do (`Bool`, or
                // `Int` where nonzero is true); the result is `Bool`. A scalar comparison is
                // still an `Int` here, so `(a < b) + 1` keeps working.
                BinOp::And | BinOp::Or => {
                    let ok = |t: &Type| is_bool_type(t) || is_int_type(t) || is_unknown_type(t);
                    if !ok(&lt) || !ok(&rt) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            "`&&` and `||` take Bool or Int operands",
                            *span,
                        )]);
                    }
                    Type::Named {
                        name: "Bool".into(),
                        args: vec![],
                        span: *span,
                    }
                }
                // String + String: concat. Nominal on both sides; `str_from_int`
                // is the explicit bridge for a handle held in an Int.
                BinOp::Add if is_string_type(&lt) => {
                    if !is_string_type(&rt) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            "string concatenation requires a String on the right; use str_from_int(...) or int_to_str(...)",
                            *span,
                        )]);
                    }
                    Type::Named {
                        name: "String".into(),
                        args: vec![],
                        span: *span,
                    }
                }
                BinOp::Add if is_string_type(&rt) => {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "string concatenation requires a String on the left",
                        *span,
                    )]);
                }
                BinOp::Mod | BinOp::Band | BinOp::Bxor | BinOp::Bor | BinOp::Shl | BinOp::Shr => {
                    if !is_int_type(&lt) || !is_int_type(&rt) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            "integer bitwise/shift operation requires Int",
                            *span,
                        )]);
                    }
                    Type::Named {
                        name: "Int".into(),
                        args: vec![],
                        span: *span,
                    }
                }
                _ => {
                    let ok = |t: &Type| is_numeric(t) || is_unknown_type(t);
                    if !ok(&lt) || !ok(&rt) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            "numeric operation requires Int or Float",
                            *span,
                        )]);
                    }
                    peel_refs(&lt).clone()
                }
            }
        }
        Expr::Unary { op, expr, span } => {
            let inner = check_expr(expr, env, ctx, types, entries)?;
            // `&x`, `&mut x` and `*r` are recorded like any other expression: the lowering reads
            // the struct a local reference points at from that type (`r = &w.p; r.x`).
            if matches!(op, UnOp::Ref | UnOp::RefMut | UnOp::Deref) {
                infer_ref_unary(*op, inner, *span)
            } else if let Some((dims, elem, place)) = tensor_dims(&inner) {
                match op {
                    UnOp::Neg => Type::Tensor {
                        elem,
                        dims,
                        place,
                        span: *span,
                    },
                    UnOp::Not if elem == TensorElem::I8 => Type::Tensor {
                        elem: TensorElem::I8,
                        dims,
                        place,
                        span: *span,
                    },
                    UnOp::Not => {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            "! on a tensor expects Tensor[I8, …]",
                            *span,
                        )]);
                    }
                    UnOp::Ref | UnOp::RefMut | UnOp::Deref => unreachable!("ref unary handled above"),
                }
            } else {
                inner
            }
        }
        Expr::Call {
            func,
            type_args,
            args,
            span,
        } => infer_call(func, type_args, args, env, ctx, types, entries, *span)?,
        Expr::Lambda {
            params,
            body,
            span,
        } => infer_lambda(params, body, None, env, ctx, types, entries, *span)?,
        Expr::Block(b) => {
            let mut local = env.clone();
            let diverged = check_block_flow(b, &mut local, ctx, types, entries)?;
            if let Some(t) = &b.tail {
                if diverged {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "unreachable code after return",
                        t.span(),
                    )]);
                }
                check_expr(t, &local, ctx, types, entries)?
            } else if diverged {
                unit_type(b.span)
            } else {
                unit_type(b.span)
            }
        }
        Expr::On {
            body,
            kernel_index,
            ..
        } => {
            let on_ctx = ReturnCtx {
                fn_ret: ctx.fn_ret,
                allow_return: false,
                fn_effects: ctx.fn_effects,
            };
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
            let diverged = check_block_flow(body, &mut local, &on_ctx, types, entries)?;
            if let Some(t) = &body.tail {
                if diverged {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "unreachable code after return",
                        t.span(),
                    )]);
                }
                check_expr(t, &local, &on_ctx, types, entries)?
            } else if diverged {
                unit_type(body.span)
            } else {
                unit_type(body.span)
            }
        }
        Expr::To { place, expr, span } => {
            let ty = check_expr(expr, env, ctx, types, entries)?;
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
                    check_expr(c, env, ctx, types, entries)?;
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
            let base_ty = check_expr(base, env, ctx, types, entries)?;
            resolve_field(peel_refs(&base_ty), field, types, *span)?
        }
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => {
            let scrut_ty = check_expr(scrutinee, env, ctx, types, entries)?;
            check_match_patterns(arms, &scrut_ty, types)?;
            let mut arm_tys = Vec::new();
            let mut arm_spans = Vec::new();
            for arm in arms {
                let mut local = env.clone();
                bind_pattern(&arm.pattern, &scrut_ty, types, &mut local)?;
                let n_before = arm_tys.len();
                // An arm whose block always returns gives no value to the `match` (SPEC «Bloque»).
                if let Expr::Block(b) = &arm.body {
                    let (div, ty) = check_branch_block(b, &local, ctx, types, entries)?;
                    let ty = ty.unwrap_or_else(|| unit_type(b.span));
                    entries.push(ExprTypeEntry {
                        span_start: arm.body.span().start,
                        span_end: arm.body.span().end,
                        ty: ty.clone(),
                    });
                    if !div {
                        arm_tys.push(ty);
                    }
                } else {
                    arm_tys.push(check_expr(&arm.body, &local, ctx, types, entries)?);
                }
                if arm_tys.len() > n_before {
                    arm_spans.push(arm.body.span());
                }
            }
            // Every arm that gives a value gives the `match`'s type (`&String` is not `String`).
            if let Some(first) = arm_tys.first() {
                for (ty, sp) in arm_tys.iter().zip(&arm_spans).skip(1) {
                    let int_bool = |t: &Type| matches!(t, Type::Named { name, .. } if name == "Int" || name == "Bool");
                    if !types_compatible(first, ty) && !(int_bool(first) && int_bool(ty)) {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!("expected {}, found {}", type_name(first), type_name(ty)),
                            *sp,
                        )]);
                    }
                }
            }
            if arm_tys.is_empty() && !arms.is_empty() {
                unit_type(*span)
            } else {
                arm_tys.first().cloned().unwrap_or_else(|| Type::Named {
                    name: "Unknown".into(),
                    args: vec![],
                    span: *span,
                })
            }
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            span,
        } => {
            let cty = check_expr(cond, env, ctx, types, entries)?;
            check_condition(cond, &cty)?;
            if let Some(else_block) = else_block {
                let (then_div, then_ty) =
                    check_branch_block(then_block, env, ctx, types, entries)?;
                for arm in elsifs {
                    let aty = check_expr(&arm.cond, env, ctx, types, entries)?;
                    check_condition(&arm.cond, &aty)?;
                    check_branch_block(&arm.body, env, ctx, types, entries)?;
                }
                let (else_div, else_ty) =
                    check_branch_block(else_block, env, ctx, types, entries)?;
                if then_div && else_div {
                    unit_type(*span)
                } else if then_div {
                    else_ty.unwrap_or_else(|| unit_type(*span))
                } else if else_div {
                    then_ty.unwrap_or_else(|| unit_type(*span))
                } else {
                    let then_ty = then_ty.unwrap();
                    let else_ty = else_ty.unwrap();
                    if !types_compatible(&then_ty, &else_ty) {
                        let err_span = else_block
                            .tail
                            .as_ref()
                            .map(|t| t.span())
                            .unwrap_or(*span);
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!(
                                "expected {}, found {}",
                                type_name(&then_ty),
                                type_name(&else_ty)
                            ),
                            err_span,
                        )]);
                    }
                    then_ty
                }
            } else {
                check_branch_block(then_block, env, ctx, types, entries)?;
                for arm in elsifs {
                    check_expr(&arm.cond, env, ctx, types, entries)?;
                    check_branch_block(&arm.body, env, ctx, types, entries)?;
                }
                unit_type(*span)
            }
        }
        Expr::Try { expr, span } => {
            let t = check_expr(expr, env, ctx, types, entries)?;
            match &t {
                Type::Named { name, args, .. } if types.enums.contains_key(name) => {
                    let fails = types.enums[name]
                        .variants
                        .iter()
                        .any(|v| v.name == "Err" || v.name == "None");
                    let ret_same = matches!(ctx.fn_ret, Type::Named { name: rn, .. } if rn == name);
                    if !fails {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!("`try` needs a Result or Option, found {name}"),
                            *span,
                        )]);
                    }
                    if !ret_same {
                        return Err(vec![Diagnostic::new(
                            ErrorCode::EType,
                            format!("`try` on {name} needs the function to return {name}"),
                            *span,
                        )]);
                    }
                    args.first().cloned().unwrap_or(Type::Named {
                        name: "Unknown".into(),
                        args: vec![],
                        span: *span,
                    })
                }
                Type::Named { name, .. } if name == "Unknown" => t.clone(),
                other => {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("`try` needs a Result or Option, found {}", type_name(other)),
                        *span,
                    )])
                }
            }
        }
    
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

#[derive(Copy, Clone, PartialEq, Eq)]
enum MatchScrutKind {
    Int,
    String,
    Enum,
    Other,
}

fn match_scrut_kind(ty: &Type, types: &TypeEnv) -> MatchScrutKind {
    let ty = peel_refs(ty);
    let Type::Named { name, .. } = ty else {
        return MatchScrutKind::Other;
    };
    if name == "Int" {
        MatchScrutKind::Int
    } else if name == "String" {
        MatchScrutKind::String
    } else if types.enums.contains_key(name) {
        MatchScrutKind::Enum
    } else {
        MatchScrutKind::Other
    }
}

fn posix_regex_valid(pat: &str) -> bool {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    let Ok(cpat) = CString::new(pat) else {
        return false;
    };
    let mut re = MaybeUninit::<libc::regex_t>::uninit();
    let rc = unsafe {
        libc::regcomp(
            re.as_mut_ptr(),
            cpat.as_ptr(),
            libc::REG_EXTENDED | libc::REG_NOSUB,
        )
    };
    if rc != 0 {
        return false;
    }
    unsafe { libc::regfree(re.as_mut_ptr()) };
    true
}

fn check_match_patterns(arms: &[MatchArm], scrut_ty: &Type, types: &TypeEnv) -> DiagResult<()> {
    let kind = match_scrut_kind(scrut_ty, types);
    for arm in arms {
        check_pattern_for_scrut(&arm.pattern, kind, types)?;
    }
    Ok(())
}

fn check_pattern_for_scrut(
    pat: &Pattern,
    kind: MatchScrutKind,
    types: &TypeEnv,
) -> DiagResult<()> {
    match (kind, pat) {
        (MatchScrutKind::Int, Pattern::Int(_, _) | Pattern::Wild(_) | Pattern::Ident(_, _)) => {
            Ok(())
        }
        (MatchScrutKind::Int, Pattern::Str(_, span) | Pattern::Regex(_, span)) => Err(vec![
            Diagnostic::new(
                ErrorCode::EType,
                "string pattern on Int scrutinee",
                *span,
            ),
        ]),
        (MatchScrutKind::Int, Pattern::Variant { span, .. }) => Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "variant pattern on non-enum scrutinee",
            *span,
        )]),
        (MatchScrutKind::String, Pattern::Str(_, _) | Pattern::Wild(_) | Pattern::Ident(_, _)) => {
            Ok(())
        }
        (MatchScrutKind::String, Pattern::Regex(re, span)) => {
            if types.target == crate::target::Target::Soso {
                return Err(vec![Diagnostic::new(
                    ErrorCode::ETarget,
                    "regex match patterns need regcomp, which the soso target does not have",
                    *span,
                )]);
            }
            if !posix_regex_valid(re) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "invalid regex pattern",
                    *span,
                )]);
            }
            Ok(())
        }
        (MatchScrutKind::String, Pattern::Int(_, span)) => Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "integer pattern on String scrutinee",
            *span,
        )]),
        (MatchScrutKind::String, Pattern::Variant { span, .. }) => Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "variant pattern on non-enum scrutinee",
            *span,
        )]),
        (MatchScrutKind::Enum, Pattern::Int(_, span) | Pattern::Str(_, span) | Pattern::Regex(_, span)) => {
            Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "scalar pattern on enum scrutinee",
                *span,
            )])
        }
        (MatchScrutKind::Enum, _) => Ok(()),
        (MatchScrutKind::Other, _) => Ok(()),
    }
}

fn bind_pattern(
    pat: &Pattern,
    scrut_ty: &Type,
    types: &TypeEnv,
    env: &mut HashMap<String, Type>,
) -> DiagResult<()> {
    let scrut_ty = peel_refs(scrut_ty);
    match pat {
        Pattern::Wild(_)
        | Pattern::Int(_, _)
        | Pattern::Str(_, _)
        | Pattern::Regex(_, _) => Ok(()),
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
            qual,
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
            let Some(edef) = types.enum_named(enum_name, qual.is_some()) else {
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

pub fn subst_type_params(ty: &Type, params: &[String], args: &[Type], span: Span) -> Type {
    match ty {
        Type::Qualified {
            qual,
            name,
            args: nested,
            ..
        } => Type::Qualified {
            qual: qual.clone(),
            name: name.clone(),
            args: nested
                .iter()
                .map(|a| subst_type_params(a, params, args, span))
                .collect(),
            span,
        },
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
        Type::Ref {
            mutable,
            lifetime,
            inner,
            ..
        } => Type::Ref {
            mutable: *mutable,
            lifetime: lifetime.clone(),
            inner: Box::new(subst_type_params(inner, params, args, span)),
            span,
        },
    }
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

fn find_enum_variant_call<'a>(
    variant: &str,
    types: &'a TypeEnv,
) -> Option<(&'a EnumDef, &'a crate::ast::EnumVariant)> {
    for edef in types.enums.values().chain(types.opaque_enums.values()) {
        if let Some(v) = edef.variants.iter().find(|v| v.name == variant) {
            return Some((edef, v));
        }
    }
    None
}

fn infer_struct_constructor(
    def: &StructDef,
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if !def.type_params.is_empty() {
        return infer_generic_struct_constructor(def, args, env, ctx, types, entries, span);
    }
    if args.len() != def.fields.len() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!(
                "struct `{}` expects {} field(s), found {}",
                def.name,
                def.fields.len(),
                args.len()
            ),
            span,
        )]);
    }
    for (arg, field) in args.iter().zip(def.fields.iter()) {
        let aty = check_expr(arg, env, ctx, types, entries)?;
        if !types_compatible(&aty, &field.ty) {
            // A field `any[P]` takes an owned value of a type that claims `P` in a box.
            if matches!(&field.ty, Type::Named { name, .. } if name == "any") {
                types_compatible_or_any_coerce_owned(&aty, &field.ty, arg.span(), ctx, types).map_err(|d| vec![d])?;
                continue;
            }
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!(
                    "struct `{}` field `{}`: expected {}, found {}",
                    def.name,
                    field.name,
                    type_name(&field.ty),
                    type_name(&aty)
                ),
                arg.span(),
            )]);
        }
    }
    Ok(Type::Named {
        name: def.name.clone(),
        args: vec![],
        span,
    })
}

/// `Box(d"a", 2)` with `struct Box[T]`: the arguments fix the type parameters through the field
/// types, and the value is a `Box[String]` (SPEC «Genéricos»). A parameter the arguments leave
/// unfixed is `E_TYPE`.
fn infer_generic_struct_constructor(
    def: &StructDef,
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.len() != def.fields.len() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!(
                "struct `{}` expects {} field(s), found {}",
                def.name,
                def.fields.len(),
                args.len()
            ),
            span,
        )]);
    }
    let mut arg_tys = Vec::new();
    for arg in args {
        arg_tys.push(check_expr(arg, env, ctx, types, entries)?);
    }
    let field_tys: Vec<Type> = def.fields.iter().map(|f| f.ty.clone()).collect();
    let Some(targs) = crate::specialize::bind_all(&field_tys, &arg_tys, &def.type_params) else {
        // Inside a generic body checked erased, an argument of unknown type leaves the
        // parameter unknown; with every argument known, an unfixed parameter is an error.
        if arg_tys.iter().any(crate::specialize::mentions_unknown) {
            let unknown = Type::Named { name: "Unknown".into(), args: vec![], span };
            return Ok(Type::Named {
                name: def.name.clone(),
                args: def.type_params.iter().map(|_| unknown.clone()).collect(),
                span,
            });
        }
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("struct `{}` requires type arguments", def.name),
            span,
        )]);
    };
    for ((arg, field), aty) in args.iter().zip(def.fields.iter()).zip(arg_tys.iter()) {
        let want = subst_type_params(&field.ty, &def.type_params, &targs, span);
        if !types_compatible(aty, &want) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!(
                    "struct `{}` field `{}`: expected {}, found {}",
                    def.name,
                    field.name,
                    type_name(&want),
                    type_name(aty)
                ),
                arg.span(),
            )]);
        }
    }
    let caller = types.current_fn.borrow().clone();
    types
        .generic_ctors
        .borrow_mut()
        .insert((caller, span.start, span.end), (def.name.clone(), Some(targs.clone())));
    Ok(Type::Named { name: def.name.clone(), args: targs, span })
}

fn infer_enum_variant_constructor(
    edef: &EnumDef,
    variant: &EnumVariant,
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if !edef.type_params.is_empty() && edef.type_params.len() != 1 {
        // Multi-param enums need explicit type args at the call site (later).
    }
    if args.len() != variant.fields.len() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!(
                "variant `{}` expects {} payload field(s), found {}",
                variant.name,
                variant.fields.len(),
                args.len()
            ),
            span,
        )]);
    }
    let mut type_args: Vec<Type> = edef
        .type_params
        .iter()
        .map(|_| Type::Named {
            name: "Unknown".into(),
            args: vec![],
            span,
        })
        .collect();
    for (arg, field_ty) in args.iter().zip(variant.fields.iter()) {
        let aty = check_expr(arg, env, ctx, types, entries)?;
        let expected = subst_type_params(field_ty, &edef.type_params, &type_args, span);
        // A payload typed as a bare type parameter fixes that parameter.
        let param_idx = match field_ty {
            Type::Named { name, args, .. } if args.is_empty() => {
                edef.type_params.iter().position(|p| p == name)
            }
            _ => None,
        };
        if let Some(i) = param_idx {
            type_args[i] = aty.clone();
        } else if !types_compatible(&aty, &expected) {
            // A payload of type `any[P]` takes the value into a box, as a `let` does.
            let at = arg.span();
            match types_compatible_or_any_coerce_owned(&aty, &expected, at, ctx, types) {
                Ok(()) => continue,
                Err(d) if types.any_coercions.borrow().contains_key(&(at.start, at.end)) => return Err(vec![d]),
                Err(_) => {}
            }
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!(
                    "variant `{}`: expected {}, found {}",
                    variant.name,
                    type_name(&expected),
                    type_name(&aty)
                ),
                arg.span(),
            )]);
        }
    }
    Ok(Type::Named {
        name: edef.name.clone(),
        args: type_args,
        span,
    })
}

fn types_compatible_or_any_coerce(
    aty: &Type,
    pt: &Type,
    at: Span,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
) -> Result<(), Diagnostic> {
    if types_compatible(aty, pt) {
        return Ok(());
    }
    if try_register_any_coercion(aty, pt, at, ctx, types).map_err(|v| v.into_iter().next().unwrap())? {
        return Ok(());
    }
    Err(Diagnostic::new(
        ErrorCode::EType,
        format!(
            "expected {}, found {}",
            type_name(pt),
            type_name(aty)
        ),
        at,
    ))
}

/// `types_compatible_or_any_coerce` where the value is kept (a `let`, an assignment, a returned
/// value): only an owned value goes into an `any[P]` there. A borrowed one (`&p` as
/// `&any[P]`) is a box that lives for one call, so it is only an argument (SPEC «Protocolos»).
fn types_compatible_or_any_coerce_owned(
    aty: &Type,
    pt: &Type,
    at: Span,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
) -> Result<(), Diagnostic> {
    types_compatible_or_any_coerce(aty, pt, at, ctx, types)?;
    if matches!(aty, Type::Ref { .. }) && types.any_coercions.borrow().contains_key(&(at.start, at.end)) {
        return Err(Diagnostic::new(
            ErrorCode::EType,
            format!("a borrowed `{}` only lives for the call it is passed to", type_name(pt)),
            at,
        ));
    }
    Ok(())
}

fn try_register_any_coercion(
    aty: &Type,
    pt: &Type,
    at: Span,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
) -> DiagResult<bool> {
    let pt = peel_refs(pt);
    let aty = peel_refs(aty);
    let Type::Named { name, args, .. } = pt else {
        return Ok(false);
    };
    if name != "any" {
        return Ok(false);
    }
    if args.len() != 1 {
        return Ok(false);
    }
    let Type::Named {
        name: protocol,
        args: pa,
        ..
    } = &args[0]
    else {
        return Ok(false);
    };
    if !pa.is_empty() {
        return Ok(false);
    }
    let Type::Named {
        name: tname,
        args: ta,
        ..
    } = aty
    else {
        return Ok(false);
    };
    if !ta.is_empty() {
        return Ok(false);
    }
    if !crate::protocols::type_claims_protocol(types, tname, protocol) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("type `{tname}` does not implement protocol `{protocol}`"),
            at,
        )]);
    }
    if !ctx.fn_effects.iter().any(|e| matches!(e, Effect::Alloc)) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EEffect,
            format!("coercing `{tname}` to `any[{protocol}]` requires effect `alloc`"),
            at,
        )]);
    }
    types
        .any_coercions
        .borrow_mut()
        .insert((at.start, at.end), (tname.clone(), protocol.clone()));
    Ok(true)
}

fn infer_any_method_call(
    field: &str,
    any_args: &[Type],
    type_args: &[TypeArg],
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let err = |msg: String, at: Span| Err(vec![Diagnostic::new(ErrorCode::EType, msg, at)]);
    if !type_args.is_empty() {
        return err(format!("method `{field}` on `any` takes no type arguments"), span);
    }
    let Type::Named {
        name: protocol,
        args: pa,
        span: ps,
    } = &any_args[0]
    else {
        return err("`any[…]` expects a protocol name".into(), span);
    };
    if !pa.is_empty() {
        return err(format!("protocol `{protocol}` takes no type arguments"), *ps);
    }
    let Some(pdef) = types.protocols.get(protocol) else {
        return err(format!("`{protocol}` is not a protocol"), *ps);
    };
    let Some(pm) = pdef.methods.iter().find(|m| m.name == field) else {
        return err(
            format!("protocol `{protocol}` has no method `{field}`"),
            span,
        );
    };
    if pm.is_static {
        return err(format!("`{field}` is a static of protocol `{protocol}`: call it on a type, not through `any`"), span);
    }
    if crate::protocols::mentions_self(pm) {
        return err(format!("method `{field}` names `Self`: it cannot be called through `any`"), span);
    }
    // As with `dyn` in Rust, a method that takes `self` by value would move the value out of
    // the box, which stays owned by the `any` (SPEC «Protocolos»).
    // A provided body is checked with `self: any[P]` standing for the implementing type, where
    // `self` by value is the type's own value: the rule is for a real `any` only.
    let in_provided_body = types.current_fn.borrow().starts_with("__protocol_body_");
    if pm.self_ref == 0 && !in_provided_body {
        return err(format!("method `{field}` takes `self` by value: it cannot be called through `any`"), span);
    }
    let Some(slot) = crate::protocols::method_slot(pdef, field) else {
        return err(format!("generic method `{field}` cannot be called through `any`"), span);
    };
    let slot = slot as u32;
    if args.len() != pm.params.len() {
        return err(
            format!(
                "method `{field}` expects {} argument(s), found {}",
                pm.params.len(),
                args.len()
            ),
            span,
        );
    }
    for (a, pt) in args.iter().zip(pm.params.iter()) {
        let aty = check_expr(a, env, ctx, types, entries)?;
        if !types_compatible(&aty, &pt.ty) {
            return err(
                format!(
                    "argument type mismatch calling `{field}`: expected {}, found {}",
                    type_name(&pt.ty),
                    type_name(&aty)
                ),
                a.span(),
            );
        }
    }
    types
        .protocol_calls
        .borrow_mut()
        .insert((span.start, span.end), (protocol.clone(), field.to_string(), slot));
    Ok(pm.ret.clone())
}

/// `base.field(args)`: the method `T__field` of the receiver's nominal type `T`.
fn infer_method_call(
    base: &Expr,
    field: &str,
    type_args: &[TypeArg],
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let err = |msg: String, at: Span| Err(vec![Diagnostic::new(ErrorCode::EType, msg, at)]);
    // `T.name(args)`: a type name that no local shadows is a static call.
    if let Expr::Ident { name: tname, .. } = base {
        if !env.contains_key(tname) {
            // `List.new[Int]()`, `Dict.new[String, Int]()`, `Bytes.new(n)`: the
            // constructor of a built-in type is the builtin, type arguments included.
            if !types.structs.contains_key(tname) && !types.enums.contains_key(tname) {
                if let Some((builtin, want)) = crate::methods::builtin_static(tname, field) {
                    let got = type_args.iter().filter(|a| matches!(a, TypeArg::Type(_))).count();
                    if got != want || type_args.len() != want {
                        return err(
                            format!(
                                "`{tname}.{field}` takes {want} type argument(s), found {}: write `{tname}.{field}[…](…)`",
                                type_args.len()
                            ),
                            span,
                        );
                    }
                    let callee = Expr::Ident { name: builtin.to_string(), span };
                    let ty = infer_call(&callee, type_args, args, env, ctx, types, entries, span)?;
                    types
                        .method_calls
                        .borrow_mut()
                        .insert((span.start, span.end), (builtin.to_string(), true));
                    return Ok(ty);
                }
            }
            if matches!(
                tname.as_str(),
                "List" | "Dict" | "Bytes" | "String" | "Int" | "Float" | "Bool" | "Thread" | "Semaphore" | "Mutex"
                    | "Channel"
            )
                && !types.structs.contains_key(tname)
                && !types.enums.contains_key(tname)
            {
                return err(format!("type `{tname}` has no static `{field}`"), span);
            }
            if types.structs.contains_key(tname) || types.enums.contains_key(tname) {
                if !type_args.is_empty() {
                    return err(format!("`{tname}.{field}` takes no type arguments"), span);
                }
                return infer_static_call(tname, field, args, env, ctx, types, entries, span);
            }
        }
    }
    if !type_args.is_empty() {
        return err(format!("method `{field}` takes no type arguments"), span);
    }
    let bty_full = check_expr(base, env, ctx, types, entries)?;
    let bty = peel_refs(&bty_full).clone();
    if let Type::Named { name, args: any_args, .. } = &bty {
        if name == "any" && any_args.len() == 1 {
            return infer_any_method_call(
                field, any_args, type_args, args, env, ctx, types, entries, span,
            );
        }
    }
    // A method of a built-in type is the builtin applied to the receiver. The
    // receiver is bound once to a synthetic name, so a chain of calls is
    // inferred in linear time.
    if field == "clone" {
        return infer_clone_method(&bty, base, env, ctx, types, entries, span);
    }
    if let Type::Named { name, .. } = &bty {
        if let Some(builtin) = crate::methods::builtin_method(name, field) {
            let recv = "__recv".to_string();
            let mut env2 = env.clone();
            env2.insert(recv.clone(), bty.clone());
            let mut call_args = vec![Expr::Ident { name: recv, span: base.span() }];
            call_args.extend(args.iter().cloned());
            let callee = Expr::Ident { name: builtin.to_string(), span };
            let ty = infer_call(&callee, type_args, &call_args, &env2, ctx, types, entries, span)?;
            types
                .method_calls
                .borrow_mut()
                .insert((span.start, span.end), (builtin.to_string(), false));
            return Ok(ty);
        }
    }
    if let Type::Named { name, args: receiver_args, .. } = &bty {
        if !receiver_args.is_empty() {
            let mangled = format!("{name}__{field}");
            if types.fn_defs.contains_key(&mangled) {
                let recv = "__recv".to_string();
                let mut env2 = env.clone();
                env2.insert(recv.clone(), bty.clone());
                let mut call_args = vec![Expr::Ident { name: recv, span: base.span() }];
                call_args.extend(args.iter().cloned());
                let callee = Expr::Ident { name: mangled.clone(), span };
                let ty = infer_call(&callee, type_args, &call_args, &env2, ctx, types, entries, span)?;
                types.method_calls.borrow_mut().insert((span.start, span.end), (mangled, false));
                return Ok(ty);
            }
        }
    }
    let tname = match &bty {
        Type::Named { name, args, .. } if args.is_empty() => name.clone(),
        other => {
            return err(
                format!("cannot call method `{field}` on type {}", type_name(other)),
                base.span(),
            )
        }
    };
    let mangled = format!("{tname}__{field}");
    let Some((params, ret)) = types.fns.get(&mangled) else {
        return err(format!("type `{tname}` has no method `{field}`"), span);
    };
    if types.statics.contains(&mangled) {
        return err(
            format!("`{tname}.{field}` is static: call it as `{tname}.{field}(…)`"),
            span,
        );
    }
    if args.len() + 1 != params.len() {
        return err(
            format!(
                "method `{tname}.{field}` expects {} argument(s), found {}",
                params.len() - 1,
                args.len()
            ),
            span,
        );
    }
    for (a, pt) in args.iter().zip(params.iter().skip(1)) {
        let aty = check_expr(a, env, ctx, types, entries)?;
        if !types_compatible(&aty, pt) {
            return err(
                format!(
                    "argument type mismatch calling `{tname}.{field}`: expected {}, found {}",
                    type_name(pt),
                    type_name(&aty)
                ),
                a.span(),
            );
        }
    }
    types
        .method_calls
        .borrow_mut()
        .insert((span.start, span.end), (mangled, false));
    Ok(ret.clone())
}

/// `tname.field(args)` where `tname` is a type: the `static` or `init`
/// function `tname__field` (`new` for an `init`).
fn infer_static_call(
    tname: &str,
    field: &str,
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let err = |msg: String, at: Span| Err(vec![Diagnostic::new(ErrorCode::EType, msg, at)]);
    let mangled = format!("{tname}__{field}");
    let Some((params, ret)) = types.fns.get(&mangled) else {
        return err(format!("type `{tname}` has no static `{field}`"), span);
    };
    if !types.statics.contains(&mangled) {
        return err(
            format!("`{tname}.{field}` is an instance method: call it on a value, `x.{field}(…)`"),
            span,
        );
    }
    if args.len() != params.len() {
        return err(
            format!(
                "`{tname}.{field}` expects {} argument(s), found {}",
                params.len(),
                args.len()
            ),
            span,
        );
    }
    for (a, pt) in args.iter().zip(params.iter()) {
        let aty = check_expr(a, env, ctx, types, entries)?;
        if !types_compatible(&aty, pt) {
            return err(
                format!(
                    "argument type mismatch calling `{tname}.{field}`: expected {}, found {}",
                    type_name(pt),
                    type_name(&aty)
                ),
                a.span(),
            );
        }
    }
    types
        .method_calls
        .borrow_mut()
        .insert((span.start, span.end), (mangled, true));
    Ok(ret.clone())
}

fn infer_call(
    func: &Expr,
    type_args: &[TypeArg],
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if let Expr::Field { base, field, .. } = func {
        return infer_method_call(base, field, type_args, args, env, ctx, types, entries, span);
    }
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
            check_type_well_formed(ty, types)?;
        }
    }
    if type_args.is_empty() {
        if let Some(def) = types.structs.get(name) {
            return infer_struct_constructor(def, args, env, ctx, types, entries, span);
        }
        if let Some(vinfo) = find_enum_variant_call(name, types) {
            return infer_enum_variant_constructor(
                &vinfo.0, &vinfo.1, args, env, ctx, types, entries, span,
            );
        }
    }
    match name {
        "relu" | "silu" | "rope" => infer_unary_tensor(args, env, ctx, types, entries, span),
        "softmax" => infer_softmax(args, env, ctx, types, entries, span),
        "reduce" => infer_reduce(args, env, ctx, types, entries, span),
        "reshape" => infer_reshape(args, env, ctx, types, entries, span),
        "transpose" => infer_transpose(args, env, ctx, types, entries, span),
        "rms_norm" => infer_rms_norm(args, env, ctx, types, entries, span),
        "attention" => infer_attention(args, env, ctx, types, entries, span),
        "qmatmul" => infer_qmatmul(args, env, ctx, types, entries, span),
        "tensor_append" => infer_tensor_append(args, env, ctx, types, entries, span),
        "argmax" => infer_argmax(args, env, ctx, types, entries, span),
        "matmul" => {
            let mut arg_tys = Vec::new();
            for a in args {
                arg_tys.push(check_expr(a, env, ctx, types, entries)?);
            }
            if arg_tys.len() != 2 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("matmul expects two tensors, got {}", arg_tys.len()),
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
                arg_tys.push(check_expr(a, env, ctx, types, entries)?);
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
        "str_bytes" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_bytes expects one String argument",
                    span,
                )]);
            }
            let arg = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_string_type(&arg) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_bytes expects String",
                    args[0].span(),
                )]);
            }
            Ok(Type::Tensor {
                elem: TensorElem::I8,
                dims: vec![Dim::Dynamic],
                place: Place::Cpu,
                span,
            })
        }
        "where" => infer_where(args, env, ctx, types, entries, span),
        "map" => infer_map_like(args, env, ctx, types, entries, span),
        "print" | "print_str" | "eprint_str" | "argc" | "str_eq" | "str_contains" | "str_index_of"
        | "str_index_from" | "str_len"
        | "str_char" | "str_skip" | "str_hash" | "map_get" | "map_put" | "not" | "free" | "copy_file" | "copy_self" | "gated_print_str"
        | "gated_copy_self" | "write_file" | "path_readable" | "regex_valid" | "mkdir_p" | "vec_push" | "vec_get" | "vec_set"
        | "vec_len" | "vec_free" | "list_free" | "list_free_shell" | "list_push_share" | "dict_free" | "enum_free" | "enum_free_shell" | "enum_set" | "enum_owns" | "enum_box_id"
        | "list_box_id" | "bytes_free" | "thread_free" | "sem_free" | "mutex_free" | "chan_free" | "clang" | "clang_obj" | "link_objs" | "exec_compile"
        | "write_png"
        | "gated_exec_compile" | "place_matmul_ai_sum" | "place_launches" => {
            for a in args {
                check_expr(a, env, ctx, types, entries)?;
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "float_sqrt" | "float_abs" | "float_floor" | "float_ceil" | "float_sin" | "float_cos"
        | "float_exp" | "float_log" | "float_pow" => {
            if types.target == crate::target::Target::Soso {
                return Err(vec![Diagnostic::new(
                    ErrorCode::ETarget,
                    format!("{name} needs libm, which the soso target does not have"),
                    span,
                )]);
            }
            let want = if name == "float_pow" { 2 } else { 1 };
            if args.len() != want {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("{name} expects {want} Float argument(s), found {}", args.len()),
                    span,
                )]);
            }
            let float = Type::Named { name: "Float".into(), args: vec![], span };
            for a in args {
                let aty = check_expr(a, env, ctx, types, entries)?;
                if !types_compatible(&aty, &float) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("{name} expects Float, found {}", type_name(&aty)),
                        a.span(),
                    )]);
                }
            }
            Ok(float)
        }
        "float_to_int" | "int_to_float" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("{name} expects one argument"),
                    span,
                )]);
            }
            check_expr(&args[0], env, ctx, types, entries)?;
            Ok(Type::Named {
                name: if name == "float_to_int" { "Int" } else { "Float" }.into(),
                args: vec![],
                span,
            })
        }
        "str_from_int" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_from_int expects one argument",
                    span,
                )]);
            }
            check_expr(&args[0], env, ctx, types, entries)?;
            Ok(Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            })
        }
        "str_as_int" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_as_int expects one argument",
                    span,
                )]);
            }
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_string_type(&t) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_as_int expects String",
                    span,
                )]);
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "str_to_f64_bits" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_to_f64_bits expects one argument",
                    span,
                )]);
            }
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_string_type(&t) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_to_f64_bits expects String",
                    span,
                )]);
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "f64_bits_to_str" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "f64_bits_to_str expects one argument",
                    span,
                )]);
            }
            check_expr(&args[0], env, ctx, types, entries)?;
            Ok(Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            })
        }
        "now_ns" => {
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "int_to_str" => {
            // `Int.to_str`: a tensor, a `String` or a struct is not an `Int` (SPEC «Tipos»); a
            // `Bool` lowers as the integer 0 or 1.
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "int_to_str expects one Int",
                    span,
                )]);
            }
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            let is_bool = matches!(peel_refs(&t), Type::Named { name, .. } if name == "Bool");
            if !is_int_type(&t) && !is_bool {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("int_to_str expects Int, found {}", type_name(&t)),
                    args[0].span(),
                )]);
            }
            Ok(Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            })
        }
        "argv" | "read_file" | "str_concat" | "str_slice"
        | "char_to_str" | "strdup" | "str_share" | "select_str" | "tmp_path" | "exec_capture" | "getenv"
        | "realpath" | "ir_text" => {
            for a in args {
                check_expr(a, env, ctx, types, entries)?;
            }
            Ok(Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            })
        }
        "vec_new" | "image_new" | "map_new" | "lex_src" => {
            for a in args {
                check_expr(a, env, ctx, types, entries)?;
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "list_clone" | "dict_clone" | "bytes_clone" | "enum_copy" | "struct_clone" | "clone_copy" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "clone takes the receiver",
                    span,
                )]);
            }
            let ty = check_expr(&args[0], env, ctx, types, entries)?;
            Ok(peel_refs(&ty).clone())
        }
        // SPEC «Hilos».
        "thread_spawn" | "thread_join" | "sem_new" | "sem_acquire" | "sem_try_acquire" | "sem_release"
        | "chan_new" | "chan_send" | "chan_recv" | "chan_close" | "mutex_new" | "mutex_enter" | "mutex_leave" => {
            infer_sync_builtin(name, type_args, args, env, ctx, types, entries, span)
        }
        "list_new" => {
            for a in args {
                check_expr(a, env, ctx, types, entries)?;
            }
            let elem = type_args.iter().find_map(|a| match a {
                TypeArg::Type(t) => Some(t.clone()),
                TypeArg::Dim(_) => None,
            });
            Ok(list_type(
                elem.unwrap_or(Type::Named {
                    name: "Int".into(),
                    args: vec![],
                    span,
                }),
                span,
            ))
        }
        // SPEC «Memoria»: the container is mutated in place; the call has no value.
        "list_push" => infer_list_push(args, env, ctx, types, entries, span).map(|_| unit_type(span)),
        "list_get" => infer_list_get(args, env, ctx, types, entries, span, false),
        "list_get_mut" => infer_list_get(args, env, ctx, types, entries, span, true),
        // SPEC «Memoria»: the container is mutated in place; the call has no value.
        "list_set" => infer_list_set(args, env, ctx, types, entries, span).map(|_| unit_type(span)),
        "list_pop" => infer_list_pop(args, env, ctx, types, entries, span),
        "dict_has" => infer_dict_has(args, env, ctx, types, entries, span),
        "dict_len" | "dict_remove" | "dict_keys" | "dict_values" => {
            infer_dict_extra(name, args, env, ctx, types, entries, span)
        }
        "list_len" => {
            if let Some(a) = args.first() {
                check_expr(a, env, ctx, types, entries)?;
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "dict_new" => infer_dict_new(type_args, span),
        // SPEC «Memoria»: the container is mutated in place; the call has no value.
        "dict_put" => infer_dict_put(args, env, ctx, types, entries, span).map(|_| unit_type(span)),
        "dict_get" => infer_dict_get(args, env, ctx, types, entries, span),
        "syscall" => {
            if types.target != crate::target::Target::Soso
                && types.target != crate::target::Target::Linux
            {
                return Err(vec![Diagnostic::new(
                    ErrorCode::ETarget,
                    "`syscall` is only available when compiling for soso or linux (host shim)",
                    span,
                )]);
            }
            if args.len() != 7 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "syscall expects seven Int arguments (nr, a0..a5)",
                    span,
                )]);
            }
            for a in args {
                let t = check_expr(a, env, ctx, types, entries)?;
                if !is_int_type(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "syscall arguments must be Int",
                        a.span(),
                    )]);
                }
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "bytes_new" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_new expects one Int argument (length)",
                    span,
                )]);
            }
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_int_type(&t) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_new expects Int length",
                    args[0].span(),
                )]);
            }
            Ok(bytes_type(span))
        }
        "bytes_len" | "bytes_ptr" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("{name} expects one Bytes argument"),
                    span,
                )]);
            }
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_bytes_type(&t) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("{name} expects Bytes"),
                    args[0].span(),
                )]);
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "bytes_get_u8"
        | "bytes_get_u16"
        | "bytes_get_u32"
        | "bytes_get_i64"
        | "bytes_set_u8"
        | "bytes_set_u16"
        | "bytes_set_u32"
        | "bytes_set_i64" => {
            let set = name.starts_with("bytes_set_");
            let expect = if set { 3 } else { 2 };
            if args.len() != expect {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("{name} expects {expect} argument(s)"),
                    span,
                )]);
            }
            let bt = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_bytes_or_ptr(&bt) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("{name} expects Bytes or Int pointer"),
                    args[0].span(),
                )]);
            }
            for a in &args[1..] {
                let t = check_expr(a, env, ctx, types, entries)?;
                if !is_int_type(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("{name} index/value arguments must be Int"),
                        a.span(),
                    )]);
                }
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "bytes_fill" => {
            if args.len() != 4 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_fill expects (Bytes, offset, length, byte)",
                    span,
                )]);
            }
            let bt = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_bytes_or_ptr(&bt) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_fill expects Bytes or Int pointer",
                    args[0].span(),
                )]);
            }
            for a in &args[1..] {
                let t = check_expr(a, env, ctx, types, entries)?;
                if !is_int_type(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "bytes_fill offset/length/byte must be Int",
                        a.span(),
                    )]);
                }
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "bytes_fill_u32" => {
            if args.len() != 4 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_fill_u32 expects (Bytes, offset, count, value)",
                    span,
                )]);
            }
            let bt = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_bytes_or_ptr(&bt) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_fill_u32 expects Bytes or Int pointer",
                    args[0].span(),
                )]);
            }
            for a in &args[1..] {
                let t = check_expr(a, env, ctx, types, entries)?;
                if !is_int_type(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "bytes_fill_u32 arguments must be Int",
                        a.span(),
                    )]);
                }
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "bytes_copy" => {
            if args.len() != 5 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_copy expects (dst, dst_off, src, src_off, len)",
                    span,
                )]);
            }
            for i in [0usize, 2] {
                let t = check_expr(&args[i], env, ctx, types, entries)?;
                if !is_bytes_or_ptr(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "bytes_copy dst/src must be Bytes or Int pointer",
                        args[i].span(),
                    )]);
                }
            }
            for i in [1usize, 3, 4] {
                let t = check_expr(&args[i], env, ctx, types, entries)?;
                if !is_int_type(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "bytes_copy offsets/length must be Int",
                        args[i].span(),
                    )]);
                }
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "str_bytes_copy" => {
            if args.len() != 3 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_bytes_copy expects (Bytes, offset, String)",
                    span,
                )]);
            }
            let bt = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_bytes_type(&bt) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_bytes_copy expects Bytes",
                    args[0].span(),
                )]);
            }
            let off = check_expr(&args[1], env, ctx, types, entries)?;
            if !is_int_type(&off) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_bytes_copy offset must be Int",
                    args[1].span(),
                )]);
            }
            let st = check_expr(&args[2], env, ctx, types, entries)?;
            if !is_string_type(&st) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "str_bytes_copy expects String",
                    args[2].span(),
                )]);
            }
            Ok(Type::Named {
                name: "Int".into(),
                args: vec![],
                span,
            })
        }
        "bytes_to_str" => {
            if args.len() != 1 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_to_str expects one Bytes argument",
                    span,
                )]);
            }
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_bytes_type(&t) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_to_str expects Bytes",
                    args[0].span(),
                )]);
            }
            Ok(Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            })
        }
        "bytes_view" => {
            if args.len() != 3 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_view expects (Bytes, offset, length)",
                    span,
                )]);
            }
            let bt = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_bytes_type(&bt) && !is_int_type(&bt) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_view expects Bytes or Int pointer",
                    args[0].span(),
                )]);
            }
            for a in &args[1..] {
                let t = check_expr(a, env, ctx, types, entries)?;
                if !is_int_type(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "bytes_view offset/length must be Int",
                        a.span(),
                    )]);
                }
            }
            Ok(bytes_type(span))
        }
        "bytes_view_mem" => {
            if args.len() != 2 {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "bytes_view_mem expects (data_ptr, length)",
                    span,
                )]);
            }
            for a in args {
                let t = check_expr(a, env, ctx, types, entries)?;
                if !is_int_type(&t) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "bytes_view_mem expects Int arguments",
                        a.span(),
                    )]);
                }
            }
            Ok(bytes_type(span))
        }
        _ => {
            if let Some((params, ret)) = types.fns.get(name) {
                if args.len() != params.len() {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!(
                            "function `{name}` expects {} argument(s), found {}",
                            params.len(),
                            args.len()
                        ),
                        span,
                    )]);
                }
                let mut arg_tys: Vec<Type> = Vec::new();
                for (i, (a, pt)) in args.iter().zip(params.iter()).enumerate() {
                    let inferred_param = types.fn_defs.get(name).and_then(|fd| {
                        if fd.type_params.is_empty() { return None; }
                        let previous: Vec<Type> = fd.params.iter().take(i).map(|p| p.ty.clone()).collect();
                        let bound = crate::specialize::bind_partial(&previous, &arg_tys, &fd.type_params);
                        let unbound: Vec<String> = fd.type_params.iter().filter(|p| !bound.contains_key(*p)).cloned().collect();
                        Some(erase_type_params(&crate::specialize::subst(&fd.params[i].ty, &bound), &unbound))
                    });
                    let pt = inferred_param.as_ref().unwrap_or(pt);
                    let aty = if let Type::Fn { params: fparams, ret: fret, .. } = pt {
                        if let Expr::Lambda { params: lparams, body, span: lspan } = a {
                            let fty = infer_lambda(lparams, body, Some(fparams.clone()), env, ctx, types, entries, *lspan)?;
                            if fparams.len() != lparams.len() || !types_compatible(&fty, pt) {
                                return Err(vec![Diagnostic::new(ErrorCode::EType, "lambda does not match function parameter type", a.span())]);
                            }
                            if !types_compatible(&fty, &Type::Fn { params: fparams.clone(), ret: fret.clone(), effects: vec![], span: a.span() }) {
                                return Err(vec![Diagnostic::new(ErrorCode::EType, "lambda return type does not match function parameter", a.span())]);
                            }
                            fty
                        } else { check_expr(a, env, ctx, types, entries)? }
                    } else { check_expr(a, env, ctx, types, entries)? };
                    arg_tys.push(aty.clone());
                    if let Err(d) =
                        types_compatible_or_any_coerce(&aty, pt, a.span(), ctx, types)
                    {
                        if captured_move_error(&aty, pt, a, types) {
                            return Err(vec![Diagnostic::new(ErrorCode::EBorrow, "cannot move a captured value out of its shared borrow", a.span())]);
                        }
                        let copy_list_get = matches!(pt, Type::Ref { inner, .. } if elem_is_copy(inner, types) && types_compatible(&aty, inner))
                            && matches!(a, Expr::Call { func, .. } if matches!(&**func, Expr::Ident { name, .. } if name == "list_get"));
                        if !receiver_autoref(types, name, i, &aty) && !copy_list_get {
                            return Err(vec![Diagnostic::new(
                                d.code,
                                d.message,
                                d.span,
                            )]);
                        }
                    }
                }
                if let Some(i) = types.generic_ret.get(name) {
                    if let Some(fd) = types.fn_defs.get(name) {
                        crate::specialize::record_generic_call(types, fd, &arg_tys, span);
                    }
                    return Ok(arg_tys[*i].clone());
                }
                if let Some(fd) = types.fn_defs.get(name) {
                    if let Some(r) = crate::specialize::record_generic_call(types, fd, &arg_tys, span) {
                        return Ok(r);
                    }
                }
                Ok(ret.clone())
            } else if let Some(Type::Fn { ret, params, .. }) = env.get(name).cloned() {
                if args.len() != params.len() {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("`{name}` expects {} argument(s), found {}", params.len(), args.len()),
                        span,
                    )]);
                }
                for (a, pt) in args.iter().zip(params.iter()) {
                    let aty = check_expr(a, env, ctx, types, entries)?;
                    let copy_ref = matches!(pt, Type::Ref { inner, .. } if elem_is_copy(inner, types) && types_compatible(&aty, inner));
                    if !types_compatible(&aty, pt) && !copy_ref {
                        return Err(vec![Diagnostic::new(ErrorCode::EType, "function argument type mismatch", a.span())]);
                    }
                }
                Ok(*ret)
            } else {
                if !is_runtime_builtin(name) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        format!("unknown function `{name}`"),
                        span,
                    )]);
                }
                for a in args {
                    check_expr(a, env, ctx, types, entries)?;
                }
                Ok(Type::Named {
                    name: "Unknown".into(),
                    args: vec![],
                    span,
                })
            }
        }
    }
}

/// A call to an undeclared name is only valid when the runtime provides
/// `sal_<name>`; anything else is a typo or a missing import.
fn is_runtime_builtin(name: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let mut set = HashSet::new();
        for src in [
            include_str!("../runtime/sal_runtime.c"),
            include_str!("../runtime/kernels.c"),
            include_str!("../runtime/instrument.c"),
            include_str!("../runtime/sal_threads.c"),
            include_str!("../runtime/gpu_driver.c"),
        ] {
            let bytes = src.as_bytes();
            let mut i = 0;
            while let Some(pos) = src[i..].find("sal_") {
                let start = i + pos + 4;
                let mut end = start;
                while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                if end > start {
                    set.insert(src[start..end].to_string());
                }
                i = end.max(start);
            }
        }
        set
    });
    // Language-level builtins that lower without a `sal_*` runtime symbol.
    (names.contains(name) && !lowering_only_runtime_fn(name)) || matches!(name, "index")
}

/// Runtime symbols that only the lowering calls, never sal source (SPEC «Métodos»: there is no
/// `str_append`). They consume or reuse a `String` block, which only a moved accumulator may do.
fn lowering_only_runtime_fn(name: &str) -> bool {
    matches!(name, "str_append" | "str_append_n" | "str_concat_n")
}

/// Lambda `x => body` or `(x, y) => body`.
fn infer_lambda(
    params: &[String],
    body: &Expr,
    param_ty_hints: Option<Vec<Type>>,
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let mut local = env.clone();
    let unknown = || Type::Named { name: "Unknown".into(), args: vec![], span };
    let mut param_tys = Vec::with_capacity(params.len());
    for (i, param_name) in params.iter().enumerate() {
        let param_ty = param_ty_hints.as_ref().and_then(|h| h.get(i)).cloned().unwrap_or_else(unknown);
        local.insert(param_name.clone(), param_ty.clone());
        param_tys.push(param_ty);
    }
    let body_ctx = ReturnCtx {
        fn_ret: ctx.fn_ret,
        allow_return: false,
        fn_effects: ctx.fn_effects,
    };
    let body_ty = check_expr(body, &local, &body_ctx, types, entries)?;
    Ok(Type::Fn {
        params: param_tys,
        ret: Box::new(body_ty),
        effects: vec![],
        span,
    })
}

fn infer_map_like(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
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
    let tensor_ty = check_expr(&args[0], env, ctx, types, entries)?;
    let elem_ty = tensor_elem_scalar(&tensor_ty, args[0].span());
    if let Some(cb) = args.get(1) {
        if let Expr::Lambda {
            params,
            body,
            span: lspan,
        } = cb
        {
            let fty = infer_lambda(params, body, Some(vec![elem_ty]), env, ctx, types, entries, *lspan)?;
            entries.push(ExprTypeEntry {
                span_start: fty.span().start,
                span_end: fty.span().end,
                ty: fty,
            });
        } else {
            check_expr(cb, env, ctx, types, entries)?;
        }
    }
    for a in args.iter().skip(2) {
        check_expr(a, env, ctx, types, entries)?;
    }
    Ok(peel_refs(&tensor_ty).clone())
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

pub fn frame_as_struct(f: &FrameDef) -> StructDef {
    let place = if f.type_params.len() == 1 {
        Place::Param(f.type_params[0].clone())
    } else {
        Place::Cpu
    };
    let fields = f
        .columns
        .iter()
        .map(|c| StructField {
            name: c.name.clone(),
            ty: column_elem_type(c.elem, place.clone(), c.span),
            span: c.span,
        })
        .collect();
    StructDef {
        lifetimes: Vec::new(),
        deprecated: None,
        name: f.name.clone(),
        type_params: f.type_params.clone(),
        protocols: Vec::new(),
        fields,
        layout_c: false,
        span: f.span,
    }
}

fn column_elem_type(elem: ColumnElem, place: Place, span: Span) -> Type {
    match elem {
        ColumnElem::String => list_type(
            Type::Named {
                name: "String".into(),
                args: vec![],
                span,
            },
            span,
        ),
        ColumnElem::F32 => Type::Tensor {
            elem: TensorElem::F32,
            dims: vec![Dim::Dynamic],
            place,
            span,
        },
        ColumnElem::F16 => Type::Tensor {
            elem: TensorElem::F16,
            dims: vec![Dim::Dynamic],
            place,
            span,
        },
        ColumnElem::BF16 => Type::Tensor {
            elem: TensorElem::BF16,
            dims: vec![Dim::Dynamic],
            place,
            span,
        },
        ColumnElem::I8 => Type::Tensor {
            elem: TensorElem::I8,
            dims: vec![Dim::Dynamic],
            place,
            span,
        },
    }
}

fn infer_where(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.len() != 2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "where expects frame and mask",
            span,
        )]);
    }
    let frame_ty = check_expr(&args[0], env, ctx, types, entries)?;
    let mask_ty = check_expr(&args[1], env, ctx, types, entries)?;
    let Type::Named { name, .. } = &frame_ty else {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "where expects a frame value as first argument",
            span,
        )]);
    };
    if !types.frames.contains_key(name) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("`{name}` is not a frame"),
            span,
        )]);
    }
    let Type::Tensor {
        elem: TensorElem::I8,
        dims,
        place,
        ..
    } = &mask_ty
    else {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "where mask must be Tensor[I8, ?] on the frame place",
            args[1].span(),
        )]);
    };
    let row_len_ok = match dims.as_slice() {
        [Dim::Static(_)] | [Dim::Dynamic] => true,
        [Dim::Static(1), Dim::Static(_)] | [Dim::Static(1), Dim::Dynamic] => true,
        _ => false,
    };
    if !row_len_ok {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "where mask must be rank-1",
            args[1].span(),
        )]);
    }
    let frame_place = types
        .structs
        .get(name)
        .and_then(|s| s.fields.first())
        .and_then(|f| tensor_dims(&f.ty).map(|(_, _, p)| p))
        .unwrap_or(Place::Cpu);
    if !place_compatible(place, &frame_place) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EPlace,
            "where mask place must match frame columns",
            args[1].span(),
        )]);
    }
    Ok(frame_ty)
}

fn infer_unary_tensor(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
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
    let t = check_expr(&args[0], env, ctx, types, entries)?;
    if tensor_dims(&t).is_none() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "expected tensor",
            args[0].span(),
        )]);
    }
    for a in args.iter().skip(1) {
        check_expr(a, env, ctx, types, entries)?;
    }
    Ok(peel_refs(&t).clone())
}

fn infer_softmax(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.is_empty() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "softmax expects a tensor",
            span,
        )]);
    }
    let t = check_expr(&args[0], env, ctx, types, entries)?;
    let (dims, elem, place) = tensor_dims(&t).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "softmax expects a tensor",
            args[0].span(),
        )]
    })?;
    if args.len() >= 2 {
        check_int_lit(&args[1], "softmax axis")?;
        if args[1].span().start > 0 {
            let _ = args[1].span();
        }
        let axis = int_lit_value(&args[1]).unwrap_or(0) as i64;
        if axis < 0 || axis >= dims.len() as i64 {
            return Err(vec![Diagnostic::new(
                ErrorCode::EShape,
                "softmax axis out of range",
                args[1].span(),
            )]);
        }
    }
    Ok(Type::Tensor {
        elem,
        dims,
        place,
        span,
    })
}

fn infer_reduce(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.is_empty() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "reduce expects a tensor",
            span,
        )]);
    }
    let t = check_expr(&args[0], env, ctx, types, entries)?;
    let (mut dims, elem, place) = tensor_dims(&t).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "reduce expects a tensor",
            args[0].span(),
        )]
    })?;
    if args.len() >= 2 {
        check_int_lit(&args[1], "reduce axis")?;
        let axis = int_lit_value(&args[1]).unwrap_or(0) as usize;
        if axis >= dims.len() {
            return Err(vec![Diagnostic::new(
                ErrorCode::EShape,
                "reduce axis out of range",
                args[1].span(),
            )]);
        }
        dims.remove(axis);
        if dims.is_empty() {
            dims.push(Dim::Static(1));
        }
    }
    Ok(Type::Tensor {
        elem,
        dims,
        place,
        span,
    })
}

fn infer_reshape(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.len() < 2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "reshape expects tensor and new dimensions",
            span,
        )]);
    }
    let t = check_expr(&args[0], env, ctx, types, entries)?;
    let (old_dims, elem, place) = tensor_dims(&t).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "reshape expects a tensor",
            args[0].span(),
        )]
    })?;
    let mut new_dims = Vec::new();
    for a in &args[1..] {
        match a {
            Expr::Int { value: v, .. } => new_dims.push(Dim::Static(*v as u64)),
            _ => {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "reshape dimensions must be integer literals",
                    a.span(),
                )])
            }
        }
    }
    if !tensor_volume_compatible(&old_dims, &new_dims) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "reshape volume mismatch",
            span,
        )]);
    }
    Ok(Type::Tensor {
        elem,
        dims: new_dims,
        place,
        span,
    })
}

fn infer_transpose(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.is_empty() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "transpose expects a tensor",
            span,
        )]);
    }
    let t = check_expr(&args[0], env, ctx, types, entries)?;
    let (mut dims, elem, place) = tensor_dims(&t).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "transpose expects a tensor",
            args[0].span(),
        )]
    })?;
    if dims.len() < 2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "transpose requires rank >= 2",
            args[0].span(),
        )]);
    }
    let n = dims.len();
    dims.swap(n - 1, n - 2);
    Ok(Type::Tensor {
        elem,
        dims,
        place,
        span,
    })
}

fn infer_rms_norm(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.len() != 2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "rms_norm expects tensor and weight",
            span,
        )]);
    }
    let t = check_expr(&args[0], env, ctx, types, entries)?;
    let w = check_expr(&args[1], env, ctx, types, entries)?;
    let (dims, elem, place) = tensor_dims(&t).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "rms_norm expects a tensor",
            args[0].span(),
        )]
    })?;
    let (wd, we, wp) = tensor_dims(&w).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "rms_norm weight must be a tensor",
            args[1].span(),
        )]
    })?;
    if elem != we || !place_compatible(&place, &wp) || dims != wd {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "rms_norm weight shape must match input",
            args[1].span(),
        )]);
    }
    Ok(peel_refs(&t).clone())
}

fn infer_attention(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.len() != 4 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "attention expects q, k, v, causal",
            span,
        )]);
    }
    let q = check_expr(&args[0], env, ctx, types, entries)?;
    let k = check_expr(&args[1], env, ctx, types, entries)?;
    let v = check_expr(&args[2], env, ctx, types, entries)?;
    check_expr(&args[3], env, ctx, types, entries)?;
    let (qd, qe, qp) = tensor_dims(&q).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EShape, "attention q must be tensor", args[0].span())]
    })?;
    let (kd, ke, kp) = tensor_dims(&k).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EShape, "attention k must be tensor", args[1].span())]
    })?;
    let (vd, ve, vp) = tensor_dims(&v).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EShape, "attention v must be tensor", args[2].span())]
    })?;
    if qe != ke || ke != ve || !place_compatible(&qp, &kp) || !place_compatible(&qp, &vp) {
        return Err(vec![Diagnostic::new(
            ErrorCode::ETensorelem,
            "attention q/k/v element and place must match",
            span,
        )]);
    }
    if qd.len() != 3 || kd.len() != 3 || vd.len() != 3 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "attention expects rank-3 q, k, v",
            span,
        )]);
    }
    Ok(q)
}

fn infer_qmatmul(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.len() != 4 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "qmatmul expects a, w, scale, group",
            span,
        )]);
    }
    let a = check_expr(&args[0], env, ctx, types, entries)?;
    let w = check_expr(&args[1], env, ctx, types, entries)?;
    check_expr(&args[2], env, ctx, types, entries)?;
    check_expr(&args[3], env, ctx, types, entries)?;
    let (a_dims, a_elem, a_place) = tensor_dims(&a).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EShape, "qmatmul left must be tensor", args[0].span())]
    })?;
    let (w_dims, w_elem, w_place) = tensor_dims(&w).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EShape, "qmatmul weight must be tensor", args[1].span())]
    })?;
    if w_elem != TensorElem::I8 {
        return Err(vec![Diagnostic::new(
            ErrorCode::ETensorelem,
            "qmatmul weight must be I8",
            args[1].span(),
        )]);
    }
    if !matches!(a_elem, TensorElem::F16 | TensorElem::BF16 | TensorElem::F32) {
        return Err(vec![Diagnostic::new(
            ErrorCode::ETensorelem,
            "qmatmul activation must be F16, BF16, or F32",
            args[0].span(),
        )]);
    }
    if !place_compatible(&a_place, &w_place) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EPlace,
            "qmatmul tensors must share place",
            span,
        )]);
    }
    if a_dims.len() < 2 || w_dims.len() < 2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EShape,
            "qmatmul requires rank >= 2",
            span,
        )]);
    }
    let mut out_dims = a_dims[..a_dims.len() - 1].to_vec();
    out_dims.extend_from_slice(&w_dims[w_dims.len() - 1..]);
    Ok(Type::Tensor {
        elem: a_elem,
        dims: out_dims,
        place: a_place,
        span,
    })
}

fn infer_tensor_append(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.len() != 2 {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "tensor_append expects cache and row",
            span,
        )]);
    }
    let cache = check_expr(&args[0], env, ctx, types, entries)?;
    let row = check_expr(&args[1], env, ctx, types, entries)?;
    let (mut dims, elem, place) = tensor_dims(&cache).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "tensor_append cache must be tensor",
            args[0].span(),
        )]
    })?;
    let (rd, re, rp) = tensor_dims(&row).ok_or_else(|| {
        vec![Diagnostic::new(
            ErrorCode::EType,
            "tensor_append row must be tensor",
            args[1].span(),
        )]
    })?;
    if elem != re || !place_compatible(&place, &rp) {
        return Err(vec![Diagnostic::new(
            ErrorCode::ETensorelem,
            "tensor_append row element/place mismatch",
            span,
        )]);
    }
    if dims.is_empty() {
        dims.push(Dim::Dynamic);
    } else {
        dims[0] = Dim::Dynamic;
    }
    let _ = rd;
    Ok(Type::Tensor {
        elem,
        dims,
        place,
        span,
    })
}

fn infer_argmax(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if args.is_empty() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "argmax expects a tensor",
            span,
        )]);
    }
    let t = check_expr(&args[0], env, ctx, types, entries)?;
    if tensor_dims(&t).is_none() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "argmax expects a tensor",
            args[0].span(),
        )]);
    }
    if args.len() >= 2 {
        check_int_lit(&args[1], "argmax axis")?;
    }
    Ok(Type::Named {
        name: "Int".into(),
        args: vec![],
        span,
    })
}

fn check_int_lit(e: &Expr, ctx: &str) -> DiagResult<()> {
    match e {
        Expr::Int { .. } => Ok(()),
        _ => Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("{ctx} must be an integer literal"),
            e.span(),
        )]),
    }
}

fn int_lit_value(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int { value: v, .. } => Some(*v),
        _ => None,
    }
}

fn tensor_volume_compatible(a: &[Dim], b: &[Dim]) -> bool {
    fn vol(d: &[Dim]) -> Option<u64> {
        let mut p = 1u64;
        for x in d {
            match x {
                Dim::Static(n) => {
                    p = p.saturating_mul(*n);
                }
                Dim::Dynamic => return None,
            }
        }
        Some(p)
    }
    match (vol(a), vol(b)) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

fn infer_tensor_binary(op: BinOp, lt: &Type, rt: &Type, span: Span) -> DiagResult<Type> {
    let cmp = matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    );
    let arith = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
    if !cmp && !arith {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "this operator is not defined on tensors",
            span,
        )]);
    }

    let (base, other) = if let Some(_) = tensor_dims(lt) {
        (lt, rt)
    } else if let Some(_) = tensor_dims(rt) {
        if !is_numeric(lt) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "tensor operation requires Int or Float scalar",
                span,
            )]);
        }
        (rt, lt)
    } else {
        unreachable!("infer_tensor_binary without tensor");
    };

    let (mut dims, elem, place) = tensor_dims(base).expect("tensor");
    if let Some((od, oe, op)) = tensor_dims(other) {
        if elem != oe {
            return Err(vec![Diagnostic::new(
                ErrorCode::ETensorelem,
                "tensor operands must share element type",
                span,
            )]);
        }
        if !place_compatible(&place, &op) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EPlace,
                "tensor operands must share place",
                span,
            )]);
        }
        dims = tensor_broadcast_shapes(&dims, &od).ok_or_else(|| {
            vec![Diagnostic::new(
                ErrorCode::EShape,
                "tensor shapes are not broadcast-compatible",
                span,
            )]
        })?;
    } else if !is_numeric(other) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "tensor operation requires Int or Float scalar on the other side",
            span,
        )]);
    }

    if cmp {
        Ok(Type::Tensor {
            elem: TensorElem::I8,
            dims,
            place,
            span,
        })
    } else {
        Ok(Type::Tensor {
            elem,
            dims,
            place,
            span,
        })
    }
}

fn dim_broadcast(da: &Dim, db: &Dim) -> Option<Dim> {
    match (da, db) {
        (Dim::Static(1), d) | (d, Dim::Static(1)) => Some(d.clone()),
        (Dim::Static(x), Dim::Static(y)) if x == y => Some(Dim::Static(*x)),
        (Dim::Dynamic, Dim::Dynamic) => Some(Dim::Dynamic),
        (Dim::Static(_), Dim::Dynamic) | (Dim::Dynamic, Dim::Static(_)) => Some(Dim::Dynamic),
        _ => None,
    }
}

fn tensor_broadcast_shapes(a: &[Dim], b: &[Dim]) -> Option<Vec<Dim>> {
    let max = a.len().max(b.len());
    let mut out = Vec::with_capacity(max);
    for k in 0..max {
        let off = max - 1 - k;
        let da = if off >= a.len() {
            Dim::Static(1)
        } else {
            a[a.len() - 1 - off].clone()
        };
        let db = if off >= b.len() {
            Dim::Static(1)
        } else {
            b[b.len() - 1 - off].clone()
        };
        out.push(dim_broadcast(&da, &db)?);
    }
    Some(out)
}

fn tensor_dims(t: &Type) -> Option<(Vec<Dim>, TensorElem, Place)> {
    match peel_refs(t) {
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

fn is_unknown_type(t: &Type) -> bool {
    matches!(t, Type::Named { name, .. } if name == "Unknown")
}

fn is_numeric(t: &Type) -> bool {
    matches!(
        peel_refs(t),
        Type::Named { name, .. } if name == "Int" || name == "Float"
    )
}

fn is_bool_type(t: &Type) -> bool {
    matches!(peel_refs(t), Type::Named { name, .. } if name == "Bool")
}

fn is_int_type(t: &Type) -> bool {
    matches!(peel_refs(t), Type::Named { name, .. } if name == "Int")
}

fn is_bytes_type(t: &Type) -> bool {
    matches!(peel_refs(t), Type::Named { name, .. } if name == "Bytes")
}

/// Puntero crudo (`Int` desde `bytes_ptr`) o valor `Bytes` del runtime Sal.
fn is_bytes_or_ptr(t: &Type) -> bool {
    is_bytes_type(t) || is_int_type(t)
}

fn bytes_type(span: Span) -> Type {
    Type::Named {
        name: "Bytes".into(),
        args: vec![],
        span,
    }
}

/// `x = v` over a local that holds a value of another type (SPEC «Liberación de locales»).
/// `Int` and `Bool` interchange; a `Unit` (the value of a call made for its effect) and a
/// reference clash with nothing. A `_name` may switch between plain scalars; it may not switch
/// to or from any other type, whose old value would be released with the wrong glue.
fn binding_kinds_clash(a: &Type, b: &Type, discard: bool) -> bool {
    fn kind(t: &Type) -> Option<(&str, bool)> {
        match t {
            Type::Named { name, .. } => match name.as_str() {
                "Int" | "Bool" => Some(("Int", true)),
                "Float" => Some(("Float", true)),
                "String" => Some(("String", true)),
                "Unit" => None,
                other => Some((other, false)),
            },
            _ => None,
        }
    }
    match (kind(a), kind(b)) {
        (Some((x, plain_x)), Some((y, plain_y))) => x != y && !(discard && plain_x && plain_y),
        _ => false,
    }
}

fn check_condition(cond: &Expr, ty: &Type) -> DiagResult<()> {
    if matches!(ty, Type::Named { name, args, .. } if args.is_empty() && name == "String") {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "condition must be Bool or Int, found String",
            cond.span(),
        )]);
    }
    Ok(())
}

/// `x.m()` where `m` takes `&self`: the receiver `x` is passed as `&x`.
fn receiver_autoref(types: &TypeEnv, name: &str, index: usize, aty: &Type) -> bool {
    if index != 0 {
        return false;
    }
    let Some(def) = types.fn_defs.get(name) else {
        return false;
    };
    let Some(param) = def.params.first() else {
        return false;
    };
    if param.name != "self" {
        return false;
    }
    let Type::Ref { inner, .. } = &param.ty else {
        return false;
    };
    types_compatible(aty, &erase_type_params(inner, &def.type_params))
}

/// A dict stores the key pointer. `&String` and `String` are that same pointer.
fn dict_key_ok(actual: &Type, expected: &Type) -> bool {
    types_compatible(actual, expected) || types_compatible(peel_one_ref(actual), expected)
}

fn peel_one_ref(t: &Type) -> &Type {
    match t {
        Type::Ref { inner, .. } => inner,
        other => other,
    }
}

fn types_compatible(a: &Type, b: &Type) -> bool {
    types_compatible_core(a, b) || {
        // `Box[String]` and its instance `Box__String` are one type (SPEC «Genéricos»).
        let (ca, cb) = (crate::specialize::canonical_instance(a), crate::specialize::canonical_instance(b));
        (ca.is_some() || cb.is_some())
            && types_compatible_core(ca.as_ref().unwrap_or(a), cb.as_ref().unwrap_or(b))
    }
}

fn types_compatible_core(a: &Type, b: &Type) -> bool {
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
                return false;
            }
            if aa.len() != ab.len() {
                return aa.is_empty() || ab.is_empty();
            }
            aa.iter()
                .zip(ab.iter())
                .all(|(x, y)| types_compatible(x, y))
        }
        (
            Type::Ref {
                mutable: m1,
                inner: i1,
                ..
            },
            Type::Ref {
                mutable: m2,
                inner: i2,
                ..
            },
        ) => {
            // `a` is the value and `b` what is expected: `&mut T` reborrows as `&T`, never the
            // other way round.
            (m1 == m2 || (*m1 && !*m2)) && types_compatible(i1, i2)
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
    match peel_refs(ty) {
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
    match peel_refs(ty) {
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

fn is_dict_key_type(ty: &Type) -> bool {
    matches!(
        peel_refs(ty),
        Type::Named { name, .. } if name == "Int" || name == "String"
    )
}

fn named0(name: &str, span: Span) -> Type {
    Type::Named { name: name.into(), args: vec![], span }
}

/// The value of a type that fits one i64 slot: a struct of two or more fields goes in
/// registers and comes back through a buffer, so it cannot cross into a thread in this cut.
fn crosses_thread_in_one_slot(t: &Type, types: &TypeEnv) -> bool {
    match peel_refs(t) {
        Type::Named { name, .. } => types.structs.get(name).is_none_or(|sd| sd.fields.len() < 2),
        Type::Tensor { .. } | Type::Fn { .. } => false,
        _ => true,
    }
}

/// `Thread`, `Semaphore` and `Channel` builtins (SPEC «Hilos»). The receivers of
/// `acquire`, `release`, `send`, `recv` and `close` are lent (`&self`); `join` moves the thread.
#[allow(clippy::too_many_arguments)]
fn infer_sync_builtin(
    name: &str,
    type_args: &[TypeArg],
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let err = |msg: String, at: Span| Err(vec![Diagnostic::new(ErrorCode::EType, msg, at)]);
    if types.target == crate::target::Target::Soso {
        return Err(vec![Diagnostic::new(
            ErrorCode::ETarget,
            format!("`{name}`: threads are not available on the soso target"),
            span,
        )]);
    }
    let want = match name {
        "sem_acquire" | "sem_try_acquire" | "sem_release" | "thread_join" | "chan_recv" | "chan_close"
        | "sem_new" | "thread_spawn" | "mutex_new" | "mutex_enter" | "mutex_leave" => 1,
        "chan_send" => 2,
        _ => 0,
    };
    if args.len() != want {
        return err(format!("`{name}` expects {want} argument(s), found {}", args.len()), span);
    }
    let receiver = |ty: &Type, what: &str| -> Option<Type> {
        match peel_refs(ty) {
            Type::Named { name: n, args: a, .. } if n == what => a.first().cloned().or(Some(unit_type(span))),
            _ => None,
        }
    };
    match name {
        "thread_spawn" => {
            // `Thread.spawn(f, a…)` became `thread_spawn(f(a…))` (src/threads.rs).
            let Expr::Call { func, args: fargs, .. } = &args[0] else {
                return err("`Thread.spawn` takes the name of a fn first".into(), span);
            };
            let Expr::Ident { name: fname, .. } = func.as_ref() else {
                return err("`Thread.spawn` takes the name of a fn first".into(), span);
            };
            if !types.fns.contains_key(fname) && !types.fn_defs.contains_key(fname) {
                return err(format!("`Thread.spawn`: `{fname}` is not a fn"), func.span());
            }
            if fargs.len() > 8 {
                return err("`Thread.spawn` passes at most 8 arguments".into(), span);
            }
            for a in fargs {
                let at = check_expr(a, env, ctx, types, entries)?;
                if matches!(at, Type::Ref { mutable: true, .. }) {
                    return err(
                        "`Thread.spawn` cannot lend `&mut` to a thread: pass the value, `&T` or a `Mutex`".into(),
                        a.span(),
                    );
                }
                if !crosses_thread_in_one_slot(&at, types) {
                    return err(
                        format!("`Thread.spawn`: a struct of several fields (`{}`) cannot be passed to a thread yet", type_name(&at)),
                        a.span(),
                    );
                }
            }
            let r = check_expr(&args[0], env, ctx, types, entries)?;
            if !crosses_thread_in_one_slot(&r, types) {
                return err(
                    format!("`Thread.spawn`: `{fname}` returns a struct of several fields (`{}`), which a thread cannot return yet", type_name(&r)),
                    span,
                );
            }
            if matches!(r, Type::Ref { .. }) {
                return err(format!("`Thread.spawn`: `{fname}` returns a reference"), span);
            }
            Ok(Type::Named { name: "Thread".into(), args: vec![r], span })
        }
        "thread_join" => {
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if matches!(t, Type::Ref { .. }) {
                return err("`join` takes the thread by value: it moves `t`".into(), args[0].span());
            }
            receiver(&t, "Thread").ok_or_else(|| {
                vec![Diagnostic::new(ErrorCode::EType, format!("`join` expects a Thread, found {}", type_name(&t)), args[0].span())]
            })
        }
        "sem_new" => {
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if !is_int_type(&t) {
                return err(format!("`Semaphore.new` expects Int permits, found {}", type_name(&t)), args[0].span());
            }
            Ok(named0("Semaphore", span))
        }
        "sem_acquire" | "sem_try_acquire" | "sem_release" => {
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            if receiver(&t, "Semaphore").is_none() {
                return err(format!("`{name}` expects a Semaphore, found {}", type_name(&t)), args[0].span());
            }
            Ok(if name == "sem_try_acquire" { named0("Bool", span) } else { unit_type(span) })
        }
        "mutex_new" => {
            let v = check_expr(&args[0], env, ctx, types, entries)?;
            if matches!(v, Type::Ref { .. }) {
                return err("`Mutex.new` takes the value it guards, not a reference".into(), args[0].span());
            }
            if !crosses_thread_in_one_slot(&v, types) {
                return err(
                    format!("`Mutex.new`: a struct of several fields (`{}`) cannot be guarded yet", type_name(&v)),
                    args[0].span(),
                );
            }
            Ok(Type::Named { name: "Mutex".into(), args: vec![v], span })
        }
        "mutex_enter" | "mutex_leave" => {
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            let Some(elem) = receiver(&t, "Mutex") else {
                return err(format!("`{name}` expects a Mutex, found {}", type_name(&t)), args[0].span());
            };
            if name == "mutex_leave" {
                return Ok(unit_type(span));
            }
            Ok(Type::Ref { mutable: true, lifetime: None, inner: Box::new(elem), span })
        }
        "chan_new" => {
            let elem = match type_args {
                [TypeArg::Type(t)] => t.clone(),
                _ => return err("`Channel.new` takes one type argument: `Channel.new[T]()`".into(), span),
            };
            Ok(Type::Named { name: "Channel".into(), args: vec![elem], span })
        }
        "chan_send" | "chan_recv" | "chan_close" => {
            let t = check_expr(&args[0], env, ctx, types, entries)?;
            let Some(elem) = receiver(&t, "Channel") else {
                return err(format!("`{name}` expects a Channel, found {}", type_name(&t)), args[0].span());
            };
            match name {
                "chan_send" => {
                    let v = check_expr(&args[1], env, ctx, types, entries)?;
                    if !types_compatible(&v, &elem) {
                        return err(
                            format!("channel element type mismatch: expected {}, found {}", type_name(&elem), type_name(&v)),
                            args[1].span(),
                        );
                    }
                    Ok(unit_type(span))
                }
                "chan_recv" => {
                    let option_ok = types.enums.get("Option").is_some_and(|e| {
                        e.variants.len() == 2 && e.variants[0].name == "None" && e.variants[1].name == "Some"
                    });
                    if !option_ok {
                        return err(
                            "`recv` requires Option with variants None and Some, in that order (import std/prelude.sal)".into(),
                            span,
                        );
                    }
                    Ok(option_type(elem, span))
                }
                _ => Ok(unit_type(span)),
            }
        }
        _ => err(format!("unknown builtin `{name}`"), span),
    }
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
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let xs_ty = if let Some(a) = args.first() {
        check_expr(a, env, ctx, types, entries)?
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
        let xty = check_expr(x, env, ctx, types, entries)?;
        if !types_compatible(&xty, &elem) {
            if captured_move_error(&xty, &elem, x, types) {
                return Err(vec![Diagnostic::new(ErrorCode::EBorrow, "cannot move a captured value out of its shared borrow", x.span())]);
            }
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
    Ok(peel_refs(&xs_ty).clone())
}

fn captured_move_error(actual: &Type, expected: &Type, expr: &Expr, types: &TypeEnv) -> bool {
    let Type::Ref { inner, .. } = actual else { return false };
    matches!(expr, Expr::Ident { name, .. } if name.starts_with("__cap"))
        && types_compatible(inner, expected)
        && !elem_is_copy(expected, types)
}

/// Whether a list element of this type is copied out by `get` (it owns no heap).
fn elem_is_copy(ty: &Type, types: &TypeEnv) -> bool {
    fn rec(ty: &Type, types: &TypeEnv, depth: u32) -> bool {
        match ty {
            Type::Named { name, args, .. } | Type::Qualified { name, args, .. } => {
                if matches!(name.as_str(), "Int" | "Float" | "Bool" | "Unit" | "Unknown") {
                    return true;
                }
                if !args.is_empty() || depth > 8 {
                    return false;
                }
                match types.structs.get(name) {
                    Some(def) => def.fields.iter().all(|f| rec(&f.ty, types, depth + 1)),
                    // A bare type parameter: the instance decides, so nothing is lent here.
                    None => !types.enums.contains_key(name) && !types.opaque_enums.contains_key(name)
                        && !matches!(name.as_str(), "String" | "List" | "Dict" | "Bytes"),
                }
            }
            Type::Ref { mutable, .. } => !mutable,
            Type::Tensor { .. } | Type::Fn { .. } => false,
        }
    }
    rec(ty, types, 0)
}

fn infer_list_get(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
    force_mut: bool,
) -> DiagResult<Type> {
    if let Some(a) = args.first() {
        let xs_ty = check_expr(a, env, ctx, types, entries)?;
        if let Some(elem) = list_elem(&xs_ty) {
            if args.get(1).is_some() {
                check_expr(&args[1], env, ctx, types, entries)?;
            }
            // SPEC «Memoria»: `get` lends the element. A `Copy` element is read by value; any
            // other is `&T`, or `&mut T` when the list itself came as `&mut`. `get_mut`
            // (`list_get(&mut xs, i)` once rewritten) is `&mut T` for a `Copy` element too.
            let get_mut = force_mut || matches!(args.first(), Some(Expr::Unary { op: UnOp::RefMut, .. }));
            if matches!(elem, Type::Ref { .. }) || (elem_is_copy(&elem, types) && !get_mut) {
                return Ok(elem);
            }
            let mutable = force_mut || matches!(xs_ty, Type::Ref { mutable: true, .. });
            return Ok(Type::Ref { mutable, lifetime: None, inner: Box::new(elem), span });
        }
    }
    Err(vec![Diagnostic::new(
        ErrorCode::EType,
        "list_get expects a List",
        span,
    )])
}

fn infer_list_set(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let xs_ty = match args.first() {
        Some(a) if args.len() == 3 => check_expr(a, env, ctx, types, entries)?,
        _ => {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "list_set expects a list, an index and an element",
                span,
            )])
        }
    };
    let elem = list_elem(&xs_ty).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EType, "list_set expects a List", span)]
    })?;
    check_expr(&args[1], env, ctx, types, entries)?;
    let xty = check_expr(&args[2], env, ctx, types, entries)?;
    if !types_compatible(&xty, &elem) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!(
                "list element type mismatch: expected {}, found {}",
                type_name(&elem),
                type_name(&xty)
            ),
            args[2].span(),
        )]);
    }
    Ok(peel_refs(&xs_ty).clone())
}

/// `list_pop(xs)`: removes the last element and moves it to the caller.
fn infer_list_pop(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    if let Some(a) = args.first() {
        let xs_ty = check_expr(a, env, ctx, types, entries)?;
        if let Some(elem) = list_elem(&xs_ty) {
            return Ok(elem);
        }
    }
    Err(vec![Diagnostic::new(ErrorCode::EType, "list_pop expects a List", span)])
}

fn infer_dict_has(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let dty = match args.first() {
        Some(a) if args.len() == 2 => check_expr(a, env, ctx, types, entries)?,
        _ => {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "dict_has expects a dict and a key",
                span,
            )])
        }
    };
    let (kty, _) = dict_kv(&dty).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EType, "dict_has expects a Dict", span)]
    })?;
    let kt = check_expr(&args[1], env, ctx, types, entries)?;
    if !dict_key_ok(&kt, &kty) {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "dict key type mismatch",
            args[1].span(),
        )]);
    }
    Ok(Type::Named { name: "Bool".into(), args: vec![], span })
}

/// `dict_len(d)` → Int, `dict_remove(d, k)` → Bool (was the key there?),
/// `dict_keys(d)` → `List[K]`, `dict_values(d)` → `List[V]` (fresh lists; the
/// String keys and values are copies).
fn infer_dict_extra(
    fname: &str,
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let want = if fname == "dict_remove" { 2 } else { 1 };
    if args.len() != want {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            format!("{fname} expects {want} argument(s)"),
            span,
        )]);
    }
    let dty = check_expr(&args[0], env, ctx, types, entries)?;
    let (kty, vty) = dict_kv(&dty).ok_or_else(|| {
        vec![Diagnostic::new(ErrorCode::EType, format!("{fname} expects a Dict"), span)]
    })?;
    let named = |n: &str| Type::Named { name: n.into(), args: vec![], span };
    match fname {
        "dict_len" => Ok(named("Int")),
        "dict_keys" => Ok(list_type(kty, span)),
        "dict_values" => Ok(list_type(vty, span)),
        _ => {
            let kt = check_expr(&args[1], env, ctx, types, entries)?;
            if !dict_key_ok(&kt, &kty) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "dict key type mismatch",
                    args[1].span(),
                )]);
            }
            Ok(named("Bool"))
        }
    }
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
                    "Dict value type must be a type",
                    span,
                )])
            }
        };
        if !is_dict_key_type(&k) || matches!(v, Type::Tensor { .. } | Type::Fn { .. }) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "Dict[K, V]: K is Int or String; V is not a Tensor or a function",
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
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let dty = if let Some(a) = args.first() {
        check_expr(a, env, ctx, types, entries)?
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
        let kt = check_expr(key, env, ctx, types, entries)?;
        if !dict_key_ok(&kt, &kty) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "dict key type mismatch",
                key.span(),
            )]);
        }
    }
    if let Some(val) = args.get(2) {
        let vt = check_expr(val, env, ctx, types, entries)?;
        if !types_compatible(&vt, &vty) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "dict value type mismatch",
                val.span(),
            )]);
        }
    }
    Ok(peel_refs(&dty).clone())
}

fn infer_dict_get(
    args: &[Expr],
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    // The runtime builds the value: `None` is variant 0 and `Some(v)` variant 1.
    let option_ok = types.enums.get("Option").is_some_and(|e| {
        e.variants.len() == 2
            && e.variants[0].name == "None"
            && e.variants[0].fields.is_empty()
            && e.variants[1].name == "Some"
            && e.variants[1].fields.len() == 1
    });
    if !option_ok {
        return Err(vec![Diagnostic::new(
            ErrorCode::EType,
            "dict_get requires Option with variants None and Some, in that order (import std/prelude.sal or define it)",
            span,
        )]);
    }
    let dty = if let Some(a) = args.first() {
        check_expr(a, env, ctx, types, entries)?
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
        let kt = check_expr(key, env, ctx, types, entries)?;
        if !dict_key_ok(&kt, &kty) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "dict key type mismatch",
                key.span(),
            )]);
        }
    }
    Ok(option_type(vty, span))
}

fn infer_clone_method(
    ty: &Type,
    base: &Expr,
    env: &HashMap<String, Type>,
    ctx: &ReturnCtx<'_>,
    types: &TypeEnv,
    entries: &mut Vec<ExprTypeEntry>,
    span: Span,
) -> DiagResult<Type> {
    let err = |msg: String| Err(vec![Diagnostic::new(ErrorCode::EType, msg, span)]);
    if is_unknown_type(ty) {
        return Ok(ty.clone());
    }
    if matches!(ty, Type::Tensor { .. }) {
        return err("`Tensor` has no `clone`: move it or copy it with `to`".into());
    }
    if matches!(ty, Type::Ref { .. } | Type::Fn { .. }) {
        return err("cannot clone a reference".into());
    }
    let builtin = match ty {
        Type::Named { name, .. } | Type::Qualified { name, .. } => match name.as_str() {
            "String" => "strdup",
            "List" => "list_clone",
            "Dict" => "dict_clone",
            "Bytes" => "bytes_clone",
            "Int" | "Float" | "Bool" | "Unit" => "clone_copy",
            other if types.enums.contains_key(other) => "enum_copy",
            _ => "struct_clone",
        },
        _ => "struct_clone",
    };
    let recv = "__recv".to_string();
    let mut env2 = env.clone();
    env2.insert(recv.clone(), ty.clone());
    let call_args = vec![Expr::Ident { name: recv, span: base.span() }];
    let callee = Expr::Ident { name: builtin.to_string(), span };
    let out = infer_call(&callee, &[], &call_args, &env2, ctx, types, entries, span)?;
    types
        .method_calls
        .borrow_mut()
        .insert((span.start, span.end), (builtin.to_string(), false));
    Ok(out)
}

fn is_string_type(t: &Type) -> bool {
    is_string_type_raw(peel_refs(t))
}

fn peel_refs(ty: &Type) -> &Type {
    match ty {
        Type::Ref { inner, .. } => peel_refs(inner),
        other => other,
    }
}

fn infer_ref_unary(op: UnOp, inner: Type, span: Span) -> Type {
    match op {
        // `&r` of a reference reborrows its referent (`&*r`): there is no `&&T`. A `&mut` of a
        // shared reference stays shared, so the mismatch shows where `&mut` is expected.
        UnOp::Ref => match inner {
            Type::Ref { inner, lifetime, .. } => Type::Ref { mutable: false, lifetime, inner, span },
            inner => Type::Ref { mutable: false, lifetime: None, inner: Box::new(inner), span },
        },
        UnOp::RefMut => match inner {
            Type::Ref { mutable, inner, lifetime, .. } => Type::Ref { mutable, lifetime, inner, span },
            inner => Type::Ref { mutable: true, lifetime: None, inner: Box::new(inner), span },
        },
        UnOp::Deref => match inner {
            Type::Ref { inner, .. } => *inner,
            other => other,
        },
        UnOp::Neg | UnOp::Not => inner,
    }
}

pub fn type_name(t: &Type) -> String {
    match t {
        Type::Named { name, .. } | Type::Qualified { name, .. } => name.clone(),
        Type::Tensor { elem, .. } => format!("Tensor[{elem:?}]"),
        Type::Fn { .. } => "fn".into(),
        Type::Ref { mutable, inner, .. } => {
            if *mutable {
                format!("&mut {}", type_name(inner))
            } else {
                format!("&{}", type_name(inner))
            }
        }
    }
}
