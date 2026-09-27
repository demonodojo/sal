use crate::ast::{BinOp, Expr, Type};

pub fn call_returns_string(name: &str) -> bool {
    matches!(
        name,
        "argv" | "read_file" | "str_concat" | "str_append" | "str_slice" | "int_to_str"
            | "char_to_str" | "strdup" | "select_str" | "tmp_path" | "exec_capture" | "getenv"
            | "ir_text"
    )
}

/// Heuristic for lowering/effects when types are not available (selfhost) or as a fast path.
pub fn expr_produces_string(e: &Expr) -> bool {
    match e {
        Expr::String { .. } => true,
        Expr::Call { func, .. } => {
            if let Expr::Ident { name, .. } = func.as_ref() {
                call_returns_string(name)
            } else {
                false
            }
        }
        Expr::Binary {
            op: BinOp::Add,
            left,
            right,
            ..
        } => expr_produces_string(left) || expr_produces_string(right),
        _ => false,
    }
}

pub fn binary_add_is_string_concat(e: &Expr) -> bool {
    match e {
        Expr::Binary {
            op: BinOp::Add,
            left,
            right,
            ..
        } => expr_produces_string(left) || expr_produces_string(right),
        _ => false,
    }
}

/// Left-assoc string `+`: the first pair is `str_concat`; later pairs `str_append` into the accumulator.
pub fn binary_add_uses_append(e: &Expr) -> bool {
    let Expr::Binary {
        op: BinOp::Add,
        left,
        ..
    } = e
    else {
        return false;
    };
    if !binary_add_is_string_concat(e) {
        return false;
    }
    matches!(left.as_ref(), Expr::Binary { op: BinOp::Add, .. })
        && binary_add_is_string_concat(left)
}

pub fn is_string_type(t: &Type) -> bool {
    matches!(t, Type::Named { name, .. } if name == "String")
}
