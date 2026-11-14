//! Protocol contracts (`protocol P`) and header claims (`struct T impl P`).
//! Dynamic dispatch only through `any[P]`.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::infer::{infer_program_with_env, TypeEnv};
use crate::span::Span;

pub fn has_protocols(prog: &Program) -> bool {
    prog.items.iter().any(|i| {
        matches!(i, Item::Protocol(_))
            || matches!(i, Item::Impl(_))
            || matches!(i, Item::Struct(s) if !s.protocols.is_empty())
            || matches!(i, Item::Enum(e) if !e.protocols.is_empty())
    })
}

/// After `desugar_method_defs`, verify every `impl P` against existing `method T.m`.
pub fn check_program_protocols(prog: &Program, types: &TypeEnv) -> DiagResult<()> {
    check_protocol_bodies(prog, types)?;
    let protocols = &types.protocols;
    for item in &prog.items {
        match item {
            Item::Struct(s) => check_type_protocols(&s.name, &s.protocols, &s.type_params, s.span, types, &protocols)?,
            Item::Enum(e) => check_type_protocols(&e.name, &e.protocols, &e.type_params, e.span, types, &protocols)?,
            Item::Impl(i) => check_impl(prog, i, types, protocols)?,
            _ => {}
        }
    }
    Ok(())
}

/// Validate provided bodies against the protocol's abstract receiver before expanding them for
/// implementing types. Giving `self` type `any[P]` makes method lookup see only P's vtable.
fn check_protocol_bodies(prog: &Program, types: &TypeEnv) -> DiagResult<()> {
    let mut checked = prog.clone();
    let mut synthetic = Vec::new();
    for item in &prog.items {
        let Item::Protocol(protocol) = item else { continue };
        for method in &protocol.methods {
            let Some(body) = &method.body else { continue };
            let span = method.span;
            let receiver = Type::Named {
                name: "any".into(),
                args: vec![Type::Named { name: protocol.name.clone(), args: vec![], span }],
                span,
            };
            let mut params = vec![Param {
                name: "self".into(),
                ty: receiver,
                mode: method.self_mode,
                span,
            }];
            params.extend(method.params.iter().cloned());
            synthetic.push(Item::Fn(FnDef {
                name: format!("__protocol_body_{}_{}", protocol.name, method.name),
                receiver: None,
                self_mode: method.self_mode,
                self_ref: method.self_ref,
                method_kind: MethodKind::Method,
                type_params: protocol.type_params.iter().chain(&method.type_params).cloned().collect(),
                params,
                ret: method.ret.clone(),
                effects: method.effects.clone(),
                body: body.clone(),
                no_heap: false,
                arena: false,
                deprecated: None,
                span,
            }));
        }
    }
    if synthetic.is_empty() {
        return Ok(());
    }
    checked.items.extend(synthetic);
    let inferred = infer_program_with_env(&checked, types)?;
    crate::effects::check_effects_with_infer(&checked, &inferred)?;
    for ((start, end), (protocol, method, _)) in inferred.protocol_calls {
        let Some(pm) = types.protocols.get(&protocol).and_then(|p| p.methods.iter().find(|m| m.name == method)) else {
            continue;
        };
        for effect in &pm.effects {
            if let Some(f) = checked.items.iter().find_map(|item| match item {
                Item::Fn(f) if f.name.starts_with("__protocol_body_") && f.body.span.start <= start && end <= f.body.span.end => Some(f),
                _ => None,
            }) {
                if !f.effects.contains(effect) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EEffect,
                        format!("protocol method `{}` requires effect `{:?}`", method, effect),
                        Span { start, end, line: f.span.line, col: f.span.col },
                    )]);
                }
            }
        }
    }
    Ok(())
}

/// Copy provided methods into each claimed type unless that type supplies an override.
pub fn instantiate_provided(prog: &mut Program) {
    let protocols = collect_protocols(prog);
    instantiate_provided_with_protocols(prog, &protocols);
}

/// Record standalone claims on types owned by this module for `any` coercions and vtables.
pub fn attach_local_impl_claims(prog: &mut Program) {
    let claims: Vec<(String, Vec<ProtocolClaim>)> = prog.items.iter().filter_map(|item| {
        let Item::Impl(imp) = item else { return None };
        let Type::Named { name, args, .. } = &imp.target else { return None };
        if !args.is_empty() { return None; }
        Some((name.clone(), imp.protocols.clone()))
    }).collect();
    for (name, added) in claims {
        for item in &mut prog.items {
            let protocols = match item {
                Item::Struct(s) if s.name == name => &mut s.protocols,
                Item::Enum(e) if e.name == name => &mut e.protocols,
                _ => continue,
            };
            protocols.extend(added.iter().cloned());
        }
    }
}

pub fn instantiate_provided_with_protocols(prog: &mut Program, protocols: &HashMap<String, ProtocolDef>) {
    let own_methods: HashSet<String> = prog.items.iter().filter_map(|item| match item {
        Item::Fn(f) => Some(f.name.clone()),
        _ => None,
    }).collect();
    let mut generated = Vec::new();
    for item in &prog.items {
        let (type_name, claims): (&str, &[ProtocolClaim]) = match item {
            Item::Struct(s) => (&s.name, &s.protocols),
            Item::Enum(e) => (&e.name, &e.protocols),
            Item::Impl(imp) => {
                if impl_type_params(&imp.target).is_some() {
                    instantiate_builtin_provided(imp, protocols, &own_methods, &mut generated);
                    continue;
                }
                let Type::Named { name, args, .. } = &imp.target else { continue };
                if !args.is_empty() { continue; }
                (name, &imp.protocols)
            }
            _ => continue,
        };
        for claim in claims {
            let Some(protocol) = protocols.get(&claim.name) else { continue };
            if protocol.type_params.len() != claim.args.len() {
                continue;
            }
            let sub: HashMap<String, Type> = protocol.type_params.iter().cloned().zip(claim.args.iter().cloned()).collect();
            for method in &protocol.methods {
                let Some(body) = &method.body else { continue };
                let name = format!("{type_name}__{}", method.name);
                if own_methods.contains(&name) {
                    continue;
                }
                let span = method.span;
                let named = Type::Named { name: type_name.into(), args: vec![], span };
                let self_ty = match method.self_ref {
                    1 => Type::Ref { mutable: false, lifetime: None, inner: Box::new(named), span },
                    2 => Type::Ref { mutable: true, lifetime: None, inner: Box::new(named), span },
                    _ => named,
                };
                let mut params = vec![Param { name: "self".into(), ty: self_ty, mode: method.self_mode, span }];
                params.extend(method.params.iter().cloned().map(|mut p| {
                    p.ty = crate::specialize::subst(&p.ty, &sub);
                    p
                }));
                let mut body = body.clone();
                crate::specialize::subst_block(&mut body, &sub);
                generated.push(Item::Fn(FnDef {
                    name,
                    receiver: None,
                    self_mode: method.self_mode,
                    self_ref: method.self_ref,
                    method_kind: MethodKind::Method,
                    type_params: method.type_params.clone(),
                    params,
                    ret: crate::specialize::subst(&method.ret, &sub),
                    effects: method.effects.clone(),
                    body,
                    no_heap: false,
                    arena: false,
                    deprecated: None,
                    span,
                }));
            }
        }
    }
    prog.items.extend(generated);
}

fn impl_type_params(target: &Type) -> Option<Vec<String>> {
    let Type::Named { name, args, .. } = target else { return None };
    if name != "List" || args.len() != 1 { return None; }
    match &args[0] {
        Type::Named { name, args, .. } if args.is_empty() => Some(vec![name.clone()]),
        _ => None,
    }
}

fn check_impl(prog: &Program, imp: &ImplDef, types: &TypeEnv, protocols: &HashMap<String, ProtocolDef>) -> DiagResult<()> {
    if impl_type_params(&imp.target).is_some() {
        return check_builtin_impl(prog, imp, protocols);
    }
    let Type::Named { name, args, .. } = &imp.target else {
        return Err(vec![Diagnostic::new(ErrorCode::EType, "unsupported impl target", imp.span)]);
    };
    if !args.is_empty() {
        return Err(vec![Diagnostic::new(ErrorCode::EType, "unsupported impl target", imp.span)]);
    }
    let type_params = if let Some(s) = types.structs.get(name) {
        &s.type_params
    } else if let Some(e) = types.enums.get(name) {
        &e.type_params
    } else {
        return Err(vec![Diagnostic::new(ErrorCode::EType, "unknown impl target", imp.span)]);
    };
    let owns_type = prog.items.iter().any(|item| match item {
        Item::Struct(s) => s.name == *name,
        Item::Enum(e) => e.name == *name,
        _ => false,
    });
    for claim in &imp.protocols {
        let owns_protocol = prog.items.iter().any(|item| matches!(item, Item::Protocol(p) if p.name == claim.name));
        if !owns_type && !owns_protocol {
            return Err(vec![Diagnostic::new(ErrorCode::EType, "impl must be in the type or protocol module", imp.span)]);
        }
    }
    check_type_protocols(name, &imp.protocols, type_params, imp.span, types, protocols)
}

fn check_builtin_impl(prog: &Program, imp: &ImplDef, protocols: &HashMap<String, ProtocolDef>) -> DiagResult<()> {
    let Some(_) = impl_type_params(&imp.target) else {
        return Err(vec![Diagnostic::new(ErrorCode::EType, "unsupported impl target", imp.span)]);
    };
    let Type::Named { args, .. } = &imp.target else { unreachable!() };
    let elem = args[0].clone();
    for claim in &imp.protocols {
        let Some(p) = protocols.get(&claim.name) else {
            return Err(vec![Diagnostic::new(ErrorCode::EType, format!("`{}` is not a protocol", claim.name), claim.span)]);
        };
        if !prog.items.iter().any(|i| matches!(i, Item::Protocol(own) if own.name == claim.name)) {
            return Err(vec![Diagnostic::new(ErrorCode::EType, "builtin impl must be in the protocol module", imp.span)]);
        }
        if p.type_params.len() != claim.args.len() {
            return Err(vec![Diagnostic::new(ErrorCode::EType, "protocol type argument count mismatch", claim.span)]);
        }
        let sub: HashMap<String, Type> = p.type_params.iter().cloned().zip(claim.args.iter().cloned()).collect();
        for method in &p.methods {
            if method.body.is_some() { continue; }
            let span = method.span;
            let int = Type::Named { name: "Int".into(), args: vec![], span };
            let unit = Type::Named { name: "Unit".into(), args: vec![], span };
            let (self_ref, params, ret) = match method.name.as_str() {
                "len" => (1, vec![], int.clone()),
                "get" => (1, vec![int.clone()], Type::Ref { mutable: false, lifetime: None, inner: Box::new(elem.clone()), span }),
                "set" => (2, vec![int, elem.clone()], unit.clone()),
                "push" => (2, vec![elem.clone()], unit),
                "pop" => (2, vec![], elem.clone()),
                _ => return Err(vec![Diagnostic::new(ErrorCode::EType, format!("List has no builtin method `{}`", method.name), span)]),
            };
            let actual: Vec<Type> = method.params.iter().map(|p| crate::specialize::subst(&p.ty, &sub)).collect();
            if method.self_ref != self_ref || !method.type_params.is_empty() || !method.effects.is_empty()
                || actual.len() != params.len() || !actual.iter().zip(&params).all(|(a,b)| types_equal_modulo_span(a,b))
                || !types_equal_modulo_span(&crate::specialize::subst(&method.ret, &sub), &ret)
            {
                return Err(vec![Diagnostic::new(ErrorCode::EType, format!("builtin List method `{}` does not match protocol signature", method.name), span)]);
            }
        }
    }
    Ok(())
}

fn instantiate_builtin_provided(imp: &ImplDef, protocols: &HashMap<String, ProtocolDef>, own: &HashSet<String>, generated: &mut Vec<Item>) {
    let Some(type_params) = impl_type_params(&imp.target) else { return };
    let Type::Named { name: target_name, .. } = &imp.target else { return };
    for claim in &imp.protocols {
        let Some(protocol) = protocols.get(&claim.name) else { continue };
        if protocol.type_params.len() != claim.args.len() { continue; }
        let sub: HashMap<String, Type> = protocol.type_params.iter().cloned().zip(claim.args.iter().cloned()).collect();
        for method in &protocol.methods {
            let Some(body) = &method.body else { continue };
            let name = format!("{target_name}__{}", method.name);
            if own.contains(&name) { continue; }
            let span = method.span;
            let self_ty = match method.self_ref {
                1 => Type::Ref { mutable: false, lifetime: None, inner: Box::new(imp.target.clone()), span },
                2 => Type::Ref { mutable: true, lifetime: None, inner: Box::new(imp.target.clone()), span },
                _ => imp.target.clone(),
            };
            let mut params = vec![Param { name: "self".into(), ty: self_ty, mode: method.self_mode, span }];
            params.extend(method.params.iter().cloned().map(|mut p| { p.ty = crate::specialize::subst(&p.ty, &sub); p }));
            let mut body = body.clone();
            crate::specialize::subst_block(&mut body, &sub);
            generated.push(Item::Fn(FnDef {
                name, receiver: None, self_mode: method.self_mode, self_ref: method.self_ref,
                method_kind: MethodKind::Method,
                type_params: type_params.iter().chain(&method.type_params).cloned().collect(),
                params, ret: crate::specialize::subst(&method.ret, &sub), effects: method.effects.clone(),
                body, no_heap: false, arena: false, deprecated: None, span,
            }));
        }
    }
}

fn collect_protocols(prog: &Program) -> HashMap<String, ProtocolDef> {
    let mut out = HashMap::new();
    for item in &prog.items {
        if let Item::Protocol(p) = item {
            out.insert(p.name.clone(), p.clone());
        }
    }
    out
}

fn check_type_protocols(
    type_name: &str,
    claims: &[ProtocolClaim],
    type_params: &[String],
    type_span: Span,
    types: &TypeEnv,
    protocols: &HashMap<String, ProtocolDef>,
) -> DiagResult<()> {
    if claims.is_empty() {
        return Ok(());
    }
    if !type_params.is_empty() {
        return Err(vec![Diagnostic::new(
            ErrorCode::EParse,
            "generic type cannot declare protocol impl",
            claims[0].span,
        )]);
    }
    let mut seen = HashSet::new();
    for claim in claims {
        if !seen.insert(claim.name.clone()) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("protocol `{}` repeated in impl clause", claim.name),
                claim.span,
            )]);
        }
        let Some(pdef) = protocols.get(&claim.name) else {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("`{}` is not a protocol", claim.name),
                claim.span,
            )]);
        };
        if pdef.type_params.len() != claim.args.len() {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                format!("protocol `{}` expects {} type arguments, found {}", claim.name, pdef.type_params.len(), claim.args.len()),
                claim.span,
            )]);
        }
        let sub: HashMap<String, Type> = pdef.type_params.iter().cloned().zip(claim.args.iter().cloned()).collect();
        for pm in &pdef.methods {
            let mangled = format!("{type_name}__{}", pm.name);
            let Some((fparams, fret)) = types.fns.get(&mangled) else {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!(
                        "type `{type_name}` missing method `{}` required by protocol `{}`",
                        pm.name, claim.name
                    ),
                    claim.span,
                )]);
            };
            let mut instantiated = pm.clone();
            for param in &mut instantiated.params {
                param.ty = crate::specialize::subst(&param.ty, &sub);
            }
            instantiated.ret = crate::specialize::subst(&instantiated.ret, &sub);
            let (raw_params, raw_ret) = if !pm.type_params.is_empty() {
                let Some(f) = types.fn_defs.get(&mangled) else { unreachable!() };
                if f.type_params.len() != pm.type_params.len() {
                    return Err(vec![Diagnostic::new(ErrorCode::EType, format!("method `{mangled}` type parameter count mismatch"), pm.span)]);
                }
                let rename: HashMap<String, Type> = f.type_params.iter().cloned().zip(pm.type_params.iter().map(|name| Type::Named { name: name.clone(), args: vec![], span: pm.span })).collect();
                (f.params.iter().map(|p| crate::specialize::subst(&p.ty, &rename)).collect(), crate::specialize::subst(&f.ret, &rename))
            } else {
                (fparams.clone(), fret.clone())
            };
            if let Err(d) = method_matches_protocol(type_name, &instantiated, &raw_params, &raw_ret, types, &mangled) {
                return Err(vec![d]);
            }
        }
    }
    let _ = type_span;
    Ok(())
}

fn method_matches_protocol(
    type_name: &str,
    pm: &ProtocolMethod,
    fparams: &[Type],
    fret: &Type,
    types: &TypeEnv,
    mangled: &str,
) -> Result<(), Diagnostic> {
    let named_self = Type::Named {
        name: type_name.to_string(),
        args: vec![],
        span: pm.span,
    };
    let expected_self = match pm.self_ref {
        1 => Type::Ref { mutable: false, lifetime: None, inner: Box::new(named_self), span: pm.span },
        2 => Type::Ref { mutable: true, lifetime: None, inner: Box::new(named_self), span: pm.span },
        _ => named_self,
    };
    if fparams.is_empty() {
        return Err(Diagnostic::new(
            ErrorCode::EType,
            format!("method `{mangled}` missing `self`"),
            pm.span,
        ));
    }
    if !types_equal_modulo_span(&fparams[0], &expected_self) {
        return Err(Diagnostic::new(
            ErrorCode::EType,
            format!(
                "method `{mangled}` self type mismatch for protocol `{}`",
                pm.name
            ),
            pm.span,
        ));
    }
    let want_params: Vec<Type> = pm.params.iter().map(|p| p.ty.clone()).collect();
    if fparams.len() != want_params.len() + 1 {
        return Err(Diagnostic::new(
            ErrorCode::EType,
            format!("method `{mangled}` arity mismatch for protocol `{}`", pm.name),
            pm.span,
        ));
    }
    for (got, want) in fparams.iter().skip(1).zip(want_params.iter()) {
        if !types_equal_modulo_span(got, want) {
            return Err(Diagnostic::new(
                ErrorCode::EType,
                format!("method `{mangled}` parameter mismatch for protocol `{}`", pm.name),
                pm.span,
            ));
        }
    }
    if !types_equal_modulo_span(fret, &pm.ret) {
        return Err(Diagnostic::new(
            ErrorCode::EType,
            format!("method `{mangled}` return mismatch for protocol `{}`", pm.name),
            pm.span,
        ));
    }
    if let Some(fdef) = types.fn_defs.get(mangled) {
        if fdef.effects != pm.effects {
            return Err(Diagnostic::new(
                ErrorCode::EType,
                format!("method `{mangled}` effects mismatch for protocol `{}`", pm.name),
                pm.span,
            ));
        }
        if fdef.self_mode != pm.self_mode {
            return Err(Diagnostic::new(
                ErrorCode::EType,
                format!("method `{mangled}` self mode mismatch for protocol `{}`", pm.name),
                pm.span,
            ));
        }
    }
    Ok(())
}

fn types_equal_modulo_span(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (
            Type::Named { name: na, args: aa, .. },
            Type::Named { name: nb, args: ab, .. },
        ) => na == nb && aa.len() == ab.len() && aa.iter().zip(ab).all(|(x, y)| types_equal_modulo_span(x, y)),
        (Type::Tensor { elem: ea, dims: da, place: pa, .. }, Type::Tensor { elem: eb, dims: db, place: pb, .. }) => {
            ea == eb && da == db && pa == pb
        }
        (Type::Fn { params: pa, ret: ra, effects: ea, .. }, Type::Fn { params: pb, ret: rb, effects: eb, .. }) => {
            ea == eb
                && pa.len() == pb.len()
                && pa.iter().zip(pb).all(|(x, y)| types_equal_modulo_span(x, y))
                && types_equal_modulo_span(ra, rb)
        }
        (Type::Ref { mutable: ma, inner: ia, .. }, Type::Ref { mutable: mb, inner: ib, .. }) => {
            ma == mb && types_equal_modulo_span(ia, ib)
        }
        _ => false,
    }
}

/// Whether nominal type `type_name` claimed protocol `protocol` in its header.
pub fn type_claims_protocol(types: &TypeEnv, type_name: &str, protocol: &str) -> bool {
    if let Some(s) = types.structs.get(type_name) {
        return s.protocols.iter().any(|p| p.name == protocol);
    }
    if let Some(e) = types.enums.get(type_name) {
        return e.protocols.iter().any(|p| p.name == protocol);
    }
    false
}

pub fn protocol_def<'a>(types: &'a TypeEnv, name: &str) -> Option<&'a ProtocolDef> {
    types.protocols.get(name)
}

pub fn vtable_global(type_name: &str, protocol: &str) -> String {
    format!("sal_vtable_{type_name}_{protocol}")
}

pub fn any_coerce_vtable(type_name: &str, protocol: &str) -> String {
    vtable_global(type_name, protocol)
}

pub fn method_slot(protocol: &ProtocolDef, method: &str) -> Option<usize> {
    protocol.methods.iter().filter(|m| m.type_params.is_empty()).position(|m| m.name == method)
}

pub fn drop_slot(protocol: &ProtocolDef) -> usize {
    protocol.methods.iter().filter(|m| m.type_params.is_empty()).count()
}

/// Build vtable entries: each method fn name, then drop thunk name.
pub fn vtable_entries(type_name: &str, protocol: &ProtocolDef) -> Vec<String> {
    let mut out: Vec<String> = protocol
        .methods
        .iter()
        .filter(|m| m.type_params.is_empty())
        .map(|m| format!("{type_name}__{}", m.name))
        .collect();
    out.push(format!("sal_any_drop_{type_name}"));
    out
}
