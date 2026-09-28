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

pub fn enum_variant_index(def: &EnumDef, variant: &str) -> Option<usize> {
    def.variants.iter().position(|v| v.name == variant)
}
