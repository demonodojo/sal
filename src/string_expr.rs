use std::collections::HashSet;

use crate::ast::{BinOp, Expr, Item, Param, Program, Type};

pub fn call_returns_string(name: &str) -> bool {
    matches!(
        name,
        "argv" | "read_file" | "str_concat" | "str_append" | "str_slice" | "int_to_str"
            | "char_to_str" | "strdup" | "select_str" | "tmp_path" | "exec_capture" | "getenv"
            | "ir_text"
    )
}

/// Names whose current type is `String`, plus user functions that return `String`.
/// The right operand of `+` does not have to be in this set: concatenation is
/// decided by the left operand (or by a child that already produces a string).
#[derive(Clone, Debug, Default)]
pub struct StringEnv {
    pub locals: HashSet<String>,
    pub fns: HashSet<String>,
}

impl StringEnv {
    pub fn from_program(prog: &Program) -> Self {
        let mut fns = HashSet::new();
        for item in &prog.items {
            if let Item::Fn(f) = item {
                if is_string_type(&f.ret) {
                    fns.insert(f.name.clone());
                }
            }
        }
        Self {
            locals: HashSet::new(),
            fns,
        }
    }

    pub fn seed_params(&mut self, params: &[Param]) {
        for p in params {
            self.note(&p.name, is_string_type(&p.ty));
        }
    }

    pub fn note(&mut self, name: &str, is_string: bool) {
        if is_string {
            self.locals.insert(name.to_string());
        } else {
            self.locals.remove(name);
        }
    }

    /// A declared type wins. Without one, the initializer decides.
    pub fn note_init(&mut self, name: &str, declared: Option<&Type>, init: &Expr) {
        let is_string = if let Some(ty) = declared {
            is_string_type(ty)
        } else {
            expr_produces_string_with(init, self)
        };
        self.note(name, is_string);
    }
}

pub fn expr_produces_string(e: &Expr) -> bool {
    expr_produces_string_with(e, &StringEnv::default())
}

pub fn expr_produces_string_with(e: &Expr, env: &StringEnv) -> bool {
    match e {
        Expr::String { .. } => true,
        Expr::Ident { name, .. } => env.locals.contains(name),
        Expr::Call { func, .. } => {
            if let Expr::Ident { name, .. } = func.as_ref() {
                call_returns_string(name) || env.fns.contains(name)
            } else {
                false
            }
        }
        Expr::Binary {
            op: BinOp::Add,
            left,
            right,
            ..
        } => expr_produces_string_with(left, env) || expr_produces_string_with(right, env),
        _ => false,
    }
}

pub fn binary_add_is_string_concat(e: &Expr) -> bool {
    binary_add_is_string_concat_with(e, &StringEnv::default())
}

pub fn binary_add_is_string_concat_with(e: &Expr, env: &StringEnv) -> bool {
    match e {
        Expr::Binary {
            op: BinOp::Add,
            left,
            right,
            ..
        } => expr_produces_string_with(left, env) || expr_produces_string_with(right, env),
        _ => false,
    }
}

/// Left-assoc string `+`: the first pair is `str_concat`; later pairs `str_append`.
/// The right operand is passed through like the second argument of those calls
/// and does not itself have to be a `String`.
pub fn binary_add_uses_append(e: &Expr) -> bool {
    binary_add_uses_append_with(e, &StringEnv::default())
}

pub fn binary_add_uses_append_with(e: &Expr, env: &StringEnv) -> bool {
    let Expr::Binary {
        op: BinOp::Add,
        left,
        ..
    } = e
    else {
        return false;
    };
    if !binary_add_is_string_concat_with(e, env) {
        return false;
    }
    matches!(left.as_ref(), Expr::Binary { op: BinOp::Add, .. })
        && binary_add_is_string_concat_with(left, env)
}

pub fn is_string_type(t: &Type) -> bool {
    matches!(t, Type::Named { name, .. } if name == "String")
}
