//! Protocol contracts (`protocol P`) and header claims (`struct T impl P`).
//! Dynamic dispatch only through `any[P]`.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::infer::TypeEnv;
use crate::span::Span;

pub fn has_protocols(prog: &Program) -> bool {
    prog.items.iter().any(|i| {
        matches!(i, Item::Protocol(_))
            || matches!(i, Item::Struct(s) if !s.protocols.is_empty())
            || matches!(i, Item::Enum(e) if !e.protocols.is_empty())
    })
}

/// After `desugar_method_defs`, verify every `impl P` against existing `method T.m`.
pub fn check_program_protocols(prog: &Program, types: &TypeEnv) -> DiagResult<()> {
    let protocols = collect_protocols(prog);
    for item in &prog.items {
        match item {
            Item::Struct(s) => check_type_protocols(&s.name, &s.protocols, &s.type_params, s.span, types, &protocols)?,
            Item::Enum(e) => check_type_protocols(&e.name, &e.protocols, &e.type_params, e.span, types, &protocols)?,
            _ => {}
        }
    }
    Ok(())
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
            if let Err(d) = method_matches_protocol(type_name, pm, fparams, fret, types, &mangled) {
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
    let expected_self = Type::Named {
        name: type_name.to_string(),
        args: vec![],
        span: pm.span,
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
    protocol.methods.iter().position(|m| m.name == method)
}

pub fn drop_slot(protocol: &ProtocolDef) -> usize {
    protocol.methods.len()
}

/// Build vtable entries: each method fn name, then drop thunk name.
pub fn vtable_entries(type_name: &str, protocol: &ProtocolDef) -> Vec<String> {
    let mut out: Vec<String> = protocol
        .methods
        .iter()
        .map(|m| format!("{type_name}__{}", m.name))
        .collect();
    out.push(format!("sal_any_drop_{type_name}"));
    out
}
