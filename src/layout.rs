use std::collections::HashMap;

use crate::ast::{EnumDef, StructDef, Type};
use crate::infer::subst_type_params as subst_type_params_public;

/// Single-field struct: same SSA/ABI as its field (zero-cost newtype).
pub fn is_transparent_struct(def: &StructDef) -> bool {
    def.fields.len() == 1
}

pub fn struct_field_index(def: &StructDef, field: &str) -> Option<usize> {
    def.fields.iter().position(|f| f.name == field)
}

pub fn subst_struct_type_params(
    def: &StructDef,
    type_args: &[Type],
    span: crate::span::Span,
) -> Vec<Type> {
    def.fields
        .iter()
        .map(|f| subst_type_params_public(&f.ty, &def.type_params, type_args, span))
        .collect()
}

pub fn llvm_struct_symbol(name: &str) -> String {
    format!("%sal.struct.{name}.ty")
}

pub fn collect_struct_defs(prog: &crate::ast::Program) -> HashMap<String, StructDef> {
    let mut m = HashMap::new();
    for item in &prog.items {
        if let crate::ast::Item::Struct(s) = item {
            m.insert(s.name.clone(), s.clone());
        }
    }
    m
}

/// Struct and frame layouts (frames lower to the same ABI as `frame_as_struct`).
pub fn collect_layout_structs(prog: &crate::ast::Program) -> HashMap<String, StructDef> {
    let mut m = collect_struct_defs(prog);
    for item in &prog.items {
        if let crate::ast::Item::Frame(f) = item {
            m.insert(f.name.clone(), crate::infer::frame_as_struct(f));
        }
    }
    m
}

pub fn collect_enum_defs(prog: &crate::ast::Program) -> HashMap<String, EnumDef> {
    let mut m = HashMap::new();
    for item in &prog.items {
        if let crate::ast::Item::Enum(e) = item {
            m.insert(e.name.clone(), e.clone());
        }
    }
    m
}

/// LLVM field slot: all user scalars and handles use i64 in the arranque ABI.
pub fn field_slots_for_struct(def: &StructDef) -> usize {
    if is_transparent_struct(def) {
        1
    } else {
        def.fields.len()
    }
}

/// LLVM field type for a struct field (`i64` when not `@layout(c)`).
pub fn llvm_field_type(ty: &Type, layout_c: bool) -> &'static str {
    if !layout_c {
        return "i64";
    }
    match ty {
        Type::Named { name, args, .. } if args.is_empty() => match name.as_str() {
            "Int" => "i64",
            "Float" => "double",
            "Bool" => "i8",
            _ => "i64",
        },
        _ => "i64",
    }
}

pub fn enum_variant_index(def: &EnumDef, variant: &str) -> Option<usize> {
    def.variants.iter().position(|v| v.name == variant)
}
