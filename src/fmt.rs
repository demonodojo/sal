use crate::ast::*;
use crate::lexer::Comment;
use crate::ownership::infer_param_modes;
use crate::span::Span;

pub fn format_program(p: &Program) -> String {
    strip_markers(&format_marked(p, None))
}

/// `sal fmt` with the source at hand: every `#…` comment goes back in place,
/// above the node that starts on the following line, or on its own line when it
/// trailed code. The AST does not hold comments, so they are matched by line.
pub fn format_program_with_comments(p: &Program, source: &str) -> String {
    let Ok((tokens, comments)) = crate::lexer::lex_with_comments(source) else {
        return strip_markers(&format_marked(p, None));
    };
    let starts = top_level_item_lines(&tokens);
    let elses = else_lines(&tokens);
    let marked = format_marked(p, Some(&starts));
    apply_comments(&marked, &comments, &elses)
}

fn format_marked(p: &Program, item_starts: Option<&[u32]>) -> String {
    // The mode analysis runs the semantic passes, which only see a program after
    // `inline for` is unrolled; such a program is written as parsed.
    if crate::inline_for::has_inline_for(p) {
        return format_program_raw(p, item_starts);
    }
    let mut annotated = infer_param_modes(p);
    // `caller_param_modes` promotes an inferred borrow to take for callers when the
    // body consumes the parameter. Writing `borrow` would freeze the declared mode
    // and the caller would free the value again. Leave that mode unwritten.
    hide_consumed_inferred_borrows(p, &mut annotated);
    format_program_raw(&annotated, item_starts)
}

/// Line of the first token of each top-level item, in source order. An item's
/// own span does not always start at its first line (`FnDef.span` is the body's),
/// and `sal fmt` needs that line to put the comments written above it back.
fn top_level_item_lines(tokens: &[crate::lexer::Token]) -> Vec<u32> {
    let mut lines = Vec::new();
    let mut depth = 0usize;
    let mut expect_start = true;
    for tok in tokens {
        match tok.kind {
            crate::lexer::TokenKind::Indent => depth += 1,
            crate::lexer::TokenKind::Dedent => depth = depth.saturating_sub(1),
            crate::lexer::TokenKind::Newline if depth == 0 => expect_start = true,
            crate::lexer::TokenKind::Eof => {}
            _ if depth == 0 && expect_start => {
                lines.push(tok.span.line);
                expect_start = false;
            }
            _ => {}
        }
    }
    lines
}

/// Line of each `else` keyword, in source order. The AST keeps the else block
/// but not the line of its keyword, and `sal fmt` needs it to keep the comments
/// written above the `else` out of the block it opens. The formatter emits the
/// `else` keywords in that same order, so `apply_comments` pairs them up.
fn else_lines(tokens: &[crate::lexer::Token]) -> Vec<u32> {
    tokens
        .iter()
        .filter(|t| t.kind == crate::lexer::TokenKind::Else)
        .map(|t| t.span.line)
        .collect()
}

/// Sentinel marks the intermediate text carries so that a comment knows where it
/// goes: `\u{1}<line>\u{1}` before the text of a node, `\u{2}<line>\u{2}` right
/// after it when the node fits in one line. `format_program` strips them.
const START_MARK: char = '\u{1}';
const END_MARK: char = '\u{2}';

fn start_marker(line: u32) -> String {
    format!("{START_MARK}{line}{START_MARK}")
}

fn end_marker(line: u32) -> String {
    format!("{END_MARK}{line}{END_MARK}")
}

/// The first well-formed marker of the given kind: byte range and line.
fn find_marker(s: &str, mark: char) -> Option<(usize, usize, u32)> {
    let mut from = 0usize;
    while let Some(rel) = s[from..].find(mark) {
        let start = from + rel;
        let after = start + mark.len_utf8();
        let digits = s[after..]
            .find(|c: char| !c.is_ascii_digit())
            .map_or(s.len() - after, |n| n);
        if digits > 0 && s[after + digits..].starts_with(mark) {
            let line = s[after..after + digits].parse().ok()?;
            return Some((start, after + digits + mark.len_utf8(), line));
        }
        from = after;
    }
    None
}

/// A marker is only ours when it owns the line edge it sits on: the indentation
/// and nothing else before a start marker, nothing else after an end marker. A
/// string literal may hold the sentinel characters too, and those stay put.
fn marker_at_line_edge(s: &str, start: usize, end: usize) -> bool {
    let line_start = s[..start].rfind('\n').map_or(0, |p| p + 1);
    let line_end = s[end..].find('\n').map_or(s.len(), |p| end + p);
    s[line_start..start].chars().all(|c| c == ' ')
        || s[end..line_end].chars().all(|c| c == ' ')
}

fn strip_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        if c == START_MARK || c == END_MARK {
            if let Some((0, end, _)) = find_marker(&s[i..], c) {
                if marker_at_line_edge(s, i, i + end) {
                    i += end;
                    continue;
                }
            }
        }
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Remove the first marker of the given kind from `text`, returning its line.
fn take_marker(text: &mut String, mark: char) -> Option<u32> {
    let mut from = 0usize;
    while let Some((rel, rel_end, line)) = find_marker(&text[from..], mark) {
        let (start, end) = (from + rel, from + rel_end);
        if marker_at_line_edge(text, start, end) {
            text.replace_range(start..end, "");
            return Some(line);
        }
        from = end;
    }
    None
}

/// Put each comment above the node that starts after it, and every trailing
/// comment back at the end of its line. A comment the AST gives no anchor for
/// (between the parameters of a signature, say) still survives: it goes to the
/// end with its original indentation.
fn apply_comments(marked: &str, comments: &[Comment], else_lines: &[u32]) -> String {
    let mut out = String::with_capacity(marked.len());
    let mut next = 0usize;
    let mut next_else = 0usize;
    for line in marked.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let mut text = body.to_string();
        let mut anchored = take_marker(&mut text, START_MARK);
        // An `else` keyword is no node of its own in the AST, so it carries no
        // line marker: take the next one of the source, in emission order.
        let mut else_at = None;
        if anchored.is_none() && text.trim() == "else" {
            let at = else_lines.get(next_else).copied();
            next_else += 1;
            if let Some(at) = at {
                anchored = Some(at);
                else_at = Some(at);
            }
        }
        if let Some(cutoff) = anchored {
            let indent: String = text.chars().take_while(|c| *c == ' ').collect();
            while next < comments.len() && comments[next].line < cutoff {
                out.push_str(&indent);
                out.push_str(&comments[next].text);
                out.push('\n');
                next += 1;
            }
        }
        let line_end = take_marker(&mut text, END_MARK);
        out.push_str(&text);
        if let Some(at) = else_at.or(line_end) {
            while next < comments.len() && comments[next].line == at && comments[next].trailing {
                out.push_str("  ");
                out.push_str(&comments[next].text);
                next += 1;
            }
        }
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    while next < comments.len() {
        out.push_str(&" ".repeat(comments[next].indent));
        out.push_str(&comments[next].text);
        out.push('\n');
        next += 1;
    }
    out
}

fn hide_consumed_inferred_borrows(source: &Program, annotated: &mut Program) {
    // `insert_drops` rewrites the program it is given, spans included; `annotated`
    // keeps the spans of the source because `sal fmt` matches comments by line.
    let mut probe = annotated.clone();
    let Ok(inferred) = crate::infer::infer_program(&probe) else {
        return;
    };
    let env = crate::string_expr::StringEnv::from_program(&probe).with_infer(&inferred);
    let _ = crate::drops::insert_drops(&mut probe, &env.types, &env.structs, &env.enums, &[]);
    let consumed = crate::drops::last_consumed_params();
    for (src_item, ann_item) in source.items.iter().zip(annotated.items.iter_mut()) {
        let (Item::Fn(src), Item::Fn(ann)) = (src_item, ann_item) else {
            continue;
        };
        let Some(flags) = consumed.get(&ann.name) else {
            continue;
        };
        for (i, param) in ann.params.iter_mut().enumerate() {
            let declared = src.params.get(i).map(|p| p.mode).unwrap_or(ParamMode::Inferred);
            if declared == ParamMode::Inferred
                && param.mode == ParamMode::Borrow
                && flags.get(i) == Some(&true)
            {
                param.mode = ParamMode::Inferred;
            }
        }
    }
}

fn format_program_raw(p: &Program, item_starts: Option<&[u32]>) -> String {
    let mut out = String::new();
    let mut first = true;
    for (i, item) in p.items.iter().enumerate() {
        if !first {
            out.push('\n');
        }
        first = false;
        let line = item_starts
            .and_then(|s| s.get(i).copied())
            .unwrap_or_else(|| item_line(item));
        out.push_str(&start_marker(line));
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
            Item::Impl(i) => {
                let claims = format_impl_clause(&i.protocols);
                out.push_str(&format!("impl {} {}\n", format_type(&i.target), claims.trim_start_matches(" impl ")));
            }
            Item::Const(c) => out.push_str(&format_const(c)),
        }
    }
    out
}

fn format_const(c: &ConstDef) -> String {
    let mut out = deprecated_line(&c.deprecated);
    out.push_str(&c.name);
    if let Some(ty) = &c.ty {
        out.push_str(": ");
        out.push_str(&format_type(ty));
    }
    out.push_str(" = ");
    out.push_str(&format_expr(&c.value, 0));
    out.push('\n');
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
        if !p.args.is_empty() {
            out.push('[');
            out.push_str(&p.args.iter().map(format_type).collect::<Vec<_>>().join(", "));
            out.push(']');
        }
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
    // The lifetimes (`struct V['a]`) are kept apart from the type parameters; they go first.
    let params: Vec<&str> = s.lifetimes.iter().chain(&s.type_params).map(String::as_str).collect();
    if !params.is_empty() {
        out.push('[');
        out.push_str(&params.join(", "));
        out.push(']');
    }
    out.push_str(&format_impl_clause(&s.protocols));
    out.push('\n');
    for f in &s.fields {
        out.push_str(&format!(
            "    {}{}: {}{}\n",
            start_marker(f.span.line),
            f.name,
            format_type(&f.ty),
            end_marker(f.span.line)
        ));
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
    out.push_str(&format_impl_clause(&e.protocols));    out.push('\n');
    for v in &e.variants {
        out.push_str("    ");
        out.push_str(&start_marker(v.span.line));
        out.push_str(&v.name);
        if !v.fields.is_empty() {
            out.push('(');
            out.push_str(
                &v
                    .fields
                    .iter()
                    .map(format_type)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push(')');
        }
        out.push_str(&end_marker(v.span.line));
        out.push('\n');
    }
    out
}

fn format_protocol(p: &ProtocolDef) -> String {
    let mut out = format!("protocol {}", p.name);
    if !p.type_params.is_empty() {
        out.push('[');
        out.push_str(&p.type_params.join(", "));
        out.push(']');
    }
    out.push('\n');
    for m in &p.methods {
        out.push_str(&start_marker(m.span.line));
        out.push_str(if m.is_static { "    static " } else { "    method " });
        out.push_str(&m.name);
        if !m.type_params.is_empty() {
            out.push('[');
            out.push_str(&m.type_params.join(", "));
            out.push(']');
        }
        out.push('(');
        if m.is_static {
            // No receiver.
        } else if m.self_ref == 2 {
            out.push_str("&mut self");
        } else if m.self_ref == 1 {
            out.push_str("&self");
        } else {
            out.push_str("self");
            // Mode words are not source syntax (SPEC «Memoria»): `self` is `self`.
        }
        for (i, param) in m.params.iter().enumerate() {
            if i > 0 || !m.is_static {
                out.push_str(", ");
            }
            out.push_str(&param.name);
            out.push_str(": ");
            out.push_str(&format_type(&param.ty));
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
        // The signature is printed on one line, so a comment that trailed it in
        // the source goes back at the end of that line.
        out.push_str(&end_marker(m.span.line));
        out.push('\n');
        if let Some(body) = &m.body {
            out.push_str(&format_block(body, 2));
        }
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
        out.push_str(&start_marker(c.span.line));
        out.push_str(&c.name);
        out.push_str(": ");
        out.push_str(match c.elem {
            ColumnElem::F32 => "F32",
            ColumnElem::F16 => "F16",
            ColumnElem::BF16 => "BF16",
            ColumnElem::I8 => "I8",
            ColumnElem::String => "String",
        });
        out.push_str(&end_marker(c.span.line));
        out.push('\n');
    }
    out
}

fn format_fn(f: &FnDef) -> String {
    let mut s = deprecated_line(&f.deprecated);
    if f.arena {
        s.push_str("@arena\n");
    }
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
    if f.receiver.is_some() && f.self_ref != 0 {
        s.push_str(if f.self_ref == 2 { "&mut self" } else { "&self" });
        wrote = true;
    } else if f.receiver.is_some() && f.self_mode != ParamMode::Inferred {
        s.push_str(match f.self_mode {
            ParamMode::Take => "self take",
            ParamMode::Keep => "self keep",
            ParamMode::Borrow | ParamMode::Inferred => "self borrow",
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
        let line = stmt_span(stmt).line;
        s.push_str(&pad);
        s.push_str(&start_marker(line));
        let text = format_stmt(stmt, indent);
        s.push_str(&text);
        if !text.contains('\n') {
            s.push_str(&end_marker(line));
        }
        s.push('\n');
    }
    if let Some(t) = &b.tail {
        s.push_str(&pad);
        s.push_str(&start_marker(expr_span(t).line));
        s.push_str(&format_expr(t, indent));
        s.push('\n');
    }
    s
}

/// Start of a statement, used to place the comments written above it.
fn stmt_span(st: &Stmt) -> &Span {
    match st {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Break { span }
        | Stmt::InlineFor { span, .. }
        | Stmt::While { span, .. } => span,
        Stmt::Expr(e) => expr_span(e),
    }
}

fn expr_span(e: &Expr) -> &Span {
    match e {
        Expr::Unit { span }
        | Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::Bool { span, .. }
        | Expr::String { span, .. }
        | Expr::Ident { span, .. }
        | Expr::Call { span, .. }
        | Expr::Lambda { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Unary { span, .. }
        | Expr::On { span, .. }
        | Expr::To { span, .. }
        | Expr::TensorLit { span, .. }
        | Expr::Field { span, .. }
        | Expr::Match { span, .. }
        | Expr::If { span, .. }
        | Expr::Try { span, .. } => span,
        Expr::Block(b) => &b.span,
    }
}

fn item_line(item: &Item) -> u32 {
    match item {
        Item::Fn(f) => f.span.line,
        Item::Import(i) => i.span.line,
        Item::Struct(s) => s.span.line,
        Item::Enum(e) => e.span.line,
        Item::Frame(f) => f.span.line,
        Item::Protocol(p) => p.span.line,
        Item::Impl(i) => i.span.line,
        Item::Const(c) => c.span.line,
    }
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
        Stmt::InlineFor { var, ty, body, .. } => {
            let mut s = format!("inline for {var} in {ty}.fields\n");
            s.push_str(&format_block(body, indent + 1));
            if s.ends_with('\n') {
                s.pop();
            }
            s
        }
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
        Expr::Unit { .. } => "()".to_string(),
        Expr::String { value, parts, .. } => format_string_literal(value, parts),
        Expr::Ident { name, .. } => name.clone(),
        Expr::Lambda { params, body, .. } => {
            let p = if params.len() == 1 { params[0].clone() } else { format!("({})", params.join(", ")) };
            let body_s = format_expr(body, indent);
            if body_s.starts_with('\n') { format!("{} =>{}", p, body_s) }
            else { format!("{} => {}", p, body_s) }
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
            let trailing = args.last().is_some_and(|a| matches!(a, Expr::Lambda { .. }));
            let ordinary_len = args.len() - usize::from(trailing);
            if !(trailing && ordinary_len == 0 && matches!(&**func, Expr::Field { .. })) {
                s.push('(');
                for (i, a) in args[..ordinary_len].iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&format_expr(a, indent));
                }
                s.push(')');
            }
            if trailing {
                s.push(' ');
                s.push_str(&format_expr(args.last().unwrap(), indent));
            }
            s
        }
        Expr::Binary { op, left, right, .. } => {
            let p = binop_prec(op);
            // Left associative: the right operand needs parentheses at equal precedence.
            let l = format_operand(left, p, indent);
            let r = format_tail_operand(right, p + 1, indent);
            format!("{} {} {}", l, format_binop(op), r)
        }
        Expr::Unary { op, expr, .. } => {
            format!("{}{}", format_unop(op), format_tail_operand(expr, ATOM_PREC, indent))
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
                s.push_str(&start_marker(arm.span.line));
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
                s.push_str(&start_marker(arm.span.line));
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
        UnOp::Ref => "&",
        UnOp::RefMut => "&mut ",
        UnOp::Deref => "*",
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

/// The canonical text of a type, as `sal fmt` writes it (`Dict[String, Int]`).
pub fn type_text(t: &Type) -> String {
    format_type(t)
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
        Type::Ref {
            mutable,
            lifetime,
            inner,
            ..
        } => {
            let mut s = "&".to_string();
            if let Some(lt) = lifetime {
                s.push('\'');
                s.push_str(lt);
                s.push(' ');
            }
            if *mutable {
                s.push_str("mut ");
            }
            s.push_str(&format_type(inner));
            s
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

/// The last operand of an operator or prefix (`a + match x`, `-if c`). A `match`, `if`, `on`
/// or block written over several lines runs to the end of the statement and closes the
/// expression by itself; inside parentheses the parser would not see its lines, so it stays
/// bare. A one-line form keeps the parentheses that `format_operand` gives it.
fn format_tail_operand(e: &Expr, min: u8, indent: usize) -> String {
    let s = format_expr(e, indent);
    let block = matches!(
        e,
        Expr::Match { .. } | Expr::If { .. } | Expr::On { .. } | Expr::Block(_)
    );
    if expr_prec(e) < min && !(block && s.contains('\n')) {
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
