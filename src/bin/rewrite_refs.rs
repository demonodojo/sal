//! One-shot source rewrite: inferred `borrow` becomes `&T`, `self borrow` becomes `&self`,
//! and a call argument in a borrow position is written `&arg`. Deleted after the migration.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::Path;

use sal_compiler::ast::*;
use sal_compiler::ownership::infer_param_modes;
use sal_compiler::parser::parse;

fn main() {
    let mut files = Vec::new();
    for arg in env::args().skip(1) {
        collect(&arg, &mut files);
    }
    let mut modes: HashMap<String, Vec<bool>> = HashMap::new();
    let mut ambiguous = std::collections::HashSet::new();
    let mut parsed: Vec<(String, String, Program)> = Vec::new();
    for path in &files {
        let Ok(src) = fs::read_to_string(path) else { continue };
        let prog = match parse(&src) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("parse {}: {}", path, e.message);
                continue;
            }
        };
        let ann = infer_param_modes(&prog);
        for item in &ann.items {
            if let Item::Fn(f) = item {
                if f.params.is_empty() {
                    continue;
                }
                let ms: Vec<bool> = f.params.iter().map(|p| matches!(p.ty, Type::Ref { .. })).collect();
                let key = match &f.receiver {
                    Some(recv) => format!("{recv}__{}", f.name),
                    None => f.name.clone(),
                };
                if modes.contains_key(&key) {
                    ambiguous.insert(key);
                } else {
                    modes.insert(key, ms);
                }
            }
        }
        parsed.push((path.clone(), src, ann));
    }
    for name in &ambiguous {
        modes.remove(name);
    }
    let mut changed = 0;
    for (path, src, ann) in &parsed {
        let next = rewrite(src, ann, &modes);
        if next != *src {
            fs::write(path, &next).expect("write");
            changed += 1;
            println!("{path}");
        }
    }
    eprintln!("{changed} files");
}

fn collect(arg: &str, out: &mut Vec<String>) {
    let path = Path::new(arg);
    if path.is_file() {
        if arg.ends_with(".sal") && !arg.contains("/fixtures/") {
            out.push(arg.to_string());
        }
        return;
    }
    let Ok(rd) = fs::read_dir(path) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "fixtures" || n == "target") {
                continue;
            }
            collect(&p.to_string_lossy(), out);
        } else if p.extension().is_some_and(|e| e == "sal") {
            collect(&p.to_string_lossy(), out);
        }
    }
}

struct Edit {
    start: usize,
    end: usize,
    text: String,
}

fn rewrite(src: &str, prog: &Program, global: &HashMap<String, Vec<bool>>) -> String {
    // A name defined in this file wins over the global map, even when the same
    // name exists in another file and was dropped as ambiguous.
    let mut owned = global.clone();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            let key = match &f.receiver {
                Some(recv) => format!("{recv}__{}", f.name),
                None => f.name.clone(),
            };
            let ms: Vec<bool> = f.params.iter().map(|p| matches!(p.ty, Type::Ref { .. })).collect();
            owned.insert(key, ms);
        }
    }
    let modes = &owned;
    let mut edits = Vec::new();
    for item in &prog.items {
        match item {
            Item::Fn(f) => {
                // Signatures are already migrated. A second pass must not turn a
                // by-value parameter into `&T` just because the old mode inference
                // called it a borrow: some tests move that value on purpose.
                let refs = ref_params(f);
                rewrite_block(&f.body, &f.name, modes, &refs, &mut edits);
            }
            Item::Protocol(p) => {
                for m in &p.methods {
                    let (hs, he) = header_range(src, m.span.end as usize);
                    rewrite_self(src, hs, he, m.self_mode, m.self_ref, &mut edits);
                    for param in &m.params {
                        rewrite_param(src, param, &mut edits);
                    }
                }
            }
            _ => {}
        }
    }
    apply(src, edits)
}

fn header_range(src: &str, body_start: usize) -> (usize, usize) {
    let bytes = src.as_bytes();
    let mut i = body_start.min(bytes.len());
    let mut begin = 0;
    while i > 0 {
        let mut line = i;
        while line > 0 && bytes[line - 1] != b'\n' {
            line -= 1;
        }
        let trim = src[line..i].trim_start();
        if trim.starts_with("fn ")
            || trim.starts_with("method ")
            || trim.starts_with("static ")
            || trim.starts_with("init ")
        {
            begin = line;
            break;
        }
        if line == 0 {
            break;
        }
        i = line - 1;
    }
    (begin, body_start.min(src.len()))
}

fn rewrite_self(src: &str, start: usize, end: usize, mode: ParamMode, self_ref: u8, edits: &mut Vec<Edit>) {
    if self_ref != 0 {
        return;
    }
    if start >= src.len() || end > src.len() || start >= end {
        return;
    }
    let header = &src[start..end];
    let (needle, repl) = match mode {
        ParamMode::Borrow => ("self borrow", "&self"),
        ParamMode::Take => ("self take", "self"),
        _ => return,
    };
    if let Some(i) = header.find(needle) {
        edits.push(Edit {
            start: start + i,
            end: start + i + needle.len(),
            text: repl.to_string(),
        });
    }
}

fn rewrite_param(src: &str, p: &Param, edits: &mut Vec<Edit>) {
    if p.name == "self" {
        return;
    }
    match p.mode {
        ParamMode::Borrow => {
            if !matches!(p.ty, Type::Ref { .. }) {
                edits.push(Edit {
                    start: p.ty.span().start as usize,
                    end: p.ty.span().start as usize,
                    text: "&".to_string(),
                });
            }
            if let Some((a, b)) = mode_word_in_param(src, p.span.end as usize) {
                edits.push(Edit { start: a, end: b, text: String::new() });
            }
        }
        ParamMode::Take => {
            if let Some((a, b)) = mode_word_in_param(src, p.span.end as usize) {
                edits.push(Edit { start: a, end: b, text: String::new() });
            }
        }
        ParamMode::Keep | ParamMode::Inferred => {}
    }
}

/// The `borrow` / `take` / `keep` word of one parameter, after its name and before `,` or `)`.
fn mode_word_in_param(src: &str, name_end: usize) -> Option<(usize, usize)> {
    let b = src.as_bytes();
    if name_end > b.len() {
        return None;
    }
    let mut i = name_end;
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'[' | b'(' => depth += 1,
            b']' | b')' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b'\n' if depth == 0 => break,
            b',' if depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    mode_word_before(src, name_end, i)
}

fn mode_word_before(src: &str, from: usize, to: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < to {
        if let Some((a, b)) = mode_word(src, i) {
            if b <= to {
                return Some((a, b));
            }
        }
        i += 1;
    }
    None
}

fn mode_word(src: &str, at: usize) -> Option<(usize, usize)> {
    let b = src.as_bytes();
    if at > b.len() {
        return None;
    }
    let mut i = at;
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    for w in ["borrow", "take", "keep"] {
        if src[i..].starts_with(w) {
            let end = i + w.len();
            let boundary = end == src.len()
                || !src[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if boundary {
                let start = if i > at && b[i - 1] == b' ' { i - 1 } else { i };
                return Some((start, end));
            }
        }
    }
    None
}

fn ref_params(f: &FnDef) -> HashSet<String> {
    f.params
        .iter()
        .filter(|p| matches!(p.ty, Type::Ref { .. }))
        .map(|p| p.name.clone())
        .collect()
}

fn rewrite_block(
    b: &Block,
    fn_name: &str,
    modes: &HashMap<String, Vec<bool>>,
    refs: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    for st in &b.stmts {
        rewrite_stmt(st, fn_name, modes, refs, edits);
    }
    if let Some(t) = &b.tail {
        rewrite_expr(t, fn_name, modes, refs, edits);
    }
}

fn rewrite_stmt(
    st: &Stmt,
    fn_name: &str,
    modes: &HashMap<String, Vec<bool>>,
    refs: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    match st {
        Stmt::Let { init, .. } => rewrite_expr(init, fn_name, modes, refs, edits),
        Stmt::Assign { value, .. } => rewrite_expr(value, fn_name, modes, refs, edits),
        Stmt::Expr(e) => rewrite_expr(e, fn_name, modes, refs, edits),
        Stmt::Return { value: Some(e), .. } => rewrite_expr(e, fn_name, modes, refs, edits),
        Stmt::While { cond, body, .. } => {
            rewrite_expr(cond, fn_name, modes, refs, edits);
            rewrite_block(body, fn_name, modes, refs, edits);
        }
        Stmt::Return { value: None, .. } | Stmt::Break { .. } => {}
    }
}

fn rewrite_expr(
    e: &Expr,
    fn_name: &str,
    modes: &HashMap<String, Vec<bool>>,
    refs: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    match e {
        Expr::Call { func, args, .. } => {
            if let Some(ms) = call_modes(func, modes) {
                for (i, arg) in args.iter().enumerate() {
                    if ms.get(i) == Some(&false) {
                        if let Expr::Unary { op: UnOp::Ref, span, .. } = arg {
                            let at = span.start as usize;
                            edits.push(Edit { start: at, end: at + 1, text: String::new() });
                        }
                        continue;
                    }
                    if ms.get(i) != Some(&true) {
                        continue;
                    }
                    if let Some(at) = extra_ref_on_param(arg, refs) {
                        edits.push(Edit { start: at, end: at + 1, text: String::new() });
                        continue;
                    }
                    if can_prefix_ref(arg) && !ident_is_ref(arg, refs) {
                        let at = expr_head(arg) as usize;
                        edits.push(Edit { start: at, end: at, text: "&".to_string() });
                    }
                }
            }
            rewrite_expr(func, fn_name, modes, refs, edits);
            for a in args {
                rewrite_expr(a, fn_name, modes, refs, edits);
            }
        }
        Expr::Binary { left, right, .. } => {
            rewrite_expr(left, fn_name, modes, refs, edits);
            rewrite_expr(right, fn_name, modes, refs, edits);
        }
        Expr::Unary { expr, .. } => rewrite_expr(expr, fn_name, modes, refs, edits),
        Expr::Field { base, .. } => rewrite_expr(base, fn_name, modes, refs, edits),
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            rewrite_expr(cond, fn_name, modes, refs, edits);
            rewrite_block(then_block, fn_name, modes, refs, edits);
            for elsif in elsifs {
                rewrite_expr(&elsif.cond, fn_name, modes, refs, edits);
                rewrite_block(&elsif.body, fn_name, modes, refs, edits);
            }
            if let Some(b) = else_block {
                rewrite_block(b, fn_name, modes, refs, edits);
            }
        }
        Expr::Block(b) | Expr::On { body: b, .. } => rewrite_block(b, fn_name, modes, refs, edits),
        Expr::Match { scrutinee, arms, .. } => {
            rewrite_expr(scrutinee, fn_name, modes, refs, edits);
            for arm in arms {
                rewrite_expr(&arm.body, fn_name, modes, refs, edits);
            }
        }
        Expr::Try { expr, .. } | Expr::To { expr, .. } => rewrite_expr(expr, fn_name, modes, refs, edits),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    rewrite_expr(c, fn_name, modes, refs, edits);
                }
            }
        }
        Expr::Lambda { body, .. } => rewrite_expr(body, fn_name, modes, refs, edits),
        _ => {}
    }
}

fn call_modes(func: &Expr, modes: &HashMap<String, Vec<bool>>) -> Option<Vec<bool>> {
    let key = match func {
        Expr::Ident { name, .. } => name.clone(),
        Expr::Field { base, field, .. } => {
            let Expr::Ident { name, .. } = base.as_ref() else {
                return None;
            };
            if let Some(ms) = modes.get(&format!("{name}__{field}")) {
                return Some(ms.clone());
            }
            // `self.m(args)`: the receiver type is not in the call. Use the method
            // when its name is unique.
            if name == "self" {
                let suffix = format!("__{field}");
                let mut found: Option<Vec<bool>> = None;
                for (k, v) in modes {
                    if k.ends_with(&suffix) {
                        if found.is_some() {
                            return None;
                        }
                        found = Some(v.clone());
                    }
                }
                return found;
            }
            return None;
        }
        _ => return None,
    };
    modes.get(&key).cloned()
}

/// `&name` where `name` is already a reference parameter: the extra `&` is removed.
fn extra_ref_on_param(arg: &Expr, refs: &HashSet<String>) -> Option<usize> {
    let Expr::Unary { op: UnOp::Ref, expr, span } = arg else {
        return None;
    };
    let Expr::Ident { name, .. } = expr.as_ref() else {
        return None;
    };
    if refs.contains(name) {
        Some(span.start as usize)
    } else {
        None
    }
}

fn ident_is_ref(arg: &Expr, refs: &HashSet<String>) -> bool {
    match arg {
        Expr::Ident { name, .. } => refs.contains(name),
        _ => false,
    }
}

/// `&` before an ident, a field chain, a call of one of those, or a literal.
/// A binary or a parenthesized call needs parentheses, so it is left alone.
fn can_prefix_ref(e: &Expr) -> bool {
    match e {
        Expr::Ident { .. } | Expr::String { .. } => true,
        Expr::Field { base, .. } => simple_head(base),
        Expr::Call { func, .. } => match func.as_ref() {
            Expr::Ident { .. } => true,
            Expr::Field { base, .. } => simple_head(base),
            _ => false,
        },
        _ => false,
    }
}

fn simple_head(e: &Expr) -> bool {
    match e {
        Expr::Ident { .. } | Expr::String { .. } => true,
        Expr::Field { base, .. } => simple_head(base),
        Expr::Call { func, .. } => match func.as_ref() {
            Expr::Ident { .. } => true,
            Expr::Field { base, .. } => simple_head(base),
            _ => false,
        },
        _ => false,
    }
}

fn expr_start(e: &Expr) -> u32 {
    match e {
        Expr::Binary { left, .. }
        | Expr::Field { base: left, .. }
        | Expr::Unary { expr: left, .. }
        | Expr::Call { func: left, .. } => expr_start(left),
        other => other.span().start,
    }
}

fn expr_end(e: &Expr) -> u32 {
    match e {
        Expr::Binary { right, .. } => expr_end(right),
        Expr::Unary { expr, .. } => expr_end(expr),
        other => other.span().end,
    }
}

/// First source byte of an expression. Field and call spans point at a later token.
fn expr_head(e: &Expr) -> u32 {
    match e {
        Expr::Field { base, .. } | Expr::Unary { expr: base, .. } | Expr::Call { func: base, .. } => {
            expr_head(base)
        }
        other => other.span().start,
    }
}

fn apply(src: &str, mut edits: Vec<Edit>) -> String {
    edits.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
    let mut out = src.to_string();
    let mut last = usize::MAX;
    for e in edits {
        if e.start > out.len() || e.end > out.len() || e.start > e.end {
            continue;
        }
        if e.end > last {
            continue;
        }
        last = e.start;
        out.replace_range(e.start..e.end, &e.text);
    }
    out
}
