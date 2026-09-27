use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::string_expr::binary_add_is_string_concat;

pub fn check_effects(prog: &Program) -> DiagResult<()> {
    for item in &prog.items {
        if let Item::Fn(f) = item {
            check_fn_effects(f)?;
        }
    }
    Ok(())
}

fn check_fn_effects(f: &FnDef) -> DiagResult<()> {
    let mut used = EffectsUsed::default();
    scan_block(&f.body, &mut used);
    for need in used.into_list() {
        if !f.effects.contains(&need) {
            return Err(vec![Diagnostic::new(
                ErrorCode::EEffect,
                format!(
                    "function `{}` uses effect `{}` but signature does not declare it",
                    f.name,
                    effect_name(&need)
                ),
                f.span,
            )
            .with_hint(format!("add `! {}` to the signature", effect_name(&need)))]);
        }
    }
    Ok(())
}

#[derive(Default)]
struct EffectsUsed {
    io: bool,
    alloc: bool,
    panic: bool,
    gpu: bool,
    tpu: bool,
}

impl EffectsUsed {
    fn into_list(self) -> Vec<Effect> {
        let mut v = Vec::new();
        if self.io {
            v.push(Effect::Io);
        }
        if self.alloc {
            v.push(Effect::Alloc);
        }
        if self.panic {
            v.push(Effect::Panic);
        }
        if self.gpu {
            v.push(Effect::Gpu);
        }
        if self.tpu {
            v.push(Effect::Tpu);
        }
        v
    }
}

fn scan_block(b: &Block, used: &mut EffectsUsed) {
    for st in &b.stmts {
        scan_stmt(st, used);
    }
    if let Some(t) = &b.tail {
        scan_expr(t, used);
    }
}

fn scan_stmt(st: &Stmt, used: &mut EffectsUsed) {
    match st {
        Stmt::Let { init, .. } => scan_expr(init, used),
        Stmt::Expr(e) => scan_expr(e, used),
        Stmt::Assign { value, .. } => scan_expr(value, used),
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                scan_expr(v, used);
            }
        }
        Stmt::While { cond, body, .. } => {
            scan_expr(cond, used);
            scan_block(body, used);
        }
    }
}

fn scan_expr(e: &Expr, used: &mut EffectsUsed) {
    match e {
        Expr::Call { func, args, .. } => {
            if let Expr::Ident { name, .. } = func.as_ref() {
                match name.as_str() {
                    "load" | "read_file" | "path_readable" => {
                        used.io = true;
                        used.alloc = true;
                    }
                    "write_file" | "write_png" | "print_str" | "eprint_str" | "copy_file" | "copy_self"
                    | "gated_print_str" | "gated_copy_self" | "clang" | "clang_obj" | "link_objs"
                    | "exec_compile" | "gated_exec_compile" | "mkdir_p" => {
                        used.io = true;
                    }
                    "str_concat" | "str_append" | "strdup" | "select_str" | "argv" | "str_slice"
                    | "int_to_str" | "char_to_str" | "vec_new" | "image_new" | "list_new" | "dict_new"
                    | "tmp_path"
                    | "exec_capture" | "getenv" | "realpath" => {
                        used.alloc = true;
                    }
                    "panic" => used.panic = true,
                    _ => {}
                }
            }
            scan_expr(func, used);
            for a in args {
                scan_expr(a, used);
            }
        }
        Expr::Lambda { body, .. } => {
            // Lambda is not an effectful call; only scan the body.
            scan_expr(body, used);
        }
        Expr::On { place, body, .. } => {
            mark_place_effect(place, used);
            scan_block(body, used);
        }
        Expr::To { place, expr, .. } => {
            mark_place_effect(place, used);
            scan_expr(expr, used);
        }
        Expr::Binary { left, right, .. } => {
            scan_expr(left, used);
            scan_expr(right, used);
            if binary_add_is_string_concat(e) {
                used.alloc = true;
            }
        }
        Expr::Unary { expr, .. } => scan_expr(expr, used),
        Expr::Block(b) => scan_block(b, used),
        Expr::TensorLit { rows, .. } => {
            used.alloc = true;
            for row in rows {
                for c in row {
                    scan_expr(c, used);
                }
            }
        }
        Expr::Field { base, .. } => scan_expr(base, used),
        Expr::Match { scrutinee, arms, .. } => {
            scan_expr(scrutinee, used);
            for arm in arms {
                scan_expr(&arm.body, used);
            }
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            ..
        } => {
            scan_expr(cond, used);
            scan_block(then_block, used);
            for arm in elsifs {
                scan_expr(&arm.cond, used);
                scan_block(&arm.body, used);
            }
            if let Some(else_block) = else_block {
                scan_block(else_block, used);
            }
        }
        Expr::Try { expr, .. } => scan_expr(expr, used),
        _ => {}
    }
}

fn mark_place_effect(place: &Place, used: &mut EffectsUsed) {
    match place {
        Place::Gpu => used.gpu = true,
        Place::Tpu => used.tpu = true,
        Place::Cpu | Place::Param(_) => {}
    }
}

fn effect_name(e: &Effect) -> &'static str {
    match e {
        Effect::Io => "io",
        Effect::Alloc => "alloc",
        Effect::Panic => "panic",
        Effect::Gpu => "gpu",
        Effect::Tpu => "tpu",
    }
}
