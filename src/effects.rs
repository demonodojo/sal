use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode, DiagResult};
use crate::infer::InferOutput;
use crate::string_expr::StringEnv;

/// Effects with the types `infer_program` gives this program on its own.
pub fn check_effects(prog: &Program) -> DiagResult<()> {
    let callees = callee_effects(&[prog]);
    check_effects_typed(prog, &StringEnv::from_program_inferred(prog), callees)
}

/// Effects with the types infer already produced (module graphs included).
pub fn check_effects_with_infer(prog: &Program, out: &InferOutput) -> DiagResult<()> {
    let callees = callee_effects(&[prog]);
    check_effects_typed(prog, &StringEnv::from_program(prog).with_infer(out), callees)
}

/// Same, with the declared effects of every function of `programs` (the whole
/// module graph) so a call to an imported function counts too.
pub fn check_effects_with_graph(
    prog: &Program,
    out: &InferOutput,
    programs: &[&Program],
) -> DiagResult<()> {
    let callees = callee_effects(programs);
    check_effects_typed(prog, &StringEnv::from_program(prog).with_infer(out), callees)
}

type CalleeEffects = Rc<HashMap<String, Vec<Effect>>>;

fn callee_effects(programs: &[&Program]) -> CalleeEffects {
    let mut table = HashMap::new();
    for p in programs {
        for item in &p.items {
            if let Item::Fn(f) = item {
                table.entry(f.name.clone()).or_insert_with(|| f.effects.clone());
            }
        }
    }
    Rc::new(table)
}

fn check_effects_typed(prog: &Program, env: &StringEnv, callees: CalleeEffects) -> DiagResult<()> {
    for item in &prog.items {
        if let Item::Fn(f) = item {
            check_fn_effects(f, env, &callees)?;
        }
    }
    Ok(())
}

/// Whether the function body performs an allocation (for `@no_heap`).
pub fn fn_body_uses_alloc(f: &FnDef, env: &StringEnv) -> bool {
    let mut used = EffectsUsed::default();
    let mut strings = env.clone();
    scan_block(&f.body, &mut used, &mut strings);
    used.alloc
}

/// Whether evaluating `e` allocates (for `@stack`).
pub fn expr_uses_alloc(e: &Expr, env: &StringEnv) -> bool {
    let mut used = EffectsUsed::default();
    let mut strings = env.clone();
    scan_expr(e, &mut used, &mut strings);
    used.alloc
}

fn check_fn_effects(f: &FnDef, env: &StringEnv, callees: &CalleeEffects) -> DiagResult<()> {
    let mut used = EffectsUsed::default();
    used.callees = callees.clone();
    let mut strings = env.clone();
    scan_block(&f.body, &mut used, &mut strings);
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
    /// Declared effects of user functions: calling one performs them too.
    callees: CalleeEffects,
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

fn scan_block(b: &Block, used: &mut EffectsUsed, strings: &mut StringEnv) {
    for st in &b.stmts {
        scan_stmt(st, used, strings);
    }
    if let Some(t) = &b.tail {
        scan_expr(t, used, strings);
    }
}

fn scan_stmt(st: &Stmt, used: &mut EffectsUsed, strings: &mut StringEnv) {
    match st {
        Stmt::Break { .. } => {}
        Stmt::Let { name, ty, init, .. } => {
            scan_expr(init, used, strings);
            strings.note_init(name, ty.as_ref(), init);
        }
        Stmt::Expr(e) => scan_expr(e, used, strings),
        Stmt::Assign { target, value, .. } => {
            scan_expr(value, used, strings);
            if let Expr::Ident { name, .. } = target {
                strings.note_init(name, None, value);
            }
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                scan_expr(v, used, strings);
            }
        }
        Stmt::While { cond, body, .. } => {
            scan_expr(cond, used, strings);
            scan_block(body, used, strings);
        }
    }
}

fn scan_expr(e: &Expr, used: &mut EffectsUsed, strings: &mut StringEnv) {
    match e {
        Expr::Call { func, args, .. } => {
            if let Expr::Ident { name, .. } = func.as_ref() {
                if let Some(effs) = used.callees.get(name.as_str()).cloned() {
                    for e in effs {
                        match e {
                            Effect::Io => used.io = true,
                            Effect::Alloc => used.alloc = true,
                            Effect::Panic => used.panic = true,
                            Effect::Gpu => used.gpu = true,
                            Effect::Tpu => used.tpu = true,
                        }
                    }
                }
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
                    | "int_to_str" | "char_to_str" | "vec_new" | "image_new" | "list_new" | "dict_new" | "dict_keys" | "dict_values"
                    | "bytes_new" | "bytes_to_str" | "bytes_view" | "bytes_view_mem" | "tmp_path"
                    | "exec_capture" | "getenv" | "realpath" => {
                        used.alloc = true;
                    }
                    "syscall" => used.io = true,
                    "panic" => used.panic = true,
                    _ => {}
                }
            }
            scan_expr(func, used, strings);
            for a in args {
                scan_expr(a, used, strings);
            }
        }
        Expr::Lambda { body, .. } => {
            // Lambda is not an effectful call; only scan the body.
            scan_expr(body, used, strings);
        }
        Expr::On { place, body, .. } => {
            mark_place_effect(place, used);
            let mut local = strings.clone();
            scan_block(body, used, &mut local);
        }
        Expr::To { place, expr, .. } => {
            mark_place_effect(place, used);
            scan_expr(expr, used, strings);
        }
        Expr::Binary { left, right, .. } => {
            scan_expr(left, used, strings);
            scan_expr(right, used, strings);
            if strings.add_is_string_concat(e) {
                used.alloc = true;
            }
        }
        Expr::Unary { expr, .. } => scan_expr(expr, used, strings),
        Expr::Block(b) => scan_block(b, used, strings),
        Expr::TensorLit { rows, .. } => {
            used.alloc = true;
            for row in rows {
                for c in row {
                    scan_expr(c, used, strings);
                }
            }
        }
        Expr::Field { base, .. } => scan_expr(base, used, strings),
        Expr::Match { scrutinee, arms, .. } => {
            scan_expr(scrutinee, used, strings);
            for arm in arms {
                let mut local = strings.clone();
                scan_expr(&arm.body, used, &mut local);
            }
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            ..
        } => {
            scan_expr(cond, used, strings);
            let mut then_strings = strings.clone();
            scan_block(then_block, used, &mut then_strings);
            for arm in elsifs {
                scan_expr(&arm.cond, used, strings);
                let mut arm_strings = strings.clone();
                scan_block(&arm.body, used, &mut arm_strings);
            }
            if let Some(else_block) = else_block {
                let mut else_strings = strings.clone();
                scan_block(else_block, used, &mut else_strings);
            }
        }
        Expr::Try { expr, .. } => scan_expr(expr, used, strings),
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
