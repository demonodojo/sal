use std::collections::HashMap;

use crate::ast::{EnumDef, StructDef};

/// Single-field struct: same SSA/ABI as its field (zero-cost newtype).
pub fn is_transparent_struct(def: &StructDef) -> bool {
    def.fields.len() == 1
}

pub fn struct_field_index(def: &StructDef, field: &str) -> Option<usize> {
    def.fields.iter().position(|f| f.name == field)
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

pub fn enum_variant_index(def: &EnumDef, variant: &str) -> Option<usize> {
    def.variants.iter().position(|v| v.name == variant)
}
