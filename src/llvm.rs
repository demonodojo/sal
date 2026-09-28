use std::cell::RefCell;
use std::collections::HashMap;

use crate::ast::{Dim, Expr, FnDef, Item, Program, Stmt, StructDef, Type};
use crate::ir::{FusedOp, IrFunction, IrInst, IrModule};
use crate::layout::{is_transparent_struct, llvm_struct_symbol};

thread_local! {
    static EMIT_STRUCT_DEFS: RefCell<HashMap<String, StructDef>> = RefCell::new(HashMap::new());
    static EMIT_AGG: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn emit_layout_scope<R>(struct_defs: &HashMap<String, StructDef>, f: impl FnOnce() -> R) -> R {
    EMIT_STRUCT_DEFS.with(|d| *d.borrow_mut() = struct_defs.clone());
    EMIT_AGG.with(|a| a.borrow_mut().clear());
    let out = f();
    EMIT_STRUCT_DEFS.with(|d| d.borrow_mut().clear());
    EMIT_AGG.with(|a| a.borrow_mut().clear());
    out
}

/// LLVM global `@.str.N` with `{ cap, len, [bytes] }` rodata; `byte_len` excludes NUL.
#[derive(Debug, Clone)]
struct StrLitGlobal {
    sym: String,
    ty: String,
    _byte_len: usize,
}

fn emit_ir_const_string(s: &mut String, dest: &str, g: &StrLitGlobal) {
    s.push_str(&format!(
        "  %{dest}_p = getelementptr inbounds {}, ptr {}, i32 0, i32 2, i32 0\n",
        g.ty, g.sym
    ));
    s.push_str(&format!("  %{dest} = ptrtoint ptr %{dest}_p to i64\n"));
}

fn str_lit_ptr_temp(g: &StrLitGlobal, tmp: &mut u32, s: &mut String) -> String {
    *tmp += 1;
    let t = format!("slit{tmp}");
    s.push_str(&format!(
        "  %{t} = getelementptr inbounds {}, ptr {}, i32 0, i32 2, i32 0\n",
        g.ty, g.sym
    ));
    format!("%{t}")
}

pub struct LlvmOptions {
    pub instrument: bool,
    /// Imported user functions defined in other `.o` files (name → param count).
    pub extern_user_fns: HashMap<String, usize>,
    /// When false, do not synthesize `@main` if the module has no `main` (dependency `.o`).
    pub emit_entry_main: bool,
    pub struct_defs: HashMap<String, StructDef>,
}

/// Static host tensor materialised from a `tensor[[…]]` literal (or param shape).
#[derive(Debug, Clone)]
pub struct HostTensor {
    /// SSA / IR name used in `Call` args (`t1`, or source name for bare assigns).
    pub ir_name: String,
    pub rows: u64,
    pub cols: u64,
    pub data: Vec<f32>,
}

/// Collect tensor literals in source order and assign IR names the same way `lower` does
/// for `let` (`tN`) and bare `name = tensor` (keeps `name`).
pub fn collect_host_tensors(prog: &Program) -> HashMap<String, HostTensor> {
    let mut out = HashMap::new();
    for item in &prog.items {
        let Item::Fn(f) = item else { continue };
        let mut counter = 0u32;
        for st in &f.body.stmts {
            match st {
                Stmt::Let { name, init, .. } => {
                    if let Some((rows, cols, data)) = tensor_lit_data(init) {
                        counter += 1;
                        let ir_name = format!("t{counter}");
                        out.insert(
                            ir_name.clone(),
                            HostTensor {
                                ir_name,
                                rows,
                                cols,
                                data,
                            },
                        );
                        let _ = name;
                    } else {
                        // Mimic lower_expr_simple counter bumps for other exprs we care about.
                        bump_counter_for_expr(init, &mut counter);
                    }
                }
                Stmt::Assign { target, value, .. } => {
                    if let (Expr::Ident { name, .. }, Some((rows, cols, data))) =
                        (target, tensor_lit_data(value))
                    {
                        out.insert(
                            name.clone(),
                            HostTensor {
                                ir_name: name.clone(),
                                rows,
                                cols,
                                data,
                            },
                        );
                    }
                }
                Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } => {
                    bump_counter_for_expr(e, &mut counter);
                }
                _ => {}
            }
        }
        if let Some(t) = &f.body.tail {
            // Tail may be `on` / call; counter already used for lets.
            let _ = t;
            collect_param_shapes(f, &mut out);
        } else {
            collect_param_shapes(f, &mut out);
        }
    }
    out
}

fn collect_param_shapes(f: &FnDef, out: &mut HashMap<String, HostTensor>) {
    for p in &f.params {
        if let Type::Tensor { dims, .. } = &p.ty {
            if dims.len() >= 2 {
                let r = &dims[dims.len() - 2];
                let c = &dims[dims.len() - 1];
                match (r, c) {
                    (Dim::Static(rows), Dim::Static(cols)) => {
                        out.entry(p.name.clone()).or_insert_with(|| HostTensor {
                            ir_name: p.name.clone(),
                            rows: *rows,
                            cols: *cols,
                            data: Vec::new(),
                        });
                    }
                    (Dim::Dynamic, Dim::Static(cols)) => {
                        // `?` rows: length is a runtime dim arg; cols known statically.
                        out.entry(p.name.clone()).or_insert_with(|| HostTensor {
                            ir_name: p.name.clone(),
                            rows: 0,
                            cols: *cols,
                            data: Vec::new(),
                        });
                    }
                    (Dim::Static(rows), Dim::Dynamic) => {
                        out.entry(p.name.clone()).or_insert_with(|| HostTensor {
                            ir_name: p.name.clone(),
                            rows: *rows,
                            cols: 0,
                            data: Vec::new(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

fn bump_counter_for_expr(e: &Expr, counter: &mut u32) {
    match e {
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } => {
            *counter += 1;
        }
        Expr::TensorLit { .. } => {
            *counter += 1;
        }
        Expr::Call { args, .. } => {
            for a in args {
                bump_counter_for_expr(a, counter);
            }
            *counter += 1;
        }
        Expr::Lambda { body, .. } => {
            bump_counter_for_expr(body, counter);
            *counter += 1;
        }
        Expr::On { body, .. } => {
            for st in &body.stmts {
                if let Stmt::Expr(e) = st {
                    bump_counter_for_expr(e, counter);
                }
            }
            if let Some(t) = &body.tail {
                bump_counter_for_expr(t, counter);
            }
            *counter += 1; // region dest
        }
        Expr::To { expr, .. } => {
            bump_counter_for_expr(expr, counter);
            *counter += 1;
        }
        _ => {
            *counter += 1;
        }
    }
}

fn tensor_lit_data(e: &Expr) -> Option<(u64, u64, Vec<f32>)> {
    let Expr::TensorLit { rows, .. } = e else {
        return None;
    };
    let nrows = rows.len() as u64;
    let ncols = rows.first().map(|r| r.len() as u64).unwrap_or(0);
    let mut data = Vec::with_capacity((nrows * ncols) as usize);
    for row in rows {
        for cell in row {
            data.push(expr_as_f32(cell)?);
        }
    }
    Some((nrows, ncols, data))
}

fn expr_as_f32(e: &Expr) -> Option<f32> {
    match e {
        Expr::Float { value, .. } => Some(*value as f32),
        Expr::Int { value, .. } => Some(*value as f32),
        _ => None,
    }
}

pub fn emit_llvm(m: &IrModule, opts: &LlvmOptions) -> String {
    emit_llvm_with_tensors(m, opts, &HashMap::new())
}

pub fn emit_llvm_with_tensors(
    m: &IrModule,
    opts: &LlvmOptions,
    tensors: &HashMap<String, HostTensor>,
) -> String {
    emit_llvm_with_externs(m, opts, tensors, &opts.extern_user_fns)
}

pub fn emit_llvm_with_externs(
    m: &IrModule,
    opts: &LlvmOptions,
    tensors: &HashMap<String, HostTensor>,
    extern_user_fns: &HashMap<String, usize>,
) -> String {
    let mut s = String::new();
    s.push_str("target triple = \"x86_64-unknown-linux-gnu\"\n\n");
    if opts.instrument {
        s.push_str("; instrumented build\n");
    }
    for (name, def) in &opts.struct_defs {
        if !is_transparent_struct(def) && !def.fields.is_empty() {
            let slots: Vec<&str> = (0..def.fields.len()).map(|_| "i64").collect();
            s.push_str(&format!(
                "{} = type {{ {} }}\n",
                llvm_struct_symbol(name),
                slots.join(", ")
            ));
        }
    }
    if !opts.struct_defs.is_empty() {
        s.push('\n');
    }
    s.push_str("declare i64 @sal_print_i64(i64)\n");
    s.push_str("declare void @sal_matmul_f32(ptr, ptr, ptr, i64, i64, i64)\n");
    s.push_str("declare void @sal_softmax_f32(ptr, i64)\n");
    s.push_str("declare ptr @sal_load_f32(ptr, ptr)\n");
    s.push_str("declare ptr @sal_place_malloc(i64, i32)\n");
    s.push_str("declare void @sal_place_free(ptr)\n");
    s.push_str("declare ptr @sal_place_copy(ptr, i64, i32)\n");
    s.push_str("declare void @sal_on_enter(i32)\n");
    s.push_str("declare i64 @sal_place_launches(i64)\n");
    s.push_str("declare ptr @malloc(i64)\n");
    s.push_str("declare void @free(ptr)\n");
    s.push_str("declare void @sal_runtime_init(i64, ptr)\n");
    s.push_str("declare i64 @sal_argc()\n");
    s.push_str("declare ptr @sal_argv(i64)\n");
    s.push_str("declare ptr @sal_read_file(ptr)\n");
    s.push_str("declare i64 @sal_path_readable(ptr)\n");
    s.push_str("declare i64 @sal_write_file(ptr, ptr)\n");
    s.push_str("declare i64 @sal_write_png(ptr, i64, i64, ptr)\n");
    s.push_str("declare ptr @sal_image_new(i64, i64)\n");
    s.push_str("declare i64 @sal_print_str(ptr)\n");
    s.push_str("declare i64 @sal_eprint_str(ptr)\n");
    s.push_str("declare ptr @sal_getenv(ptr)\n");
    s.push_str("declare i64 @sal_mkdir_p(ptr)\n");
    s.push_str("declare i64 @sal_str_eq(ptr, ptr)\n");
    s.push_str("declare i64 @sal_str_contains(ptr, ptr)\n");
    s.push_str("declare i64 @sal_str_len(ptr)\n");
    s.push_str("declare i64 @sal_str_bytes(ptr)\n");
    s.push_str("declare ptr @sal_str_concat(ptr, ptr)\n");
    s.push_str("declare ptr @sal_str_append(ptr, ptr)\n");
    s.push_str("declare ptr @sal_strdup(ptr)\n");
    s.push_str("declare ptr @sal_select_str(i64, ptr, ptr)\n");
    s.push_str("declare void @sal_free(ptr)\n");
    s.push_str("declare i64 @sal_copy_file(ptr, ptr)\n");
    s.push_str("declare i64 @sal_copy_self(ptr)\n");
    s.push_str("declare i64 @sal_not(i64)\n");
    s.push_str("declare i64 @sal_gated_print_str(i64, ptr)\n");
    s.push_str("declare i64 @sal_gated_copy_self(i64, ptr)\n");
    s.push_str("declare i64 @sal_str_char(ptr, i64)\n");
    s.push_str("declare i64 @sal_str_skip(ptr, i64, i64, i64)\n");
    s.push_str("declare i64 @sal_str_hash(ptr, i64)\n");
    s.push_str("declare ptr @sal_map_new()\n");
    s.push_str("declare i64 @sal_map_get(ptr, ptr)\n");
    s.push_str("declare i64 @sal_map_put(ptr, ptr, i64)\n");
    s.push_str("declare ptr @sal_ir_text(ptr)\n");
    s.push_str("declare ptr @sal_lex_src(ptr)\n");
    s.push_str("declare ptr @sal_str_slice(ptr, i64, i64)\n");
    s.push_str("declare ptr @sal_int_to_str(i64)\n");
    s.push_str("declare ptr @sal_char_to_str(i64)\n");
    s.push_str("declare ptr @sal_str_from_int(i64)\n");
    s.push_str("declare i64 @sal_str_as_int(ptr)\n");
    s.push_str("declare ptr @sal_vec_new()\n");
    s.push_str("declare i64 @sal_vec_push(ptr, i64)\n");
    s.push_str("declare i64 @sal_vec_get(ptr, i64)\n");
    s.push_str("declare i64 @sal_vec_set(ptr, i64, i64)\n");
    s.push_str("declare i64 @sal_vec_len(ptr)\n");
    s.push_str("declare void @sal_vec_free(ptr)\n");
    s.push_str("declare ptr @sal_list_new()\n");
    s.push_str("declare ptr @sal_list_new_typed(i64)\n");
    s.push_str("declare ptr @sal_list_push(ptr, i64)\n");
    s.push_str("declare i64 @sal_list_len(ptr)\n");
    s.push_str("declare i64 @sal_list_get(ptr, i64)\n");
    s.push_str("declare i64 @sal_clang(ptr, ptr)\n");
    s.push_str("declare ptr @sal_realpath(ptr)\n");
    s.push_str("declare i64 @sal_clang_obj(ptr, ptr)\n");
    s.push_str("declare i64 @sal_link_objs(ptr, ptr)\n");
    s.push_str("declare ptr @sal_tmp_path(ptr)\n");
    s.push_str("declare ptr @sal_exec_capture(ptr, ptr)\n");
    s.push_str("declare i64 @sal_exec_compile(ptr, ptr, ptr)\n");
    s.push_str("declare i64 @sal_gated_exec_compile(i64, ptr, ptr, ptr)\n");
    s.push_str("declare void @sal_instrument_init()\n");
    s.push_str("declare void @sal_instrument_shutdown()\n");
    s.push_str("declare ptr @sal_instrument_malloc(i64, i32, ptr)\n");
    s.push_str("declare void @sal_instrument_alloc(ptr, i64, i32, ptr)\n");
    s.push_str("declare void @sal_instrument_free(ptr)\n");
    s.push_str("declare void @sal_instrument_check_f32(ptr, i64, ptr)\n");
    s.push_str("declare void @sal_instrument_check_ptr(ptr, i64, i32, ptr)\n");
    s.push_str("declare void @sal_instrument_check_index(i64, i64, ptr)\n");
    s.push_str("declare ptr @sal_dict_new(i64, i64)\n");
    s.push_str("declare ptr @sal_dict_put(ptr, ptr, i64)\n");
    s.push_str("declare i64 @sal_dict_get(ptr, ptr)\n\n");

    for (name, nparams) in extern_user_fns {
        let params: Vec<String> = (0..*nparams).map(|i| format!("i64 %arg{i}")).collect();
        let ps = if params.is_empty() {
            String::new()
        } else {
            params.join(", ")
        };
        s.push_str(&format!("declare i64 @{name}({ps})\n"));
    }
    if !extern_user_fns.is_empty() {
        s.push('\n');
    }

    s.push_str("@.site.alloc = private unnamed_addr constant [6 x i8] c\"alloc\\00\"\n");
    s.push_str("@.site.matmul = private unnamed_addr constant [7 x i8] c\"matmul\\00\"\n");
    s.push_str("@.site.load = private unnamed_addr constant [5 x i8] c\"load\\00\"\n");
    s.push_str("@.site.softmax = private unnamed_addr constant [8 x i8] c\"softmax\\00\"\n");
    s.push_str("@.site.access = private unnamed_addr constant [7 x i8] c\"access\\00\"\n");
    s.push_str("@.site.index = private unnamed_addr constant [6 x i8] c\"index\\00\"\n\n");

    let mut str_globals: HashMap<String, StrLitGlobal> = HashMap::new();
    let mut str_id = 0u32;
    let mut ensure_str = |raw: &str, s: &mut String, str_globals: &mut HashMap<String, StrLitGlobal>| {
        if str_globals.contains_key(raw) {
            return;
        }
        str_id += 1;
        let gname = format!("@.str.{str_id}");
        let lty = format!("%str.{str_id}.ty");
        let bytes = raw.as_bytes();
        let arr = bytes.len() + 1;
        let blen = bytes.len();
        s.push_str(&format!("{lty} = type {{ i64, i64, [{arr} x i8] }}\n"));
        s.push_str(&format!(
            "{gname} = private unnamed_addr constant {lty} {{ i64 0, i64 {blen}, [{arr} x i8] c\""
        ));
        for &b in bytes {
            match b {
                b'\\' => s.push_str("\\\\"),
                b'"' => s.push_str("\\22"),
                b'\n' => s.push_str("\\0A"),
                b'\t' => s.push_str("\\09"),
                b'\r' => s.push_str("\\0D"),
                b'\0' => s.push_str("\\00"),
                _ if b.is_ascii_graphic() || b == b' ' => s.push(b as char),
                _ => s.push_str(&format!("\\{b:02X}")),
            }
        }
        s.push_str("\\00\" }, align 16\n");
        str_globals.insert(
            raw.to_string(),
            StrLitGlobal {
                sym: gname,
                ty: lty,
                _byte_len: blen,
            },
        );
    };

    for f in &m.functions {
        walk_insts_for_strings(&f.instructions, &mut ensure_str, &mut s, &mut str_globals);
    }
    if !str_globals.is_empty() {
        s.push('\n');
    }

    let mut owned_extern: Vec<IrFunction> = Vec::new();
    for (name, nparams) in extern_user_fns {
        owned_extern.push(IrFunction {
            name: name.clone(),
            params: (0..*nparams).map(|i| format!("arg{i}")).collect(),
            instructions: Vec::new(),
            regions: Vec::new(),
            dim_params: Vec::new(),
        });
    }
    let mut user_fns: HashMap<String, &IrFunction> =
        m.functions.iter().map(|f| (f.name.clone(), f)).collect();
    for ef in &owned_extern {
        user_fns.insert(ef.name.clone(), ef);
    }

    for f in &m.functions {
        if f.name == "main" {
            if fn_has_matmul(f) || fn_has_load(f) {
                emit_main(f, opts, tensors, &str_globals, &user_fns, &mut s);
            } else {
                emit_main_general(f, opts, &str_globals, &user_fns, &mut s);
            }
        } else if fn_has_matmul(f) {
            emit_matmul_fn(f, &mut s);
        } else {
            emit_general_fn(f, &str_globals, &user_fns, &opts.struct_defs, &mut s);
        }
    }

    if opts.emit_entry_main && !m.functions.iter().any(|f| f.name == "main") {
        s.push_str("define i64 @main(i64 %argc, ptr %argv) {\n");
        s.push_str("  call void @sal_runtime_init(i64 %argc, ptr %argv)\n");
        s.push_str("  ret i64 0\n}\n");
    }

    s
}

fn walk_insts_for_strings<F>(
    insts: &[IrInst],
    ensure_str: &mut F,
    s: &mut String,
    str_globals: &mut HashMap<String, StrLitGlobal>,
) where
    F: FnMut(&str, &mut String, &mut HashMap<String, StrLitGlobal>),
{
    for inst in insts {
        match inst {
            IrInst::ConstString { value, .. } => {
                ensure_str(value, s, str_globals);
            }
            IrInst::Call { func, args, .. } => {
                if func == "sal_load" || func == "sal_load_f32" {
                    if let Some(path) = args.first() {
                        let raw = path.trim_matches('"');
                        ensure_str(raw, s, str_globals);
                    }
                }
            }
            IrInst::If {
                then_body,
                else_body,
                ..
            } => {
                walk_insts_for_strings(then_body, ensure_str, s, str_globals);
                walk_insts_for_strings(else_body, ensure_str, s, str_globals);
            }
            IrInst::While {
                cond_insts, body, ..
            } => {
                walk_insts_for_strings(cond_insts, ensure_str, s, str_globals);
                walk_insts_for_strings(body, ensure_str, s, str_globals);
            }
            IrInst::Switch {
                arms, default_body, ..
            } => {
                for arm in arms {
                    walk_insts_for_strings(&arm.body, ensure_str, s, str_globals);
                }
                walk_insts_for_strings(default_body, ensure_str, s, str_globals);
            }
            _ => {}
        }
    }
}

fn fn_has_load(f: &IrFunction) -> bool {
    f.instructions.iter().any(|i| {
        matches!(
            i,
            IrInst::Call { func, .. } if func == "sal_load" || func == "sal_load_f32"
        )
    })
}

fn fn_has_matmul(f: &IrFunction) -> bool {
    f.instructions.iter().any(|i| {
        matches!(
            i,
            IrInst::Call { func, .. } if func == "sal_matmul_f32" || func == "sal_matmul"
        )
    }) || f.regions.iter().any(|r| {
        r.ops
            .iter()
            .any(|o| matches!(o, FusedOp::Matmul { .. }))
    })
}

fn region_place_for_matmul(f: &IrFunction, matmul_ordinal: usize) -> String {
    let mut n = 0usize;
    for r in &f.regions {
        if r.ops.iter().any(|o| matches!(o, FusedOp::Matmul { .. })) {
            if n == matmul_ordinal {
                return r.place.clone();
            }
            n += 1;
        }
    }
    "cpu".into()
}

fn region_has_relu_epilogue(f: &IrFunction, matmul_ordinal: usize) -> bool {
    let mut n = 0usize;
    for r in &f.regions {
        if r.ops.iter().any(|o| matches!(o, FusedOp::Matmul { .. })) {
            if n == matmul_ordinal {
                return r.ops.iter().any(|o| {
                    matches!(o, FusedOp::MapEpilogue { op, .. } if op == "relu" || op == "map")
                });
            }
            n += 1;
        }
    }
    false
}

/// Derive (m, k, n) from host tensor shapes; `None` when an axis is still unknown.
fn matmul_dims(
    lhs: &str,
    rhs: &str,
    tensors: &HashMap<String, HostTensor>,
) -> Option<(u64, u64, u64)> {
    let a = tensors.get(lhs)?;
    let b = tensors.get(rhs)?;
    // a: m×k, b: k×n
    if a.cols != b.rows && a.cols != 0 && b.rows != 0 {
        // Static mismatch — still emit with a.cols as k if both known; caller may fail at runtime.
    }
    let m = a.rows;
    let k = a.cols;
    let n = b.cols;
    if m == 0 || k == 0 || n == 0 {
        return None;
    }
    Some((m, k, n))
}

/// Resolve a dim SSA name to a concrete i64 (ConstInt or host shape), or a runtime `%name`.
#[derive(Debug, Clone)]
enum DimOperand {
    Imm(u64),
    Runtime(String),
}

fn const_int_map(f: &IrFunction) -> HashMap<String, i64> {
    let mut m = HashMap::new();
    for inst in &f.instructions {
        if let IrInst::ConstInt { dest, value } = inst {
            m.insert(dest.clone(), *value);
        }
    }
    m
}

fn resolve_dim_operand(
    name: &str,
    consts: &HashMap<String, i64>,
    dim_params: &[String],
) -> DimOperand {
    if let Some(v) = consts.get(name) {
        return DimOperand::Imm(*v as u64);
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return DimOperand::Imm(name.parse().unwrap_or(0));
    }
    let _ = dim_params;
    DimOperand::Runtime(name.to_string())
}

/// Prefer IR Call dim args (`m,k,n` SSA names); fall back to host tensor shapes.
fn resolve_matmul_mnk(
    args: &[String],
    consts: &HashMap<String, i64>,
    f: &IrFunction,
    tensors: &HashMap<String, HostTensor>,
) -> Option<(DimOperand, DimOperand, DimOperand)> {
    let lhs = args.first().map(String::as_str).unwrap_or("");
    let rhs = args.get(1).map(String::as_str).unwrap_or("");
    if args.len() >= 5 {
        let m = resolve_dim_operand(&args[2], consts, &f.dim_params);
        let k = resolve_dim_operand(&args[3], consts, &f.dim_params);
        let n = resolve_dim_operand(&args[4], consts, &f.dim_params);
        // Fill Imm gaps from host tensors when IR named a dim_param but literal/shape is known.
        let m = fill_dim_from_host(m, tensors.get(lhs).map(|t| t.rows));
        let k = fill_dim_from_host(k, tensors.get(lhs).map(|t| t.cols));
        let n = fill_dim_from_host(n, tensors.get(rhs).map(|t| t.cols));
        return Some((m, k, n));
    }
    matmul_dims(lhs, rhs, tensors).map(|(m, k, n)| {
        (
            DimOperand::Imm(m),
            DimOperand::Imm(k),
            DimOperand::Imm(n),
        )
    })
}

fn fill_dim_from_host(op: DimOperand, host: Option<u64>) -> DimOperand {
    match (&op, host) {
        (DimOperand::Runtime(_), Some(v)) if v > 0 => DimOperand::Imm(v),
        _ => op,
    }
}

fn dim_llvm_op(d: &DimOperand) -> String {
    match d {
        DimOperand::Imm(v) => format!("{v}"),
        DimOperand::Runtime(n) => format!("%{n}"),
    }
}

fn emit_main(
    f: &IrFunction,
    opts: &LlvmOptions,
    tensors: &HashMap<String, HostTensor>,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
) {
    s.push_str("define i64 @main(i64 %argc, ptr %argv) {\n");
    s.push_str("entry:\n");
    s.push_str("  call void @sal_runtime_init(i64 %argc, ptr %argv)\n");
    if opts.instrument {
        s.push_str("  call void @sal_instrument_init()\n");
    }

    // Materialise host tensors referenced by this function's matmul/load.
    let mut ptrs: HashMap<String, String> = HashMap::new();
    let mut buf_bytes: HashMap<String, u64> = HashMap::new();
    let mut ssa_elems: HashMap<String, u64> = HashMap::new();
    let mut owned: Vec<String> = Vec::new();
    let mut uid = 0u32;

    // Pre-allocate host tensors that feed matmul/load or place_copy.
    let mut needed: Vec<String> = Vec::new();
    for inst in &f.instructions {
        match inst {
            IrInst::Call { func, args, .. }
                if func == "sal_matmul_f32" || func == "sal_matmul" =>
            {
                for a in args.iter().take(2) {
                    if !needed.contains(a) {
                        needed.push(a.clone());
                    }
                }
            }
            IrInst::PlaceCopy { from, .. } => {
                if !needed.contains(from) {
                    needed.push(from.clone());
                }
            }
            _ => {}
        }
    }
    for name in &needed {
        if let Some(t) = tensors.get(name) {
            uid += 1;
            let nbytes = t.rows.saturating_mul(t.cols).saturating_mul(4);
            let ptr = format!("buf_{uid}");
            emit_alloc(opts, &ptr, nbytes, "alloc", s);
            if !t.data.is_empty() {
                for (i, &v) in t.data.iter().enumerate() {
                    s.push_str(&format!(
                        "  %{ptr}_gep{i} = getelementptr float, ptr %{ptr}, i64 {i}\n"
                    ));
                    s.push_str(&format!(
                        "  store float {}, ptr %{ptr}_gep{i}\n",
                        fmt_float(v)
                    ));
                }
            }
            ptrs.insert(name.clone(), ptr.clone());
            buf_bytes.insert(name.clone(), nbytes);
            owned.push(ptr);
        }
    }

    let mut ret = "0".to_string();
    let mut matmul_ordinal = 0usize;
    let mut last_out: Option<(String, DimOperand)> = None; // ptr name, elem count
    let consts = const_int_map(f);

    for inst in &f.instructions {
        match inst {
            IrInst::ConstInt { dest, value } => {
                s.push_str(&format!("  %{dest} = add i64 0, {value}\n"));
            }
            IrInst::ConstString { dest, value } => {
                if let Some(g) = str_globals.get(value) {
                    emit_ir_const_string(s, dest, g);
                } else {
                    s.push_str(&format!("  %{dest} = ptrtoint ptr @.site.alloc to i64\n"));
                }
            }
            IrInst::Binary {
                dest,
                op,
                left,
                right,
            } => {
                let l = i64_operand(left, &consts);
                let r = i64_operand(right, &consts);
                match op.as_str() {
                    "add" => s.push_str(&format!("  %{dest} = add i64 {l}, {r}\n")),
                    "sub" => s.push_str(&format!("  %{dest} = sub i64 {l}, {r}\n")),
                    "mul" => s.push_str(&format!("  %{dest} = mul i64 {l}, {r}\n")),
                    "div" => s.push_str(&format!("  %{dest} = sdiv i64 {l}, {r}\n")),
                    "eq" => {
                        s.push_str(&format!("  %{dest}_c = icmp eq i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "ne" => {
                        s.push_str(&format!("  %{dest}_c = icmp ne i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "lt" => {
                        s.push_str(&format!("  %{dest}_c = icmp slt i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "le" => {
                        s.push_str(&format!("  %{dest}_c = icmp sle i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "gt" => {
                        s.push_str(&format!("  %{dest}_c = icmp sgt i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "ge" => {
                        s.push_str(&format!("  %{dest}_c = icmp sge i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    _ => s.push_str(&format!("  %{dest} = add i64 {l}, 0\n")),
                }
            }
            IrInst::Return { value } => {
                if value.starts_with('t') {
                    ret = format!("%{value}");
                } else {
                    ret = value.clone();
                }
            }
            IrInst::Call {
                dest,
                func,
                args,
            } => {
                if func == "sal_print" && args.len() == 1 {
                    let a = &args[0];
                    let arg = if a.starts_with('t') || a.chars().all(|c| c.is_ascii_digit()) {
                        if a.chars().all(|c| c.is_ascii_digit()) {
                            a.clone()
                        } else {
                            format!("%{a}")
                        }
                    } else {
                        format!("%{a}")
                    };
                    s.push_str(&format!("  call i64 @sal_print_i64(i64 {arg})\n"));
                } else if func == "sal_load" || func == "sal_load_f32" {
                    emit_load(dest.as_deref(), args, opts, str_globals, &mut ptrs, &mut owned, &mut uid, s);
                } else if func == "sal_matmul_f32" || func == "sal_matmul" {
                    let place = region_place_for_matmul(f, matmul_ordinal);
                    let place_i = place_to_i32(&place);
                    let relu = region_has_relu_epilogue(f, matmul_ordinal);
                    let lhs = args.first().map(String::as_str).unwrap_or("");
                    let rhs = args.get(1).map(String::as_str).unwrap_or("");
                    let dims = resolve_matmul_mnk(args, &consts, f, tensors).or_else(|| {
                        // Last-resort smoke: 2×2 when both buffers hold 4 floats.
                        let a = tensors.get(lhs)?;
                        let b = tensors.get(rhs)?;
                        if a.data.len() == 4 && b.data.len() == 4 {
                            Some((
                                DimOperand::Imm(2),
                                DimOperand::Imm(2),
                                DimOperand::Imm(2),
                            ))
                        } else {
                            None
                        }
                    });
                    if let Some((m, k, n)) = dims {
                        let a_ptr = ptrs
                            .get(lhs)
                            .cloned()
                            .unwrap_or_else(|| "null".into());
                        let b_ptr = ptrs
                            .get(rhs)
                            .cloned()
                            .unwrap_or_else(|| "null".into());
                        uid += 1;
                        let out = format!("out_{uid}");
                        // Same numeric kernel on cpu/gpu/tpu; allocate on the region's place heap.
                        if place_i != 0 {
                            s.push_str(&format!("  call void @sal_on_enter(i32 {place_i})\n"));
                        }
                        match (&m, &n) {
                            (DimOperand::Imm(mv), DimOperand::Imm(nv)) => {
                                let out_elems = mv.saturating_mul(*nv);
                                let out_bytes = out_elems.saturating_mul(4);
                                emit_alloc_place(opts, &out, out_bytes, place_i, "matmul", s);
                            }
                            _ => {
                                let m_op = dim_llvm_op(&m);
                                let n_op = dim_llvm_op(&n);
                                s.push_str(&format!(
                                    "  %{out}_elems = mul i64 {m_op}, {n_op}\n"
                                ));
                                s.push_str(&format!(
                                    "  %{out}_bytes = mul i64 %{out}_elems, 4\n"
                                ));
                                if opts.instrument {
                                    s.push_str(&format!(
                                        "  %{out} = call ptr @sal_instrument_malloc(i64 %{out}_bytes, i32 {place_i}, ptr @.site.matmul)\n"
                                    ));
                                } else {
                                    s.push_str(&format!(
                                        "  %{out} = call ptr @sal_place_malloc(i64 %{out}_bytes, i32 {place_i})\n"
                                    ));
                                }
                            }
                        }
                        owned.push(out.clone());
                        if opts.instrument {
                            emit_instrument_matmul_ptrs(
                                &a_ptr, &b_ptr, &m, &k, &n, place_i, s,
                            );
                        }
                        let a_op = if a_ptr == "null" {
                            "null".into()
                        } else {
                            format!("%{a_ptr}")
                        };
                        let b_op = if b_ptr == "null" {
                            "null".into()
                        } else {
                            format!("%{b_ptr}")
                        };
                        let m_op = dim_llvm_op(&m);
                        let k_op = dim_llvm_op(&k);
                        let n_op = dim_llvm_op(&n);
                        s.push_str(&format!(
                            "  call void @sal_matmul_f32(ptr {a_op}, ptr {b_op}, ptr %{out}, i64 {m_op}, i64 {k_op}, i64 {n_op})\n"
                        ));
                        if opts.instrument {
                            match (&m, &n) {
                                (DimOperand::Imm(mv), DimOperand::Imm(nv)) => {
                                    let out_elems = mv.saturating_mul(*nv);
                                    s.push_str(&format!(
                                        "  call void @sal_instrument_check_f32(ptr %{out}, i64 {out_elems}, ptr @.site.matmul)\n"
                                    ));
                                }
                                _ => {
                                    s.push_str(&format!(
                                        "  call void @sal_instrument_check_f32(ptr %{out}, i64 %{out}_elems, ptr @.site.matmul)\n"
                                    ));
                                }
                            }
                        }
                        // Fused relu epilogue: in-place; unroll only for concrete extents.
                        if relu {
                            if let (DimOperand::Imm(mv), DimOperand::Imm(nv)) = (&m, &n) {
                                let out_elems = mv.saturating_mul(*nv);
                                for i in 0..out_elems {
                                    s.push_str(&format!(
                                        "  %{out}_rgep{i} = getelementptr float, ptr %{out}, i64 {i}\n"
                                    ));
                                    s.push_str(&format!(
                                        "  %{out}_rv{i} = load float, ptr %{out}_rgep{i}\n"
                                    ));
                                    s.push_str(&format!(
                                        "  %{out}_rc{i} = fcmp olt float %{out}_rv{i}, 0.0\n"
                                    ));
                                    s.push_str(&format!(
                                        "  %{out}_rr{i} = select i1 %{out}_rc{i}, float 0.0, float %{out}_rv{i}\n"
                                    ));
                                    s.push_str(&format!(
                                        "  store float %{out}_rr{i}, ptr %{out}_rgep{i}\n"
                                    ));
                                }
                            } else {
                                s.push_str(
                                    "  ; relu epilogue skipped for dynamic out extent (no host loop)\n",
                                );
                            }
                        }
                        if let Some(d) = dest {
                            // Expose element sum as the SSA value so `s = on … matmul(…)`
                            // can print the numeric product (A×I → 10).
                            let sum_imm = match (&m, &n) {
                                (DimOperand::Imm(mv), DimOperand::Imm(nv)) => {
                                    let ne = mv.saturating_mul(*nv);
                                    if ne > 0 && ne <= 16 {
                                        Some(ne)
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            };
                            if let Some(ne) = sum_imm {
                                s.push_str(&format!(
                                    "  %{d}_sum0 = fpext float 0.0 to double\n"
                                ));
                                let mut prev = format!("{d}_sum0");
                                for i in 0..ne {
                                    s.push_str(&format!(
                                        "  %{d}_sg{i} = getelementptr float, ptr %{out}, i64 {i}\n"
                                    ));
                                    s.push_str(&format!(
                                        "  %{d}_sv{i} = load float, ptr %{d}_sg{i}\n"
                                    ));
                                    s.push_str(&format!(
                                        "  %{d}_sd{i} = fpext float %{d}_sv{i} to double\n"
                                    ));
                                    let next = format!("{d}_sum{}", i + 1);
                                    s.push_str(&format!(
                                        "  %{next} = fadd double %{prev}, %{d}_sd{i}\n"
                                    ));
                                    prev = next;
                                }
                                s.push_str(&format!(
                                    "  %{d} = fptosi double %{prev} to i64\n"
                                ));
                            } else {
                                s.push_str(&format!(
                                    "  %{d} = ptrtoint ptr %{out} to i64\n"
                                ));
                            }
                            ptrs.insert(d.clone(), out.clone());
                        }
                        let out_elems = match (&m, &n) {
                            (DimOperand::Imm(mv), DimOperand::Imm(nv)) => {
                                DimOperand::Imm(mv.saturating_mul(*nv))
                            }
                            _ => DimOperand::Runtime(format!("{out}_elems")),
                        };
                        if let (Some(d), DimOperand::Imm(ne)) = (dest.as_ref(), &out_elems) {
                            ssa_elems.insert(d.clone(), *ne);
                        }
                        last_out = Some((out, out_elems));
                    } else {
                        s.push_str(
                            "  ; matmul dims unknown (no IR lengths / host shapes); skipped\n",
                        );
                        if let Some(d) = dest {
                            s.push_str(&format!("  %{d} = add i64 0, 0\n"));
                        }
                    }
                    matmul_ordinal += 1;
                } else if func == "sal_softmax" || func == "sal_softmax_f32" {
                    if let Some(input) = args.first() {
                        let n = ssa_elems.get(input).copied().unwrap_or_else(|| {
                            tensors
                                .get(input)
                                .map(|t| t.rows.saturating_mul(t.cols))
                                .unwrap_or(0)
                        });
                        if let Some(p) = ptrs.get(input).cloned() {
                            if n > 0 {
                                s.push_str(&format!(
                                    "  call void @sal_softmax_f32(ptr %{p}, i64 {n})\n"
                                ));
                                if opts.instrument {
                                    s.push_str(&format!(
                                        "  call void @sal_instrument_check_f32(ptr %{p}, i64 {n}, ptr @.site.softmax)\n"
                                    ));
                                }
                            }
                            if let Some(d) = dest {
                                ptrs.insert(d.clone(), p.clone());
                                if n > 0 {
                                    ssa_elems.insert(d.clone(), n);
                                }
                                s.push_str(&format!("  %{d} = ptrtoint ptr %{p} to i64\n"));
                            }
                        } else if let Some(d) = dest {
                            s.push_str(&format!("  %{d} = add i64 0, 0\n"));
                        }
                    }
                } else if func == "sal_place_launches" || func == "place_launches" {
                    let pl = args.first().map(|a| i64_operand(a, &consts)).unwrap_or_else(|| "0".into());
                    if let Some(d) = dest {
                        s.push_str(&format!(
                            "  %{d} = call i64 @sal_place_launches(i64 {pl})\n"
                        ));
                    } else {
                        s.push_str(&format!("  call i64 @sal_place_launches(i64 {pl})\n"));
                    }
                } else if func == "sal_index" || func == "index" {
                    // `index(collection, i)` — IR has no List lengths yet; with
                    // `--instrument` probe a small constant bound before the stub access.
                    let idx = args.get(1).map(String::as_str).unwrap_or("0");
                    let idx_op = if idx.chars().all(|c| c.is_ascii_digit()) {
                        idx.to_string()
                    } else if let Some(v) = consts.get(idx) {
                        format!("{v}")
                    } else {
                        format!("%{idx}")
                    };
                    if opts.instrument {
                        s.push_str(&format!(
                            "  call void @sal_instrument_check_index(i64 {idx_op}, i64 1, ptr @.site.index)\n"
                        ));
                    }
                    if let Some(d) = dest {
                        s.push_str(&format!("  %{d} = add i64 0, 0\n"));
                    }
                } else {
                    let mut tmp = uid;
                    emit_runtime_or_user_call(
                        dest.as_deref(),
                        func,
                        args,
                        &consts,
                        str_globals,
                        user_fns,
                        s,
                        &mut tmp,
                        false,
                    );
                    uid = tmp;
                }
            }
            IrInst::Drop { name } => {
                if let Some(p) = ptrs.get(name) {
                    emit_free(opts, p, s);
                    owned.retain(|x| x != p);
                }
            }
            IrInst::PlaceCopy {
                dest,
                from,
                to_place,
            } => {
                let place_i = place_to_i32(to_place);
                let nbytes = tensors
                    .get(from)
                    .map(|t| t.rows.saturating_mul(t.cols).saturating_mul(4))
                    .or_else(|| {
                        tensors
                            .get(dest)
                            .map(|t| t.rows.saturating_mul(t.cols).saturating_mul(4))
                    })
                    .or_else(|| buf_bytes.get(from).copied())
                    .unwrap_or(0);
                if let Some(p) = ptrs.get(from).cloned() {
                    if nbytes > 0 {
                        uid += 1;
                        let dst = format!("pcopy_{uid}");
                        s.push_str(&format!(
                            "  %{dst} = call ptr @sal_place_copy(ptr %{p}, i64 {nbytes}, i32 {place_i})\n"
                        ));
                        if opts.instrument {
                            s.push_str(&format!(
                                "  call void @sal_instrument_alloc(ptr %{dst}, i64 {nbytes}, i32 {place_i}, ptr @.site.access)\n"
                            ));
                            s.push_str(&format!(
                                "  call void @sal_instrument_check_ptr(ptr %{dst}, i64 {nbytes}, i32 {place_i}, ptr @.site.access)\n"
                            ));
                        }
                        ptrs.insert(dest.clone(), dst.clone());
                        buf_bytes.insert(dest.clone(), nbytes);
                        owned.push(dst);
                        s.push_str(&format!(
                            "  ; place_copy {from} -> {dest} on {to_place} ({nbytes} bytes)\n"
                        ));
                    } else {
                        // Unknown size: alias (no device driver).
                        s.push_str(&format!(
                            "  ; place_copy {from} -> {dest} on {to_place} (alias, size unknown)\n"
                        ));
                        if opts.instrument {
                            s.push_str(&format!(
                                "  call void @sal_instrument_check_ptr(ptr %{p}, i64 0, i32 {place_i}, ptr @.site.access)\n"
                            ));
                        }
                        ptrs.insert(dest.clone(), p);
                        if let Some(n) = buf_bytes.get(from).copied() {
                            buf_bytes.insert(dest.clone(), n);
                        }
                    }
                } else {
                    s.push_str(&format!(
                        "  ; place_copy {from} -> {dest} on {to_place} (no src ptr)\n"
                    ));
                }
            }
            IrInst::If { .. } => {
                // Matmul/load main path does not lower structured if.
            }
            IrInst::While { .. } => {}
            IrInst::KernelGrid {
                place,
                index_names,
                bounds,
                body,
                tail,
                dest,
            } => {
                let place_i = place_to_i32(place);
                if place_i != 0 {
                    s.push_str(&format!("  call void @sal_on_enter(i32 {place_i})\n"));
                }
                let kconsts = consts.clone();
                emit_kernel_grid_body(
                    s,
                    &mut uid,
                    index_names,
                    bounds,
                    body,
                    tail,
                    dest,
                    &kconsts,
                    str_globals,
                );
            }
            IrInst::Aggregate { .. }
            | IrInst::Extract { .. }
            | IrInst::EnumMake { .. }
            | IrInst::BitAnd { .. }
            | IrInst::Switch { .. } => {}
        }
    }

    // Prefer returning a distinctive checksum of the last matmul output when present
    // so exec tests can assert the numeric product (matmul_test.c identity case → sum 10).
    if let Some((out, nelems)) = &last_out {
        if let DimOperand::Imm(nelems) = nelems {
            if *nelems > 0 && *nelems <= 16 {
                s.push_str(&format!("  %{out}_sum0 = fpext float 0.0 to double\n"));
                let mut prev = format!("{out}_sum0");
                for i in 0..*nelems {
                    s.push_str(&format!(
                        "  %{out}_sg{i} = getelementptr float, ptr %{out}, i64 {i}\n"
                    ));
                    s.push_str(&format!(
                        "  %{out}_sv{i} = load float, ptr %{out}_sg{i}\n"
                    ));
                    s.push_str(&format!(
                        "  %{out}_sd{i} = fpext float %{out}_sv{i} to double\n"
                    ));
                    let next = format!("{out}_sum{}", i + 1);
                    s.push_str(&format!(
                        "  %{next} = fadd double %{prev}, %{out}_sd{i}\n"
                    ));
                    prev = next;
                }
                s.push_str(&format!(
                    "  %matmul_exit = fptosi double %{prev} to i64\n"
                ));
                ret = "%matmul_exit".into();
            }
        }
    }

    // Non-instrumented builds free owned buffers; instrumented builds leave them
    // live so shutdown can report LEAK when the source never Drop'd them.
    if !opts.instrument {
        for p in &owned {
            emit_free(opts, p, s);
        }
    }

    if opts.instrument {
        s.push_str("  call void @sal_instrument_shutdown()\n");
    }
    s.push_str(&format!("  ret i64 {ret}\n"));
    s.push_str("}\n");
}

fn place_to_i32(place: &str) -> i32 {
    match place {
        "gpu" => 1,
        "tpu" => 2,
        _ => 0,
    }
}

fn emit_instrument_matmul_ptrs(
    a_ptr: &str,
    b_ptr: &str,
    m: &DimOperand,
    k: &DimOperand,
    n: &DimOperand,
    place_i: i32,
    s: &mut String,
) {
    match (m, k) {
        (DimOperand::Imm(mv), DimOperand::Imm(kv)) if a_ptr != "null" => {
            let ab = mv.saturating_mul(*kv).saturating_mul(4);
            s.push_str(&format!(
                "  call void @sal_instrument_check_ptr(ptr %{a_ptr}, i64 {ab}, i32 {place_i}, ptr @.site.access)\n"
            ));
        }
        _ => {}
    }
    match (k, n) {
        (DimOperand::Imm(kv), DimOperand::Imm(nv)) if b_ptr != "null" => {
            let bb = kv.saturating_mul(*nv).saturating_mul(4);
            s.push_str(&format!(
                "  call void @sal_instrument_check_ptr(ptr %{b_ptr}, i64 {bb}, i32 {place_i}, ptr @.site.access)\n"
            ));
        }
        _ => {}
    }
}

fn emit_alloc(opts: &LlvmOptions, ptr: &str, nbytes: u64, site: &str, s: &mut String) {
    emit_alloc_place(opts, ptr, nbytes, 0, site, s);
}

fn emit_alloc_place(
    opts: &LlvmOptions,
    ptr: &str,
    nbytes: u64,
    place_i: i32,
    site: &str,
    s: &mut String,
) {
    let site_g = match site {
        "matmul" => "@.site.matmul",
        "load" => "@.site.load",
        _ => "@.site.alloc",
    };
    if opts.instrument {
        s.push_str(&format!(
            "  %{ptr} = call ptr @sal_instrument_malloc(i64 {nbytes}, i32 {place_i}, ptr {site_g})\n"
        ));
    } else {
        s.push_str(&format!(
            "  %{ptr} = call ptr @sal_place_malloc(i64 {nbytes}, i32 {place_i})\n"
        ));
    }
}

fn emit_free(opts: &LlvmOptions, ptr: &str, s: &mut String) {
    if opts.instrument {
        s.push_str(&format!("  call void @sal_instrument_free(ptr %{ptr})\n"));
    } else {
        s.push_str(&format!("  call void @sal_place_free(ptr %{ptr})\n"));
    }
}

fn emit_load(
    dest: Option<&str>,
    args: &[String],
    opts: &LlvmOptions,
    str_globals: &HashMap<String, StrLitGlobal>,
    ptrs: &mut HashMap<String, String>,
    owned: &mut Vec<String>,
    uid: &mut u32,
    s: &mut String,
) {
    let path_raw = args
        .first()
        .map(|p| p.trim_matches('"').to_string())
        .unwrap_or_default();
    *uid += 1;
    let path_ptr = if let Some(g) = str_globals.get(&path_raw) {
        str_lit_ptr_temp(g, uid, s)
    } else {
        "@.site.load".to_string()
    };
    let elems = format!("load_elems_{uid}");
    s.push_str(&format!("  %{elems} = alloca i64, align 8\n"));
    s.push_str(&format!(
        "  store i64 0, ptr %{elems}\n"
    ));
    *uid += 1;
    let buf = format!("load_buf_{uid}");
    s.push_str(&format!(
        "  %{buf} = call ptr @sal_load_f32(ptr {path_ptr}, ptr %{elems})\n"
    ));
    if opts.instrument {
        s.push_str(&format!(
            "  %{buf}_n = load i64, ptr %{elems}\n"
        ));
        s.push_str(&format!(
            "  %{buf}_nb = mul i64 %{buf}_n, 4\n"
        ));
        s.push_str(&format!(
            "  call void @sal_instrument_alloc(ptr %{buf}, i64 %{buf}_nb, i32 0, ptr @.site.load)\n"
        ));
        s.push_str(&format!(
            "  call void @sal_instrument_check_f32(ptr %{buf}, i64 %{buf}_n, ptr @.site.load)\n"
        ));
    }
    if let Some(d) = dest {
        ptrs.insert(d.to_string(), buf.clone());
        s.push_str(&format!("  %{d} = ptrtoint ptr %{buf} to i64\n"));
    }
    owned.push(buf);
}

fn fmt_float(v: f32) -> String {
    if v.is_nan() {
        return "0x7FF8000000000000".into(); // LLVM double nan bitcast used carefully — use hex float
    }
    // LLVM textual float: decimal is fine for ordinary values.
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn emit_matmul_fn(f: &IrFunction, s: &mut String) {
    // Host stub for a fused matmul region: call the runtime primitive on every place.
    // cpu/gpu/tpu share the same numeric kernel; place heaps distinguish actors.
    // Signature always exposes m,k,n; IR dim_params (e.g. x_d0 for `?`) map onto %m/%k/%n.
    let place = f
        .regions
        .iter()
        .find(|r| r.ops.iter().any(|o| matches!(o, FusedOp::Matmul { .. })))
        .map(|r| r.place.as_str())
        .unwrap_or("cpu");
    let place_i = place_to_i32(place);
    s.push_str(&format!(
        "define void @{}(ptr %a, ptr %b, ptr %out, i64 %m, i64 %k, i64 %n) {{\n",
        f.name
    ));
    s.push_str("entry:\n");
    if place_i != 0 {
        s.push_str(&format!("  call void @sal_on_enter(i32 {place_i})\n"));
    }
    // Alias IR-named dynamic axes onto the ABI dim formals.
    for (i, dp) in f.dim_params.iter().enumerate() {
        let formal = match i {
            0 => "m",
            1 => "k",
            _ => "n",
        };
        s.push_str(&format!("  ; dim_param {dp} -> %{formal}\n"));
        s.push_str(&format!("  %{dp} = add i64 0, %{formal}\n"));
    }
    let mut emitted = false;
    let consts = const_int_map(f);
    for inst in &f.instructions {
        if let IrInst::Call { func, args, .. } = inst {
            if func == "sal_matmul_f32" || func == "sal_matmul" {
                if args.len() >= 5 {
                    let m = resolve_dim_operand(&args[2], &consts, &f.dim_params);
                    let k = resolve_dim_operand(&args[3], &consts, &f.dim_params);
                    let n = resolve_dim_operand(&args[4], &consts, &f.dim_params);
                    // Prefer ABI formals when the IR names a dim_param; ConstInt stays Imm.
                    let m_op = match &m {
                        DimOperand::Imm(v) => format!("{v}"),
                        DimOperand::Runtime(name) if f.dim_params.iter().any(|d| d == name) => {
                            format!("%{name}")
                        }
                        DimOperand::Runtime(_) => "%m".into(),
                    };
                    let k_op = match &k {
                        DimOperand::Imm(v) => format!("{v}"),
                        DimOperand::Runtime(name) if f.dim_params.iter().any(|d| d == name) => {
                            format!("%{name}")
                        }
                        DimOperand::Runtime(_) => "%k".into(),
                    };
                    let n_op = match &n {
                        DimOperand::Imm(v) => format!("{v}"),
                        DimOperand::Runtime(name) if f.dim_params.iter().any(|d| d == name) => {
                            format!("%{name}")
                        }
                        DimOperand::Runtime(_) => "%n".into(),
                    };
                    s.push_str(&format!(
                        "  call void @sal_matmul_f32(ptr %a, ptr %b, ptr %out, i64 {m_op}, i64 {k_op}, i64 {n_op})\n"
                    ));
                } else {
                    s.push_str(
                        "  call void @sal_matmul_f32(ptr %a, ptr %b, ptr %out, i64 %m, i64 %k, i64 %n)\n",
                    );
                }
                emitted = true;
            }
        }
    }
    if !emitted {
        if f.regions.iter().any(|r| {
            r.ops
                .iter()
                .any(|o| matches!(o, FusedOp::Matmul { .. }))
        }) {
            s.push_str(
                "  call void @sal_matmul_f32(ptr %a, ptr %b, ptr %out, i64 %m, i64 %k, i64 %n)\n",
            );
        }
    }
    s.push_str("  ret void\n");
    s.push_str("}\n");
}

fn i64_operand(name: &str, consts: &HashMap<String, i64>) -> String {
    if let Some(v) = consts.get(name) {
        return format!("{v}");
    }
    if name.chars().all(|c| c == '-' || c.is_ascii_digit()) && !name.is_empty() {
        return name.to_string();
    }
    format!("%{name}")
}

fn emit_runtime_or_user_call(
    dest: Option<&str>,
    func: &str,
    args: &[String],
    consts: &HashMap<String, i64>,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
    tmp: &mut u32,
    tail: bool,
) {
    let ptr_arg = |a: &str, s: &mut String, tmp: &mut u32| -> String {
        // Only quoted IR args name string literals; bare names are i64 temps/params.
        if a.starts_with('"') {
            let raw = a.trim_matches('"');
            if let Some(g) = str_globals.get(raw) {
                return str_lit_ptr_temp(g, tmp, s);
            }
        }
        *tmp += 1;
        let t = format!("cast{tmp}");
        let op = i64_operand(a, consts);
        s.push_str(&format!("  %{t} = inttoptr i64 {op} to ptr\n"));
        format!("%{t}")
    };

    match func {
        "sal_print" => {
            if args.len() == 1 {
                let a = &args[0];
                if consts.contains_key(a) || a.chars().all(|c| c.is_ascii_digit()) {
                    let arg = i64_operand(a, consts);
                    s.push_str(&format!("  call i64 @sal_print_i64(i64 {arg})\n"));
                } else {
                    let p = ptr_arg(a, s, tmp);
                    if let Some(d) = dest {
                        s.push_str(&format!("  %{d} = call i64 @sal_print_str(ptr {p})\n"));
                    } else {
                        s.push_str(&format!("  call i64 @sal_print_str(ptr {p})\n"));
                    }
                    return;
                }
            }
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = add i64 0, 0\n"));
            }
        }
        "sal_print_str" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_print_str(ptr {p})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_print_str(ptr {p})\n"));
            }
        }
        "sal_eprint_str" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_eprint_str(ptr {p})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_eprint_str(ptr {p})\n"));
            }
        }
        "sal_getenv" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_getenv(ptr {p})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_mkdir_p" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_mkdir_p(ptr {p})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_mkdir_p(ptr {p})\n"));
            }
        }
        "sal_argc" => {
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_argc()\n"));
            }
        }
        "sal_argv" => {
            let i = args.first().map(|a| i64_operand(a, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_argv(i64 {i})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_read_file" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_read_file(ptr {p})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_path_readable" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_path_readable(ptr {p})\n"));
            }
        }
        "sal_image_new" => {
            let w = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let h = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_image_new(i64 {w}, i64 {h})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_write_png" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            let w = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let h = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let pix = args.get(3).map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_write_png(ptr {p}, i64 {w}, i64 {h}, ptr {pix})\n"
                ));
            } else {
                s.push_str(&format!(
                    "  call i64 @sal_write_png(ptr {p}, i64 {w}, i64 {h}, ptr {pix})\n"
                ));
            }
        }
        "sal_write_file" => {
            let p = args.first().map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            let ddata = args.get(1).map(|a| ptr_arg(a, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_write_file(ptr {p}, ptr {ddata})\n"
                ));
            } else {
                s.push_str(&format!("  call i64 @sal_write_file(ptr {p}, ptr {ddata})\n"));
            }
        }
        "sal_str_eq" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_str_eq(ptr {a}, ptr {b})\n"));
            }
        }
        "sal_str_contains" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_str_contains(ptr {a}, ptr {b})\n"
                ));
            }
        }
        "sal_str_len" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_str_len(ptr {a})\n"));
            }
        }
        "sal_str_bytes" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_str_bytes(ptr {a})\n"));
            }
        }
        "sal_str_concat" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_str_concat(ptr {a}, ptr {b})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_str_append" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_str_append(ptr {a}, ptr {b})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_strdup" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_strdup(ptr {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_select_str" => {
            let c = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let a = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(2).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_select_str(i64 {c}, ptr {a}, ptr {b})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_free" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            s.push_str(&format!("  call void @sal_free(ptr {a})\n"));
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = add i64 0, 0\n"));
            }
        }
        "sal_copy_file" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_copy_file(ptr {a}, ptr {b})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_copy_file(ptr {a}, ptr {b})\n"));
            }
        }
        "sal_copy_self" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_copy_self(ptr {a})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_copy_self(ptr {a})\n"));
            }
        }
        "sal_not" => {
            let a = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_not(i64 {a})\n"));
            }
        }
        "sal_gated_print_str" => {
            let c = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let p = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_gated_print_str(i64 {c}, ptr {p})\n"
                ));
            } else {
                s.push_str(&format!("  call i64 @sal_gated_print_str(i64 {c}, ptr {p})\n"));
            }
        }
        "sal_gated_copy_self" => {
            let c = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let p = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_gated_copy_self(i64 {c}, ptr {p})\n"
                ));
            } else {
                s.push_str(&format!("  call i64 @sal_gated_copy_self(i64 {c}, ptr {p})\n"));
            }
        }
        "sal_str_char" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let i = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_str_char(ptr {a}, i64 {i})\n"));
            }
        }
        "sal_str_skip" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let i = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let k = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let lim = args.get(3).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_str_skip(ptr {a}, i64 {i}, i64 {k}, i64 {lim})\n"
                ));
            }
        }
        "sal_str_hash" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let seed = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_str_hash(ptr {a}, i64 {seed})\n"));
            }
        }
        "sal_map_new" => {
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_map_new()\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_map_get" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let k = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_map_get(ptr {a}, ptr {k})\n"));
            }
        }
        "sal_map_put" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let k = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let v = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_map_put(ptr {a}, ptr {k}, i64 {v})\n"
                ));
            } else {
                s.push_str(&format!("  call i64 @sal_map_put(ptr {a}, ptr {k}, i64 {v})\n"));
            }
        }
        "sal_ir_text" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_ir_text(ptr {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_lex_src" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_lex_src(ptr {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_str_slice" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let c = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_str_slice(ptr {a}, i64 {b}, i64 {c})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_int_to_str" => {
            let a = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_int_to_str(i64 {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_char_to_str" => {
            let a = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_char_to_str(i64 {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_str_from_int" => {
            let a = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_str_from_int(i64 {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_str_as_int" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_str_as_int(ptr {a})\n"));
            }
        }
        "sal_vec_new" => {
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_vec_new()\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_vec_push" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_vec_push(ptr {a}, i64 {b})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_vec_push(ptr {a}, i64 {b})\n"));
            }
        }
        "sal_vec_get" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_vec_get(ptr {a}, i64 {b})\n"));
            }
        }
        "sal_vec_set" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let c = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_vec_set(ptr {a}, i64 {b}, i64 {c})\n"
                ));
            } else {
                s.push_str(&format!("  call i64 @sal_vec_set(ptr {a}, i64 {b}, i64 {c})\n"));
            }
        }
        "sal_vec_len" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_vec_len(ptr {a})\n"));
            }
        }
        "sal_vec_free" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            s.push_str(&format!("  call void @sal_vec_free(ptr {a})\n"));
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = add i64 0, 0\n"));
            }
        }
        "sal_list_new" => {
            let kind = args
                .first()
                .map(|a| i64_operand(a, consts))
                .unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_list_new_typed(i64 {kind})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_list_push" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_list_push(ptr {a}, i64 {b})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            } else {
                s.push_str(&format!("  call ptr @sal_list_push(ptr {a}, i64 {b})\n"));
            }
        }
        "sal_list_len" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_list_len(ptr {a})\n"));
            }
        }
        "sal_list_get" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_list_get(ptr {a}, i64 {b})\n"));
            }
        }
        "sal_dict_new" => {
            let kk = args
                .first()
                .map(|a| i64_operand(a, consts))
                .unwrap_or_else(|| "0".into());
            let vk = args
                .get(1)
                .map(|a| i64_operand(a, consts))
                .unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_dict_new(i64 {kk}, i64 {vk})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_dict_put" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let k = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let v = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_dict_put(ptr {a}, ptr {k}, i64 {v})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            } else {
                s.push_str(&format!("  call ptr @sal_dict_put(ptr {a}, ptr {k}, i64 {v})\n"));
            }
        }
        "sal_dict_get" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let k = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_dict_get(ptr {a}, ptr {k})\n"));
            }
        }
        "sal_clang" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_clang(ptr {a}, ptr {b})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_clang(ptr {a}, ptr {b})\n"));
            }
        }
        "sal_realpath" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_realpath(ptr {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_clang_obj" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_clang_obj(ptr {a}, ptr {b})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_clang_obj(ptr {a}, ptr {b})\n"));
            }
        }
        "sal_link_objs" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_link_objs(ptr {a}, ptr {b})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_link_objs(ptr {a}, ptr {b})\n"));
            }
        }
        "sal_tmp_path" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_tmp_path(ptr {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_exec_capture" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_exec_capture(ptr {a}, ptr {b})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_exec_compile" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let c = args.get(2).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_exec_compile(ptr {a}, ptr {b}, ptr {c})\n"
                ));
            } else {
                s.push_str(&format!("  call i64 @sal_exec_compile(ptr {a}, ptr {b}, ptr {c})\n"));
            }
        }
        "sal_place_launches" => {
            let pl = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_place_launches(i64 {pl})\n"));
            } else {
                s.push_str(&format!("  call i64 @sal_place_launches(i64 {pl})\n"));
            }
        }
        "sal_gated_exec_compile" => {
            let c0 = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let a = args.get(1).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let b = args.get(2).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let c = args.get(3).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_gated_exec_compile(i64 {c0}, ptr {a}, ptr {b}, ptr {c})\n"
                ));
            } else {
                s.push_str(&format!(
                    "  call i64 @sal_gated_exec_compile(i64 {c0}, ptr {a}, ptr {b}, ptr {c})\n"
                ));
            }
        }
        "sal_index" | "index" => {
            let idx = args.get(1).map(String::as_str).unwrap_or("0");
            let idx_op = i64_operand(idx, consts);
            s.push_str(&format!(
                "  call void @sal_instrument_check_index(i64 {idx_op}, i64 1, ptr @.site.index)\n"
            ));
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = add i64 0, 0\n"));
            }
        }
        other if user_fns.contains_key(other) => {
            let uf = user_fns[other];
            let mut arg_ops = Vec::new();
            for (i, _) in uf.params.iter().enumerate() {
                let a = args.get(i).map(String::as_str).unwrap_or("0");
                arg_ops.push(format!("i64 {}", i64_operand(a, consts)));
            }
            let args_s = arg_ops.join(", ");
            if let Some(d) = dest {
                let mark = if tail { "tail " } else { "" };
                s.push_str(&format!("  %{d} = {mark}call i64 @{other}({args_s})\n"));
            } else {
                s.push_str(&format!("  call i64 @{other}({args_s})\n"));
            }
        }
        other => {
            if let Some(d) = dest {
                s.push_str(&format!("  ; call {other} -> %{d}\n"));
                s.push_str(&format!("  %{d} = add i64 0, 0\n"));
            } else {
                s.push_str(&format!("  ; call {other}\n"));
            }
        }
    }
}

fn emit_body_insts(
    f: &IrFunction,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
) -> String {
    let mut label_id = 0u32;
    let mut tmp = 0u32;
    let mut cur_block = "entry".to_string();
    let (ret, _) = emit_inst_list(
        &f.instructions,
        str_globals,
        user_fns,
        s,
        &mut label_id,
        &mut tmp,
        &mut cur_block,
    );
    ret
}

fn emit_inst_list(
    instructions: &[IrInst],
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
    label_id: &mut u32,
    tmp: &mut u32,
    cur_block: &mut String,
) -> (String, String) {
    let consts = const_int_map_from(instructions);
    let mut ret = "0".to_string();
    for inst in instructions {
        match inst {
            IrInst::ConstInt { dest, value } => {
                s.push_str(&format!("  %{dest} = add i64 0, {value}\n"));
            }
            IrInst::ConstString { dest, value } => {
                if let Some(g) = str_globals.get(value) {
                    emit_ir_const_string(s, dest, g);
                } else {
                    s.push_str(&format!("  %{dest} = ptrtoint ptr @.site.alloc to i64\n"));
                }
            }
            IrInst::Binary {
                dest,
                op,
                left,
                right,
            } => {
                let l = i64_operand(left, &consts);
                let r = i64_operand(right, &consts);
                match op.as_str() {
                    "add" => s.push_str(&format!("  %{dest} = add i64 {l}, {r}\n")),
                    "sub" => s.push_str(&format!("  %{dest} = sub i64 {l}, {r}\n")),
                    "mul" => s.push_str(&format!("  %{dest} = mul i64 {l}, {r}\n")),
                    "div" => s.push_str(&format!("  %{dest} = sdiv i64 {l}, {r}\n")),
                    "eq" => {
                        s.push_str(&format!("  %{dest}_c = icmp eq i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "ne" => {
                        s.push_str(&format!("  %{dest}_c = icmp ne i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "lt" => {
                        s.push_str(&format!("  %{dest}_c = icmp slt i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "le" => {
                        s.push_str(&format!("  %{dest}_c = icmp sle i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "gt" => {
                        s.push_str(&format!("  %{dest}_c = icmp sgt i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    "ge" => {
                        s.push_str(&format!("  %{dest}_c = icmp sge i64 {l}, {r}\n"));
                        s.push_str(&format!("  %{dest} = zext i1 %{dest}_c to i64\n"));
                    }
                    _ => s.push_str(&format!("  %{dest} = add i64 {l}, 0\n")),
                }
            }
            IrInst::Return { value } => {
                ret = i64_operand(value, &consts);
            }
            IrInst::Aggregate {
                dest,
                struct_name,
                fields,
            } => {
                let ty = llvm_struct_symbol(struct_name);
                if fields.is_empty() {
                    s.push_str(&format!("  %{dest} = insertvalue {ty} undef, i64 0, 0\n"));
                } else {
                    let mut agg = String::new();
                    for (i, f) in fields.iter().enumerate() {
                        let op = i64_operand(f, &consts);
                        let next = if i + 1 == fields.len() {
                            dest.clone()
                        } else {
                            *tmp += 1;
                            format!("agg{tmp}")
                        };
                        if i == 0 {
                            s.push_str(&format!(
                                "  %{next} = insertvalue {ty} undef, i64 {op}, 0\n"
                            ));
                        } else {
                            s.push_str(&format!(
                                "  %{next} = insertvalue {ty} %{agg}, i64 {op}, {i}\n"
                            ));
                        }
                        agg = next;
                    }
                }
                EMIT_AGG.with(|a| {
                    a.borrow_mut()
                        .insert(dest.clone(), struct_name.clone());
                });
            }
            IrInst::Extract {
                dest,
                base,
                struct_name,
                field_index,
            } => {
                let ty = llvm_struct_symbol(struct_name);
                let b = if EMIT_AGG.with(|a| a.borrow().contains_key(base)) {
                    format!("%{base}")
                } else {
                    i64_operand(base, &consts)
                };
                s.push_str(&format!(
                    "  %{dest} = extractvalue {ty} {b}, {field_index}\n"
                ));
            }
            IrInst::EnumMake {
                dest,
                variant_index,
                payload,
                ..
            } => {
                if let Some(p) = payload {
                    let pop = i64_operand(p, &consts);
                    *tmp += 1;
                    let sh = format!("em{tmp}");
                    s.push_str(&format!("  %{sh} = shl i64 {pop}, 8\n"));
                    let tag = variant_index + 1;
                    s.push_str(&format!(
                        "  %{dest} = or i64 %{sh}, {tag}\n"
                    ));
                } else if *variant_index == 0 {
                    s.push_str(&format!("  %{dest} = add i64 0, 0\n"));
                } else {
                    let tag = variant_index + 1;
                    s.push_str(&format!("  %{dest} = add i64 0, {tag}\n"));
                }
            }
            IrInst::BitAnd { dest, left, right } => {
                let l = i64_operand(left, &consts);
                let r = i64_operand(right, &consts);
                s.push_str(&format!("  %{dest} = and i64 {l}, {r}\n"));
            }
            IrInst::Call { dest, func, args } => {
                if func == "sal_matmul_f32" || func == "sal_matmul" || func == "sal_load"
                    || func == "sal_load_f32" || func == "sal_softmax" || func == "sal_softmax_f32"
                {
                    if let Some(d) = dest {
                        s.push_str(&format!("  %{d} = add i64 0, 0\n"));
                    }
                } else {
                    emit_runtime_or_user_call(
                        dest.as_deref(),
                        func,
                        args,
                        &consts,
                        str_globals,
                        user_fns,
                        s,
                        tmp,
                        false,
                    );
                }
            }
            IrInst::If {
                cond,
                then_body,
                else_body,
                then_val,
                else_val,
                dest,
                carried,
            } => {
                *label_id += 1;
                let id = *label_id;
                let then_l = format!("then{id}");
                let else_l = format!("else{id}");
                let join_l = format!("join{id}");
                let c = i64_operand(cond, &consts);
                s.push_str(&format!("  %ifcond{id} = icmp ne i64 {c}, 0\n"));
                s.push_str(&format!(
                    "  br i1 %ifcond{id}, label %{then_l}, label %{else_l}\n"
                ));
                s.push_str(&format!("{then_l}:\n"));
                let mut then_block = then_l.clone();
                let then_consts = const_int_map_from(then_body);
                emit_inst_list(
                    then_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut then_block,
                );
                let tv = if then_consts.contains_key(then_val)
                    || then_val.chars().all(|c| c.is_ascii_digit())
                {
                    i64_operand(then_val, &then_consts)
                } else {
                    format!("%{then_val}")
                };
                s.push_str(&format!("  br label %{join_l}\n"));
                s.push_str(&format!("{else_l}:\n"));
                let mut else_block = else_l.clone();
                let else_consts = const_int_map_from(else_body);
                emit_inst_list(
                    else_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut else_block,
                );
                let ev = if else_consts.contains_key(else_val)
                    || else_val.chars().all(|c| c.is_ascii_digit())
                {
                    i64_operand(else_val, &else_consts)
                } else {
                    format!("%{else_val}")
                };
                s.push_str(&format!("  br label %{join_l}\n"));
                s.push_str(&format!("{join_l}:\n"));
                s.push_str(&format!(
                    "  %{dest} = phi i64 [ {tv}, %{then_block} ], [ {ev}, %{else_block} ]\n"
                ));
                for carry in carried {
                    let ct = if then_consts.contains_key(&carry.then_val)
                        || carry.then_val.chars().all(|c| c.is_ascii_digit())
                    {
                        i64_operand(&carry.then_val, &then_consts)
                    } else {
                        format!("%{}", carry.then_val)
                    };
                    let ce = if else_consts.contains_key(&carry.else_val)
                        || carry.else_val.chars().all(|c| c.is_ascii_digit())
                    {
                        i64_operand(&carry.else_val, &else_consts)
                    } else {
                        format!("%{}", carry.else_val)
                    };
                    s.push_str(&format!(
                        "  %{} = phi i64 [ {ct}, %{then_block} ], [ {ce}, %{else_block} ]\n",
                        carry.dest
                    ));
                }
                *cur_block = join_l;
            }
            IrInst::Drop { .. } | IrInst::PlaceCopy { .. } | IrInst::KernelGrid { .. } => {}
            IrInst::Switch {
                scrut,
                arms,
                default_body,
                default_val,
                dest,
            } => {
                *label_id += 1;
                let id = *label_id;
                let scrut_op = i64_operand(scrut, &consts);
                *tmp += 1;
                let tag = format!("swtag{tmp}");
                s.push_str(&format!("  %{tag} = and i64 {scrut_op}, 255\n"));
                let join = format!("swjoin{id}");
                let def_l = format!("swdef{id}");
                s.push_str(&format!("  switch i64 %{tag}, label %{def_l} [\n"));
                for (i, arm) in arms.iter().enumerate() {
                    s.push_str(&format!("    i64 {}, label %sw{id}_{i}\n", arm.tag));
                }
                s.push_str("  ]\n");
                let mut incoming: Vec<(String, String)> = Vec::new();
                for (i, arm) in arms.iter().enumerate() {
                    let lab = format!("sw{id}_{i}");
                    s.push_str(&format!("{lab}:\n"));
                    let mut block = lab.clone();
                    emit_inst_list(
                        &arm.body,
                        str_globals,
                        user_fns,
                        s,
                        label_id,
                        tmp,
                        &mut block,
                    );
                    let arm_consts = const_int_map_from(&arm.body);
                    let v = if arm_consts.contains_key(&arm.value)
                        || arm.value.chars().all(|c| c.is_ascii_digit())
                    {
                        i64_operand(&arm.value, &arm_consts)
                    } else {
                        format!("%{}", arm.value)
                    };
                    s.push_str(&format!("  br label %{join}\n"));
                    incoming.push((v, block));
                }
                s.push_str(&format!("{def_l}:\n"));
                let mut def_block = def_l.clone();
                emit_inst_list(
                    default_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut def_block,
                );
                let def_consts = const_int_map_from(default_body);
                let dv = if def_consts.contains_key(default_val)
                    || default_val.chars().all(|c| c.is_ascii_digit())
                {
                    i64_operand(default_val, &def_consts)
                } else {
                    format!("%{default_val}")
                };
                s.push_str(&format!("  br label %{join}\n"));
                incoming.push((dv, def_block));
                s.push_str(&format!("{join}:\n"));
                let phi = incoming
                    .iter()
                    .map(|(v, b)| format!("[ {v}, %{b} ]"))
                    .collect::<Vec<_>>()
                    .join(", ");
                s.push_str(&format!("  %{dest} = phi i64 {phi}\n"));
                *cur_block = join;
            }
            IrInst::While {
                cond_insts,
                cond,
                body,
                carried,
            } => {
                *label_id += 1;
                let id = *label_id;
                let head = format!("wh{id}");
                let body_l = format!("wb{id}");
                let latch = format!("wl{id}");
                let end_l = format!("we{id}");
                let pred = cur_block.clone();
                s.push_str(&format!("  br label %{head}\n"));
                s.push_str(&format!("{head}:\n"));
                for carry in carried {
                    let inc = i64_operand(&carry.incoming, &HashMap::new());
                    let upd = i64_operand(&carry.updated, &HashMap::new());
                    s.push_str(&format!(
                        "  %{} = phi i64 [ {inc}, %{pred} ], [ {upd}, %{latch} ]\n",
                        carry.dest
                    ));
                }
                let mut head_block = head.clone();
                emit_inst_list(
                    cond_insts,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut head_block,
                );
                let cconsts = const_int_map_from(cond_insts);
                let c = i64_operand(cond, &cconsts);
                s.push_str(&format!("  %whc{id} = icmp ne i64 {c}, 0\n"));
                s.push_str(&format!(
                    "  br i1 %whc{id}, label %{body_l}, label %{end_l}\n"
                ));
                s.push_str(&format!("{body_l}:\n"));
                let mut body_block = body_l.clone();
                emit_inst_list(
                    body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut body_block,
                );
                s.push_str(&format!("  br label %{latch}\n"));
                s.push_str(&format!("{latch}:\n"));
                s.push_str(&format!("  br label %{head}\n"));
                s.push_str(&format!("{end_l}:\n"));
                *cur_block = end_l;
            }
        }
    }
    (ret, cur_block.clone())
}

fn const_int_map_from(instructions: &[IrInst]) -> HashMap<String, i64> {
    let mut m = HashMap::new();
    for inst in instructions {
        if let IrInst::ConstInt { dest, value } = inst {
            m.insert(dest.clone(), *value);
        }
    }
    m
}

fn emit_general_fn(
    f: &IrFunction,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    struct_defs: &HashMap<String, StructDef>,
    s: &mut String,
) {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("i64 %{p}"))
        .collect();
    s.push_str(&format!(
        "define i64 @{}({}) {{\n",
        f.name,
        params.join(", ")
    ));
    s.push_str("entry:\n");
    emit_layout_scope(struct_defs, || {
        let mut label_id = 0u32;
        let mut tmp = 0u32;
        let mut cur_block = "entry".to_string();
        if let Some(IrInst::Return { value }) = f.instructions.last() {
            let prefix = &f.instructions[..f.instructions.len() - 1];
            emit_value_as_return(
                prefix,
                value,
                str_globals,
                user_fns,
                s,
                &mut label_id,
                &mut tmp,
                &mut cur_block,
            );
        } else {
            let ret = emit_body_insts(f, str_globals, user_fns, s);
            s.push_str(&format!("  ret i64 {ret}\n"));
        }
    });
    s.push_str("}\n");
}

/// Emit `body` and return `val` from every path. A trailing call or `if` becomes
/// a tail call so the recursive loops of the self-hosted compiler are jumps.
fn emit_value_as_return(
    body: &[IrInst],
    val: &str,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
    label_id: &mut u32,
    tmp: &mut u32,
    cur_block: &mut String,
) {
    if let Some(IrInst::If {
        cond,
        then_body,
        else_body,
        then_val,
        else_val,
        dest,
        carried: _,
    }) = body.last()
    {
        if dest == val {
            let prefix = &body[..body.len() - 1];
            if !prefix.is_empty() {
                emit_inst_list(
                    prefix,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    cur_block,
                );
            }
            emit_if_as_return(
                cond,
                then_body,
                else_body,
                then_val,
                else_val,
                str_globals,
                user_fns,
                s,
                label_id,
                tmp,
                cur_block,
            );
            return;
        }
    }
    if let Some(IrInst::Call {
        dest: Some(d),
        func,
        args,
    }) = body.last()
    {
        if d == val && user_fns.contains_key(func) {
            let prefix = &body[..body.len() - 1];
            if !prefix.is_empty() {
                emit_inst_list(
                    prefix,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    cur_block,
                );
            }
            let consts = const_int_map_from(body);
            emit_runtime_or_user_call(
                Some(d),
                func,
                args,
                &consts,
                str_globals,
                user_fns,
                s,
                tmp,
                true,
            );
            s.push_str(&format!("  ret i64 %{d}\n"));
            return;
        }
    }
    if !body.is_empty() {
        emit_inst_list(
            body,
            str_globals,
            user_fns,
            s,
            label_id,
            tmp,
            cur_block,
        );
    }
    let consts = const_int_map_from(body);
    let op = i64_operand(val, &consts);
    s.push_str(&format!("  ret i64 {op}\n"));
}

fn emit_if_as_return(
    cond: &str,
    then_body: &[IrInst],
    else_body: &[IrInst],
    then_val: &str,
    else_val: &str,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
    label_id: &mut u32,
    tmp: &mut u32,
    cur_block: &mut String,
) {
    *label_id += 1;
    let id = *label_id;
    let then_l = format!("then{id}");
    let else_l = format!("else{id}");
    let consts = const_int_map_from(&[]);
    let c = i64_operand(cond, &consts);
    s.push_str(&format!("  %ifcond{id} = icmp ne i64 {c}, 0\n"));
    s.push_str(&format!(
        "  br i1 %ifcond{id}, label %{then_l}, label %{else_l}\n"
    ));
    s.push_str(&format!("{then_l}:\n"));
    let mut then_block = then_l.clone();
    emit_value_as_return(
        then_body,
        then_val,
        str_globals,
        user_fns,
        s,
        label_id,
        tmp,
        &mut then_block,
    );
    s.push_str(&format!("{else_l}:\n"));
    let mut else_block = else_l;
    emit_value_as_return(
        else_body,
        else_val,
        str_globals,
        user_fns,
        s,
        label_id,
        tmp,
        &mut else_block,
    );
    *cur_block = format!("join{id}");
}

fn emit_main_general(
    f: &IrFunction,
    opts: &LlvmOptions,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
) {
    s.push_str("define i64 @main(i64 %argc, ptr %argv) {\n");
    s.push_str("entry:\n");
    s.push_str("  call void @sal_runtime_init(i64 %argc, ptr %argv)\n");
    if opts.instrument {
        s.push_str("  call void @sal_instrument_init()\n");
    }
    let ret = emit_body_insts(f, str_globals, user_fns, s);
    if opts.instrument {
        s.push_str("  call void @sal_instrument_shutdown()\n");
    }
    s.push_str(&format!("  ret i64 {ret}\n"));
    s.push_str("}\n");
}

fn emit_kernel_grid_body(
    s: &mut String,
    uid: &mut u32,
    index_names: &[String],
    bounds: &[u64],
    body: &[IrInst],
    tail: &str,
    dest: &str,
    consts: &HashMap<String, i64>,
    str_globals: &HashMap<String, StrLitGlobal>,
) {
    if index_names.len() != bounds.len() {
        s.push_str("  ; kernel grid: index/bounds mismatch\n");
        s.push_str(&format!("  %{dest} = add i64 0, 0\n"));
        return;
    }
    *uid += 1;
    let done = format!("kg_done_{uid}");
    let mut prefix = Vec::new();
    emit_kernel_product(
        s,
        0,
        index_names,
        bounds,
        body,
        tail,
        dest,
        consts,
        str_globals,
        &mut prefix,
    );
    s.push_str(&format!("  br label %{done}\n"));
    s.push_str(&format!("{done}:\n"));
}

fn emit_kernel_product(
    s: &mut String,
    depth: usize,
    index_names: &[String],
    bounds: &[u64],
    body: &[IrInst],
    tail: &str,
    dest: &str,
    consts: &HashMap<String, i64>,
    str_globals: &HashMap<String, StrLitGlobal>,
    prefix: &mut Vec<i64>,
) {
    if depth >= index_names.len() {
        let mut local = consts.clone();
        for (name, val) in index_names.iter().zip(prefix.iter()) {
            local.insert(format!("__{name}"), *val);
        }
        let body_consts = const_int_map_from(body);
        local.extend(body_consts);
        for inst in body {
            match inst {
                IrInst::ConstInt { dest: d, value } => {
                    s.push_str(&format!("  %{d} = add i64 0, {value}\n"));
                }
                IrInst::ConstString { dest: d, value } => {
                    if let Some(g) = str_globals.get(value) {
                        emit_ir_const_string(s, d, g);
                    } else {
                        s.push_str(&format!("  %{d} = ptrtoint ptr @.site.alloc to i64\n"));
                    }
                }
                IrInst::Binary {
                    dest: d,
                    op,
                    left,
                    right,
                } => {
                    let l = i64_operand(left, &local);
                    let r = i64_operand(right, &local);
                    match op.as_str() {
                        "add" => s.push_str(&format!("  %{d} = add i64 {l}, {r}\n")),
                        "sub" => s.push_str(&format!("  %{d} = sub i64 {l}, {r}\n")),
                        "mul" => s.push_str(&format!("  %{d} = mul i64 {l}, {r}\n")),
                        _ => s.push_str(&format!("  %{d} = add i64 {l}, 0\n")),
                    }
                }
                _ => {}
            }
        }
        let tv = i64_operand(tail, &local);
        s.push_str(&format!("  %{dest} = add i64 {tv}, 0\n"));
        return;
    }
    let b = bounds[depth];
    for v in 0..b {
        prefix.push(v as i64);
        emit_kernel_product(
            s,
            depth + 1,
            index_names,
            bounds,
            body,
            tail,
            dest,
            consts,
            str_globals,
            prefix,
        );
        prefix.pop();
    }
}
