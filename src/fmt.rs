use crate::ast::*;
use crate::ownership::infer_param_modes;

pub fn format_program(p: &Program) -> String {
    let annotated = infer_param_modes(p);
    format_program_raw(&annotated)
}

fn format_program_raw(p: &Program) -> String {
    let mut out = String::new();
    let mut first = true;
    for item in &p.items {
        if !first {
            out.push('\n');
        }
        first = false;
        match item {
            Item::Import(imp) => out.push_str(&format!("import {}\n", imp.path)),
            Item::Fn(f) => out.push_str(&format_fn(f)),
            Item::Struct(s) => out.push_str(&format_struct(s)),
            Item::Enum(e) => out.push_str(&format_enum(e)),
        }
    }
    out
}

fn format_struct(s: &StructDef) -> String {
    let mut out = format!("struct {}", s.name);
    if !s.type_params.is_empty() {
        out.push('[');
        out.push_str(&s.type_params.join(", "));
        out.push(']');
    }
    out.push('\n');
    for f in &s.fields {
        out.push_str(&format!("    {}: {}\n", f.name, format_type(&f.ty)));
    }
    out
}

fn format_enum(e: &EnumDef) -> String {
    let mut out = format!("enum {}", e.name);
    if !e.type_params.is_empty() {
        out.push('[');
        out.push_str(&e.type_params.join(", "));
        out.push(']');
    }
    out.push('\n');
    for v in &e.variants {
        out.push_str("    ");
        out.push_str(&v.name);
        if !v.fields.is_empty() {
            out.push('(');
            out.push_str(
                &v.fields
                    .iter()
                    .map(format_type)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push(')');
        }
        out.push('\n');
    }
    out
}

fn format_fn(f: &FnDef) -> String {
    let mut s = format!("fn {}", f.name);
    if !f.type_params.is_empty() {
        s.push('[');
        s.push_str(&f.type_params.join(", "));
        s.push(']');
    }
    s.push('(');
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&p.name);
        s.push_str(": ");
        s.push_str(&format_type(&p.ty));
        match p.mode {
            ParamMode::Borrow => s.push_str(" borrow"),
            ParamMode::Take => s.push_str(" take"),
            ParamMode::Inferred => {}
        }
    }
    s.push_str(") -> ");
    s.push_str(&format_type(&f.ret));
    if !f.effects.is_empty() {
        s.push_str(" ! ");
        for (i, e) in f.effects.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(format_effect(e));
        }
    }
    s.push('\n');
    s.push_str(&format_block(&f.body, 1));
    s
}

fn format_effect(e: &Effect) -> &'static str {
    match e {
        Effect::Io => "io",
        Effect::Alloc => "alloc",
        Effect::Panic => "panic",
        Effect::Gpu => "gpu",
        Effect::Tpu => "tpu",
    }
}

fn format_block(b: &Block, indent: usize) -> String {
    let pad = "    ".repeat(indent);
    let mut s = String::new();
    for stmt in &b.stmts {
        s.push_str(&pad);
        s.push_str(&format_stmt(stmt, indent));
        s.push('\n');
    }
    if let Some(t) = &b.tail {
        s.push_str(&pad);
        s.push_str(&format_expr(t, indent));
        s.push('\n');
    }
    s
}

fn format_stmt(st: &Stmt, indent: usize) -> String {
    match st {
        Stmt::Let { name, ty, init, .. } => {
            let mut s = if let Some(t) = ty {
                format!("{name}: {} = ", format_type(t))
            } else {
                format!("{name} = ")
            };
            s.push_str(&format_expr(init, indent));
            s
        }
        Stmt::Expr(e) => format_expr(e, indent),
        Stmt::Assign { target, value, .. } => {
            format!(
                "{} = {}",
                format_expr(target, indent),
                format_expr(value, indent)
            )
        }
        Stmt::Return { value, .. } => match value {
            Some(v) => format!("return {}", format_expr(v, indent)),
            None => "return".to_string(),
        },
        Stmt::While { cond, body, .. } => {
            let mut s = format!("while {}\n", format_expr(cond, indent));
            s.push_str(&format_block(body, indent + 1));
            if s.ends_with('\n') {
                s.pop();
            }
            s
        }
    }
}

fn format_expr(e: &Expr, indent: usize) -> String {
    match e {
        Expr::Int { value, .. } => value.to_string(),
        Expr::Float { value, .. } => format_float(*value),
        Expr::Bool { value, .. } => value.to_string(),
        Expr::String { value, .. } => format!("\"{value}\""),
        Expr::Ident { name, .. } => name.clone(),
        Expr::Lambda { params, body, .. } => {
            let p = params.first().map(|s| s.as_str()).unwrap_or("_");
            format!("{} => {}", p, format_expr(body, indent))
        }
        Expr::Call {
            func,
            type_args,
            args,
            ..
        } => {
            let mut s = format_expr(func, indent);
            if !type_args.is_empty() {
                s.push('[');
                for (i, t) in type_args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&format_type_arg(t));
                }
                s.push(']');
            }
            s.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format_expr(a, indent));
            }
            s.push(')');
            s
        }
        Expr::Binary { op, left, right, .. } => {
            format!(
                "{} {} {}",
                format_expr(left, indent),
                format_binop(op),
                format_expr(right, indent)
            )
        }
        Expr::Unary { op, expr, .. } => {
            format!("{}{}", format_unop(op), format_expr(expr, indent))
        }
        Expr::Block(b) => {
            let body = format_block(b, indent + 1);
            format!("\n{}", body.trim_end_matches('\n'))
        }
        Expr::On {
            place,
            kernel,
            body,
            ..
        } => {
            let mut s = format!("on {}", format_place(place));
            if *kernel {
                s.push_str(" kernel");
            }
            s.push('\n');
            let body_s = format_block(body, indent + 1);
            s.push_str(body_s.trim_end_matches('\n'));
            s
        }
        Expr::To { place, expr, .. } => {
            format!("to {} {}", format_place(place), format_expr(expr, indent))
        }
        Expr::TensorLit { rows, .. } => {
            let mut s = "tensor[".to_string();
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push('[');
                for (j, c) in row.iter().enumerate() {
                    if j > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&format_expr(c, indent));
                }
                s.push(']');
            }
            s.push(']');
            s
        }
        Expr::Field { base, field, .. } => {
            format!("{}.{}", format_expr(base, indent), field)
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            let mut s = format!("match {}\n", format_expr(scrutinee, indent));
            let pad = "    ".repeat(indent + 1);
            for (i, arm) in arms.iter().enumerate() {
                if i > 0 {
                    s.push('\n');
                }
                s.push_str(&pad);
                s.push_str(&format_pattern(&arm.pattern));
                s.push_str(" => ");
                s.push_str(&format_expr(&arm.body, indent + 1));
            }
            s
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            let pad = "    ".repeat(indent);
            let mut s = format!("if {}\n", format_expr(cond, indent));
            s.push_str(&format_block(then_block, indent + 1));
            s.push_str(&pad);
            s.push_str("else\n");
            s.push_str(&format_block(else_block, indent + 1));
            // trim trailing newline from format_block for expression context — keep it
            if s.ends_with('\n') {
                s.pop();
            }
            s
        }
        Expr::Try { expr, .. } => format!("try {}", format_expr(expr, indent)),
    }
}

fn format_float(v: f64) -> String {
    let s = v.to_string();
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

fn format_binop(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
    }
}

fn format_unop(op: &UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
    }
}

fn format_type_arg(t: &TypeArg) -> String {
    match t {
        TypeArg::Type(ty) => format_type(ty),
        TypeArg::Dim(Dim::Static(n)) => n.to_string(),
        TypeArg::Dim(Dim::Dynamic) => "?".to_string(),
    }
}

fn format_type(t: &Type) -> String {
    match t {
        Type::Named { name, args, .. } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!(
                    "{}[{}]",
                    name,
                    args.iter()
                        .map(format_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        Type::Tensor {
            elem, dims, place, ..
        } => {
            let elem_s = match elem {
                TensorElem::F32 => "F32",
                TensorElem::F16 => "F16",
                TensorElem::BF16 => "BF16",
                TensorElem::I8 => "I8",
            };
            let dims_s = dims
                .iter()
                .map(|d| match d {
                    Dim::Static(n) => n.to_string(),
                    Dim::Dynamic => "?".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Tensor[{}, {}] on {}",
                elem_s,
                dims_s,
                format_place(place)
            )
        }
        Type::Fn {
            params,
            ret,
            effects,
            ..
        } => {
            let mut s = "fn(".to_string();
            s.push_str(
                &params
                    .iter()
                    .map(format_type)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            s.push_str(") -> ");
            s.push_str(&format_type(ret));
            if !effects.is_empty() {
                s.push_str(" ! ");
                s.push_str(
                    &effects
                        .iter()
                        .map(format_effect)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            s
        }
    }
}

fn format_place(p: &Place) -> String {
    match p {
        Place::Cpu => "cpu".to_string(),
        Place::Gpu => "gpu".to_string(),
        Place::Tpu => "tpu".to_string(),
        Place::Param(n) => n.clone(),
    }
}

fn format_pattern(p: &Pattern) -> String {
    match p {
        Pattern::Wild(_) => "_".to_string(),
        Pattern::Ident(n, _) => n.clone(),
        Pattern::Int(n, _) => n.to_string(),
        Pattern::Variant { name, args, .. } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!(
                    "{}({})",
                    name,
                    args.iter()
                        .map(format_pattern)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }
}
