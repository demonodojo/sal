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
            Item::Import(imp) => {
                let path = format_import_path(&imp.path);
                if let Some(alias) = &imp.alias {
                    out.push_str(&format!("import {} as {}\n", path, alias));
                } else {
                    out.push_str(&format!("import {}\n", path));
                }
            }
            Item::Fn(f) => out.push_str(&format_fn(f)),
            Item::Struct(s) => out.push_str(&format_struct(s)),
            Item::Frame(f) => out.push_str(&format_frame(f)),
            Item::Enum(e) => out.push_str(&format_enum(e)),
            Item::Protocol(p) => out.push_str(&format_protocol(p)),
        }
    }
    out
}

fn format_impl_clause(protocols: &[ProtocolClaim]) -> String {
    if protocols.is_empty() {
        return String::new();
    }
    let mut out = String::from(" impl ");
    for (i, p) in protocols.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&p.name);
    }
    out
}

/// `@deprecated("…")` line of an item, when it has one.
fn deprecated_line(msg: &Option<String>) -> String {
    match msg {
        Some(m) => format!("@deprecated(\"{}\")\n", m.replace('\\', "\\\\").replace('"', "\\\"")),
        None => String::new(),
    }
}

fn format_struct(s: &StructDef) -> String {
    let mut out = deprecated_line(&s.deprecated);
    if s.layout_c {
        out.push_str("@layout(c)\n");
    }
    out.push_str(&format!("struct {}", s.name));
    if !s.type_params.is_empty() {
        out.push('[');
        out.push_str(&s.type_params.join(", "));
        out.push(']');
    }
    out.push_str(&format_impl_clause(&s.protocols));
    out.push('\n');
    for f in &s.fields {
        out.push_str(&format!("    {}: {}\n", f.name, format_type(&f.ty)));
    }
    out
}

fn format_enum(e: &EnumDef) -> String {
    let mut out = deprecated_line(&e.deprecated);
    out.push_str(&format!("enum {}", e.name));
    if !e.type_params.is_empty() {
        out.push('[');
        out.push_str(&e.type_params.join(", "));
        out.push(']');
    }
    out.push_str(&format_impl_clause(&e.protocols));
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

fn format_protocol(p: &ProtocolDef) -> String {
    let mut out = format!("protocol {}\n", p.name);
    for m in &p.methods {
        out.push_str("    method ");
        out.push_str(&m.name);
        out.push('(');
        out.push_str("self");
        match m.self_mode {
            ParamMode::Take => out.push_str(" take"),
            ParamMode::Borrow => out.push_str(" borrow"),
            ParamMode::Inferred => {}
        }
        for param in &m.params {
            out.push_str(", ");
            out.push_str(&param.name);
            out.push_str(": ");
            out.push_str(&format_type(&param.ty));
            match param.mode {
                ParamMode::Borrow => out.push_str(" borrow"),
                ParamMode::Take => out.push_str(" take"),
                ParamMode::Inferred => {}
            }
        }
        out.push_str(") -> ");
        out.push_str(&format_type(&m.ret));
        if !m.effects.is_empty() {
            out.push_str(" ! ");
            out.push_str(
                &m
                    .effects
                    .iter()
                    .map(format_effect)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        out.push('\n');
    }
    out
}

fn format_frame(f: &FrameDef) -> String {
    let mut out = format!("frame {}", f.name);
    if !f.type_params.is_empty() {
        out.push('[');
        out.push_str(&f.type_params.join(", "));
        out.push(']');
    }
    out.push('\n');
    for c in &f.columns {
        out.push_str("    ");
        out.push_str(&c.name);
        out.push_str(": ");
        out.push_str(match c.elem {
            ColumnElem::F32 => "F32",
            ColumnElem::F16 => "F16",
            ColumnElem::BF16 => "BF16",
            ColumnElem::I8 => "I8",
            ColumnElem::String => "String",
        });
        out.push('\n');
    }
    out
}

fn format_fn(f: &FnDef) -> String {
    let mut s = deprecated_line(&f.deprecated);
    if f.no_heap {
        s.push_str("@no_heap\n");
    }
    match (&f.receiver, f.method_kind) {
        (Some(t), MethodKind::Static) => s.push_str(&format!("static {}.{}", t, f.name)),
        (Some(t), MethodKind::Init) => s.push_str(&format!("init {}", t)),
        (Some(t), MethodKind::Method) => s.push_str(&format!("method {}.{}", t, f.name)),
        (None, _) => s.push_str(&format!("fn {}", f.name)),
    }
    if !f.type_params.is_empty() {
        s.push('[');
        s.push_str(&f.type_params.join(", "));
        s.push(']');
    }
    s.push('(');
    let mut wrote = false;
    if f.receiver.is_some() && f.self_mode != ParamMode::Inferred {
        s.push_str(match f.self_mode {
            ParamMode::Take => "self take",
            _ => "self borrow",
        });
        wrote = true;
    }
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 || wrote {
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
    s.push(')');
    if f.receiver.is_none() || f.method_kind != MethodKind::Init {
        s.push_str(" -> ");
        s.push_str(&format_type(&f.ret));
    }
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
        Stmt::Let { name, ty, init, frozen, stack, .. } => {
            let mut s = String::new();
            if *frozen {
                s.push_str("@frozen ");
            }
            if *stack {
                s.push_str("@stack ");
            }
            s.push_str(&if let Some(t) = ty {
                format!("let {name}: {} = ", format_type(t))
            } else if *frozen || *stack {
                format!("let {name} = ")
            } else {
                format!("{name} = ")
            });
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
        Stmt::Break { .. } => "break".to_string(),
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
        Expr::String { value, parts, .. } => format_string_literal(value, parts),
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
            let mut s = format_postfix_base(func, indent);
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
            let p = binop_prec(op);
            // Left associative: the right operand needs parentheses at equal precedence.
            let l = format_operand(left, p, indent);
            let r = format_operand(right, p + 1, indent);
            format!("{} {} {}", l, format_binop(op), r)
        }
        Expr::Unary { op, expr, .. } => {
            format!("{}{}", format_unop(op), format_operand(expr, ATOM_PREC, indent))
        }
        Expr::Block(b) => {
            let body = format_block(b, indent + 1);
            format!("\n{}", body.trim_end_matches('\n'))
        }
        Expr::On {
            place,
            kernel,
            kernel_index,
            body,
            ..
        } => {
            let mut s = format!("on {}", format_place(place));
            if *kernel {
                s.push_str(" kernel");
                if let Some(ki) = kernel_index {
                    s.push(' ');
                    for (i, n) in ki.names.iter().enumerate() {
                        if i > 0 {
                            s.push_str(", ");
                        }
                        s.push_str(n);
                    }
                    s.push_str(" in ");
                    match &ki.shape {
                        crate::ast::KernelShape::Type(t) => s.push_str(&format_type(t)),
                        crate::ast::KernelShape::Binding(b) => s.push_str(b),
                    }
                }
            }
            s.push('\n');
            let body_s = format_block(body, indent + 1);
            s.push_str(body_s.trim_end_matches('\n'));
            s
        }
        Expr::To { place, expr, .. } => {
            format!("to {} {}", format_place(place), format_operand(expr, ATOM_PREC, indent))
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
            format!("{}.{}", format_postfix_base(base, indent), field)
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
                s.push_str(" =>");
                if matches!(arm.body, Expr::Block(_)) {
                    s.push('\n');
                    s.push_str(&format_expr(&arm.body, indent + 1));
                } else {
                    s.push(' ');
                    s.push_str(&format_expr(&arm.body, indent + 1));
                }
            }
            s
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            ..
        } => {
            let pad = "    ".repeat(indent);
            let mut s = format!("if {}\n", format_expr(cond, indent));
            s.push_str(&format_block(then_block, indent + 1));
            for arm in elsifs {
                s.push_str(&pad);
                s.push_str(&format!("elsif {}\n", format_expr(&arm.cond, indent)));
                s.push_str(&format_block(&arm.body, indent + 1));
            }
            if let Some(else_block) = else_block {
                s.push_str(&pad);
                s.push_str("else\n");
                s.push_str(&format_block(else_block, indent + 1));
            }
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
        BinOp::Mod => "%",
        BinOp::Band => "&",
        BinOp::Bxor => "^",
        BinOp::Bor => "|",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

fn format_unop(op: &UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
    }
}

fn format_type_name(name: &str, args: &[Type]) -> String {
    if args.is_empty() {
        name.to_string()
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

fn format_type_arg(t: &TypeArg) -> String {
    match t {
        TypeArg::Type(ty) => format_type(ty),
        TypeArg::Dim(Dim::Static(n)) => n.to_string(),
        TypeArg::Dim(Dim::Dynamic) => "?".to_string(),
    }
}

fn format_type(t: &Type) -> String {
    match t {
        Type::Named { name, args, .. } => format_type_name(name, args),
        Type::Qualified {
            qual,
            name,
            args,
            ..
        } => format!("{}.{}", qual, format_type_name(name, args)),
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
        Pattern::Str(s, _) => {
            let mut out = String::from("\"");
            escape_string_text(s, &mut out);
            out.push('"');
            out
        }
        Pattern::Regex(s, _) => {
            let mut out = String::from("r\"");
            escape_string_text(s, &mut out);
            out.push('"');
            out
        }
        Pattern::Variant {
            qual,
            name,
            args,
            ..
        } => {
            let head = match qual {
                Some(q) => format!("{q}.{name}"),
                None => name.clone(),
            };
            if args.is_empty() && qual.is_none() {
                head
            } else {
                format!(
                    "{}({})",
                    head,
                    args.iter()
                        .map(format_pattern)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }
}

/// `std.io` stays a dotted path; anything that is a file path (`"a/b.sal"`)
/// goes back between quotes so it parses again.
fn format_import_path(path: &str) -> String {
    let dotted = !path.is_empty()
        && !path.ends_with(".sal")
        && path
            .split('.')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_alphanumeric() || c == '_'));
    if dotted {
        path.to_string()
    } else {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn escape_string_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

fn escape_fstring_lit(text: &str, out: &mut String) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '#' if chars.peek() == Some(&'{') => {
                out.push('#');
                out.push('#');
                chars.next();
                out.push('{');
            }
            c => out.push(c),
        }
    }
}

/// A string literal back to source: plain `"…"` or `f"…#{name}…"`.
fn format_string_literal(value: &str, parts: &[StringPart]) -> String {
    if parts.is_empty() {
        let mut out = String::from("\"");
        escape_string_text(value, &mut out);
        out.push('"');
        return out;
    }
    let mut out = String::from("f\"");
    for part in parts {
        match part {
            StringPart::Lit(t) => escape_fstring_lit(t, &mut out),
            StringPart::Interp(n) => {
                out.push('#');
                out.push('{');
                out.push_str(n);
                out.push('}');
            }
        }
    }
    out.push('"');
    out
}

/// Precedence levels of the SPEC grammar, loosest first.
const ATOM_PREC: u8 = 10;

fn binop_prec(op: &BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        BinOp::Bor => 4,
        BinOp::Bxor => 5,
        BinOp::Band => 6,
        BinOp::Shl | BinOp::Shr => 7,
        BinOp::Add | BinOp::Sub => 8,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 9,
    }
}

fn expr_prec(e: &Expr) -> u8 {
    match e {
        Expr::Binary { op, .. } => binop_prec(op),
        // `try` and lambdas bind looser than every binary operator; `to` and
        // blocks that span lines are wrapped too so they stay one operand.
        Expr::Lambda { .. } | Expr::Try { .. } => 0,
        Expr::To { .. } | Expr::Match { .. } | Expr::If { .. } | Expr::On { .. } | Expr::Block(_) => 1,
        _ => ATOM_PREC,
    }
}

/// The base of `.field` or of a call: anything looser than a postfix
/// (operators, prefixes, `try`, lambdas, `if`/`match`/`to`…) keeps its parentheses,
/// so `(a - b).abs()` does not turn into `a - b.abs()`.
fn format_postfix_base(e: &Expr, indent: usize) -> String {
    let s = format_expr(e, indent);
    if expr_prec(e) < ATOM_PREC || matches!(e, Expr::Unary { .. }) {
        format!("({s})")
    } else {
        s
    }
}

/// `e` as an operand that needs at least precedence `min`.
fn format_operand(e: &Expr, min: u8, indent: usize) -> String {
    let s = format_expr(e, indent);
    if expr_prec(e) < min {
        format!("({s})")
    } else {
        s
    }
}
