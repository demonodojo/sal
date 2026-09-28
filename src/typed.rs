use serde::{Deserialize, Serialize};

use crate::ast::*;

/// AST tipado: mismo árbol con modos de parámetro inferidos y tipos en nodos clave.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypedProgram {
    pub schema_version: u32,
    pub items: Vec<TypedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TypedItem {
    Fn(TypedFnDef),
    Import(Import),
    Struct(StructDef),
    Enum(EnumDef),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypedFnDef {
    pub def: FnDef,
    pub expr_types: Vec<ExprTypeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExprTypeEntry {
    pub span_start: u32,
    pub span_end: u32,
    pub ty: Type,
}

impl TypedProgram {
    pub fn from_program(p: Program, entries: Vec<(FnDef, Vec<ExprTypeEntry>)>) -> Self {
        let p = crate::ownership::infer_param_modes(&p);
        let mut items = Vec::new();
        for item in p.items {
            match item {
                Item::Import(i) => items.push(TypedItem::Import(i)),
                Item::Struct(s) => items.push(TypedItem::Struct(s)),
                Item::Frame(f) => {
                    items.push(TypedItem::Struct(crate::infer::frame_as_struct(&f)))
                }
                Item::Enum(e) => items.push(TypedItem::Enum(e)),
                Item::Fn(f) => {
                    let types = entries
                        .iter()
                        .find(|(fd, _)| fd.name == f.name)
                        .map(|(_, t)| t.clone())
                        .unwrap_or_default();
                    items.push(TypedItem::Fn(TypedFnDef {
                        def: f,
                        expr_types: types,
                    }));
                }
            }
        }
        Self {
            schema_version: AST_SCHEMA_VERSION,
            items,
        }
    }
}
