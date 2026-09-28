use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::effects::fn_body_uses_alloc;
use crate::string_expr::StringEnv;

pub fn is_stack_type(ty: &Type, structs: &HashMap<String, StructDef>) -> bool {
    is_stack_type_rec(ty, structs, &mut HashSet::new())
}

fn is_stack_type_rec(
    ty: &Type,
    structs: &HashMap<String, StructDef>,
    visiting: &mut HashSet<String>,
) -> bool {
    match ty {
        Type::Named { name, args, .. } => {
            if !args.is_empty() {
                return false;
            }
            match name.as_str() {
                "Int" | "Float" | "Bool" | "Unit" => true,
                "String" | "List" | "Dict" | "Tensor" => false,
                _ => {
                    if !visiting.insert(name.clone()) {
                        return false;
                    }
                    let ok = structs.get(name).is_some_and(|def| {
                        def.fields
                            .iter()
                            .all(|f| is_stack_type_rec(&f.ty, structs, visiting))
                    });
                    visiting.remove(name);
                    ok
                }
            }
        }
        Type::Tensor { .. } | Type::Fn { .. } => false,
    }
}

pub fn type_uses_heap(ty: &Type, structs: &HashMap<String, StructDef>) -> bool {
    !is_stack_type(ty, structs)
}

pub fn layout_c_field_ok(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named { name, args, .. }
            if args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool")
    )
}

pub fn check_layout_structs(prog: &Program) -> DiagResult<()> {
    for item in &prog.items {
        if let Item::Struct(s) = item {
            if !s.layout_c {
                continue;
            }
            for f in &s.fields {
                if !layout_c_field_ok(&f.ty) {
                    return Err(vec![Diagnostic::new(
                        ErrorCode::EType,
                        "@layout(c) struct fields must be Int, Float or Bool",
                        f.span,
                    )]);
                }
            }
        }
    }
    Ok(())
}

pub fn check_no_heap_fns(prog: &Program, env: &StringEnv) -> DiagResult<()> {
    let structs = collect_structs(prog);
    for item in &prog.items {
        let Item::Fn(f) = item else { continue };
        if !f.no_heap {
            continue;
        }
        if type_uses_heap(&f.ret, &structs) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "@no_heap function cannot return a heap type",
                f.span,
            )]);
        }
        for p in &f.params {
            if type_uses_heap(&p.ty, &structs) {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    format!("@no_heap function cannot take heap parameter `{}`", p.name),
                    p.span,
                )]);
            }
        }
        if fn_body_uses_alloc(f, env) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EType,
                "@no_heap function body cannot allocate",
                f.span,
            )]);
        }
        check_block_no_heap_types(&f.body, &structs)?;
    }
    Ok(())
}

fn check_block_no_heap_types(b: &Block, structs: &HashMap<String, StructDef>) -> DiagResult<()> {
    for st in &b.stmts {
        match st {
            Stmt::Let { ty: Some(t), span, .. } if type_uses_heap(t, structs) => {
                return Err(vec![Diagnostic::new(
                    ErrorCode::EType,
                    "@no_heap function cannot bind a heap type",
                    *span,
                )]);
            }
            Stmt::While { body, .. } => check_block_no_heap_types(body, structs)?,
            _ => {}
        }
    }
    Ok(())
}

fn collect_structs(prog: &Program) -> HashMap<String, StructDef> {
    let mut m = HashMap::new();
    for item in &prog.items {
        if let Item::Struct(s) = item {
            m.insert(s.name.clone(), s.clone());
        }
    }
    m
}
