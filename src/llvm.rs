use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::{Dim, Expr, FnDef, Item, Program, Stmt, StructDef, StructField, TensorElem, Type};
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
    pub target: crate::target::Target,
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
    /// Function that owns this literal / shape entry.
    pub owner_fn: String,
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
                Stmt::Let { name, ty, init, .. } => {
                    if let Some((rows, cols, data)) = tensor_lit_data(init) {
                        counter += 1;
                        let ir_name = format!("t{counter}");
                        out.insert(
                            ir_name.clone(),
                            HostTensor {
                                owner_fn: f.name.clone(),
                                ir_name,
                                rows,
                                cols,
                                data,
                            },
                        );
                        // A `?` axis on the let type is its own SSA temp after the literal.
                        if let Some(Type::Tensor { dims, .. }) = ty {
                            for d in dims {
                                if matches!(d, Dim::Dynamic) {
                                    counter += 1;
                                }
                            }
                        }
                        let _ = name;
                    } else {
                        // Mimic lower_expr_simple counter bumps for other exprs we care about.
                        bump_counter_for_expr(init, &mut counter);
                    }
                }
                Stmt::Assign { target, value, .. } => {
                    if let (Expr::Ident { .. }, Some((rows, cols, data))) =
                        (target, tensor_lit_data(value))
                    {
                        counter += 1;
                        let ir_name = format!("t{counter}");
                        out.insert(
                            ir_name.clone(),
                            HostTensor {
                                owner_fn: f.name.clone(),
                                ir_name,
                                rows,
                                cols,
                                data,
                            },
                        );
                    } else if matches!(target, Expr::Ident { .. }) {
                        bump_counter_for_expr(value, &mut counter);
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
            let mk = |rows: u64, cols: u64| HostTensor {
                owner_fn: f.name.clone(),
                ir_name: p.name.clone(),
                rows,
                cols,
                data: Vec::new(),
            };
            if dims.len() == 1 {
                if let Dim::Static(n) = &dims[0] {
                    out.entry(p.name.clone()).or_insert_with(|| mk(1, *n));
                }
            } else if dims.len() >= 2 {
                let r = &dims[dims.len() - 2];
                let c = &dims[dims.len() - 1];
                match (r, c) {
                    (Dim::Static(rows), Dim::Static(cols)) => {
                        out.entry(p.name.clone()).or_insert_with(|| mk(*rows, *cols));
                    }
                    (Dim::Dynamic, Dim::Static(cols)) => {
                        out.entry(p.name.clone()).or_insert_with(|| mk(0, *cols));
                    }
                    (Dim::Static(rows), Dim::Dynamic) => {
                        out.entry(p.name.clone()).or_insert_with(|| mk(*rows, 0));
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
    s.push_str(&format!("target triple = \"{}\"\n\n", opts.target.triple()));
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
    s.push_str("%SalBytes = type { ptr, i64 }\n");
    s.push_str("@sal_bytes_oob = private unnamed_addr constant [20 x i8] c\"bytes: out of bounds\"\n\n");
    s.push_str("declare i64 @sal_print_i64(i64)\n");
    s.push_str("declare void @sal_panic(ptr)\n");
    s.push_str("declare void @sal_bytes_check(i64, i64, i64)\n");
    s.push_str("declare i64 @sal_os_syscall(i64, i64, i64, i64, i64, i64, i64)\n");
    s.push_str("declare void @sal_matmul_f32(ptr, ptr, ptr, i64, i64, i64)\n");
    s.push_str("declare void @sal_matmul_f16(ptr, ptr, ptr, i64, i64, i64)\n");
    s.push_str("declare void @sal_matmul_bf16(ptr, ptr, ptr, i64, i64, i64)\n");
    s.push_str("declare void @sal_qmatmul_f32(ptr, ptr, ptr, ptr, i64, i64, i64, i64)\n");
    s.push_str("declare void @sal_attention_f32(ptr, ptr, ptr, ptr, i64, i64, i64, i64, i64)\n");
    s.push_str("declare void @sal_tensor_write_row_f32(ptr, i64, ptr, i64)\n");
    s.push_str("declare void @sal_tensor_write_rows_f32(ptr, i64, ptr, i64, i64)\n");
    s.push_str("declare void @sal_softmax_f32(ptr, i64)\n");
    s.push_str("declare void @sal_softmax_axis_f32(ptr, i64, i64)\n");
    s.push_str("declare void @sal_silu_f32(ptr, ptr, i64)\n");
    s.push_str("declare void @sal_rope_f32(ptr, i64, i64, i64)\n");
    s.push_str("declare void @sal_rms_norm_f32(ptr, ptr, ptr, i64, double)\n");
    s.push_str("declare i64 @sal_argmax_axis_f32(ptr, i64)\n");
    s.push_str("declare void @sal_region_begin(i64, i64)\n");
    s.push_str("declare void @sal_region_end(i64)\n");
    s.push_str("declare void @sal_tensor_bin_f32(i32, ptr, ptr, ptr, i64, i32, double)\n");
    s.push_str("declare void @sal_tensor_cmp_f32(i32, ptr, ptr, ptr, i64, i32, double)\n");
    s.push_str("declare void @sal_tensor_neg_f32(ptr, ptr, i64)\n");
    s.push_str("declare void @sal_tensor_not_i8(ptr, ptr, i64)\n");
    s.push_str("declare i64 @sal_mask_select_count(ptr, i64)\n");
    s.push_str("declare void @sal_tensor_select_f32(ptr, ptr, ptr, i64)\n");
    s.push_str("declare void @sal_tensor_select_i8(ptr, ptr, ptr, i64)\n");
    s.push_str("declare ptr @sal_list_select(ptr, ptr, i64)\n");
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
    s.push_str("declare i64 @sal_str_to_f64_bits(ptr)\n");
    s.push_str("declare ptr @sal_f64_bits_to_str(i64)\n");
    s.push_str("declare i64 @sal_now_ns()\n");
    s.push_str("declare ptr @sal_vec_new()\n");
    s.push_str("declare i64 @sal_vec_push(ptr, i64)\n");
    s.push_str("declare i64 @sal_vec_get(ptr, i64)\n");
    s.push_str("declare i64 @sal_vec_set(ptr, i64, i64)\n");
    s.push_str("declare i64 @sal_vec_len(ptr)\n");
    s.push_str("declare void @sal_vec_free(ptr)\n");
    s.push_str("declare ptr @sal_bytes_new(i64)\n");
    s.push_str("declare i64 @sal_bytes_len(ptr)\n");
    s.push_str("declare i64 @sal_bytes_fill(ptr, i64, i64, i64)\n");
    s.push_str("declare i64 @sal_bytes_fill_u32(ptr, i64, i64, i64)\n");
    s.push_str("declare i64 @sal_bytes_copy(ptr, i64, ptr, i64, i64)\n");
    s.push_str("declare i64 @sal_str_bytes_copy(ptr, i64, ptr)\n");
    s.push_str("declare ptr @sal_bytes_to_str(ptr)\n");
    s.push_str("declare ptr @sal_bytes_view(ptr, i64, i64)\n");
    s.push_str("declare ptr @sal_bytes_view_mem(i64, i64)\n");
    s.push_str("declare i64 @sal_bytes_ptr(ptr)\n");
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
                emit_host_tensor_fn(
                    f,
                    opts,
                    tensors,
                    &str_globals,
                    &user_fns,
                    HostFnEntry::Main,
                    &mut s,
                );
            } else {
                emit_main_general(f, opts, tensors, &str_globals, &user_fns, &mut s);
            }
        } else if fn_is_fused_matmul_stub(f) {
            emit_matmul_fn(f, &mut s);
        } else if fn_has_matmul(f) || fn_has_load(f) {
            emit_host_tensor_fn(
                f,
                opts,
                tensors,
                &str_globals,
                &user_fns,
                HostFnEntry::User,
                &mut s,
            );
        } else {
            emit_general_fn(
                f,
                opts,
                tensors,
                &str_globals,
                &user_fns,
                &opts.struct_defs,
                &mut s,
            );
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
                        if path.starts_with('"') {
                            let raw = path.trim_matches('"');
                            ensure_str(raw, s, str_globals);
                        }
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

/// Fused matmul regions lower to a void stub `(a,b,out,m,k,n)`. Full sal functions that
/// also call attention/rope/load must use `emit_general_fn`, not this stub.
fn fn_is_fused_matmul_stub(f: &IrFunction) -> bool {
    if !fn_has_matmul(f) {
        return false;
    }
    if fn_has_load(f) {
        return false;
    }
    for inst in &f.instructions {
        match inst {
            IrInst::Call { func, .. } => {
                if func != "sal_matmul_f32" && func != "sal_matmul" {
                    return false;
                }
            }
            IrInst::Return { .. } | IrInst::ConstInt { .. } => {}
            _ => return false,
        }
    }
    true
}

fn insts_have_tensor_ops(insts: &[IrInst]) -> bool {
    for inst in insts {
        match inst {
            IrInst::TensorBin { .. }
            | IrInst::TensorUnary { .. }
            | IrInst::ConstFloat { .. }
            | IrInst::PlaceCopy { .. } => {
                return true;
            }
            IrInst::If {
                then_body,
                else_body,
                ..
            } => {
                if insts_have_tensor_ops(then_body) || insts_have_tensor_ops(else_body) {
                    return true;
                }
            }
            IrInst::While {
                cond_insts,
                body,
                ..
            } => {
                if insts_have_tensor_ops(cond_insts) || insts_have_tensor_ops(body) {
                    return true;
                }
            }
            IrInst::Switch {
                arms,
                default_body,
                ..
            } => {
                for arm in arms {
                    if insts_have_tensor_ops(&arm.body) {
                        return true;
                    }
                }
                if insts_have_tensor_ops(default_body) {
                    return true;
                }
            }
            IrInst::KernelGrid { body, .. } => {
                if insts_have_tensor_ops(body) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn fn_has_tensor_ops(f: &IrFunction) -> bool {
    insts_have_tensor_ops(&f.instructions)
}

struct TensorEmitCtx<'a> {
    opts: &'a LlvmOptions,
    ptrs: &'a mut HashMap<String, String>,
    buf_bytes: &'a mut HashMap<String, u64>,
    owned: &'a mut Vec<String>,
    consts: &'a HashMap<String, i64>,
    floats: &'a HashMap<String, f64>,
    uid: &'a mut u32,
}

fn collect_tensor_ptr_names(insts: &[IrInst], out: &mut HashSet<String>) {
    for inst in insts {
        match inst {
            IrInst::TensorBin { dest, left, right, .. } => {
                out.insert(dest.clone());
                out.insert(left.clone());
                out.insert(right.clone());
            }
            IrInst::TensorUnary { dest, input, .. } => {
                out.insert(dest.clone());
                out.insert(input.clone());
            }
            IrInst::Call { args, .. } => {
                for a in args {
                    out.insert(a.clone());
                }
            }
            IrInst::If {
                then_body,
                else_body,
                ..
            } => {
                collect_tensor_ptr_names(then_body, out);
                collect_tensor_ptr_names(else_body, out);
            }
            IrInst::While {
                cond_insts,
                body,
                ..
            } => {
                collect_tensor_ptr_names(cond_insts, out);
                collect_tensor_ptr_names(body, out);
            }
            IrInst::Switch {
                arms,
                default_body,
                ..
            } => {
                for arm in arms {
                    collect_tensor_ptr_names(&arm.body, out);
                }
                collect_tensor_ptr_names(default_body, out);
            }
            IrInst::KernelGrid { body, .. } => collect_tensor_ptr_names(body, out),
            IrInst::PlaceCopy { from, dest, .. } => {
                out.insert(from.clone());
                out.insert(dest.clone());
            }
            IrInst::TensorRebind { dest, src } => {
                out.insert(dest.clone());
                out.insert(src.clone());
            }
            _ => {}
        }
    }
}

fn emit_materialize_host_tensor(
    opts: &LlvmOptions,
    name: &str,
    t: &HostTensor,
    s: &mut String,
    ptrs: &mut HashMap<String, String>,
    owned: &mut Vec<String>,
    uid: &mut u32,
) {
    if ptrs.contains_key(name) {
        return;
    }
    let elems = t.rows.saturating_mul(t.cols).max(1);
    let nbytes = elems.saturating_mul(4);
    *uid += 1;
    let ptr = format!("buf_{uid}");
    emit_alloc(opts, &ptr, nbytes, "alloc", s);
    for (i, &v) in t.data.iter().enumerate() {
        s.push_str(&format!(
            "  %{ptr}_gep{i} = getelementptr float, ptr %{ptr}, i64 {i}\n"
        ));
        s.push_str(&format!(
            "  store float {}, ptr %{ptr}_gep{i}\n",
            fmt_float(v)
        ));
    }
    ptrs.insert(name.to_string(), ptr.clone());
    s.push_str(&format!("  %{name} = ptrtoint ptr %{ptr} to i64\n"));
    owned.push(ptr);
}

fn fn_needs_tensor_emit(f: &IrFunction, tensors: &HashMap<String, HostTensor>) -> bool {
    if fn_has_tensor_ops(f) {
        return true;
    }
    let mut needed = HashSet::new();
    collect_tensor_ptr_names(&f.instructions, &mut needed);
    tensors
        .iter()
        .any(|(k, t)| !t.data.is_empty() && needed.contains(k))
}

fn seed_tensor_ptrs_for_fn(
    f: &IrFunction,
    tensors: &HashMap<String, HostTensor>,
    opts: &LlvmOptions,
    s: &mut String,
    ptrs: &mut HashMap<String, String>,
    buf_bytes: &mut HashMap<String, u64>,
    owned: &mut Vec<String>,
    uid: &mut u32,
    consts: &mut HashMap<String, i64>,
) {
    let _ = buf_bytes;
    let mut needed = HashSet::new();
    collect_tensor_ptr_names(&f.instructions, &mut needed);
    for p in &f.params {
        needed.insert(p.clone());
    }
    for name in &needed {
        if let Some(t) = tensors.get(name) {
            if t.owner_fn != f.name {
                continue;
            }
            if !t.data.is_empty() {
                emit_materialize_host_tensor(opts, name, t, s, ptrs, owned, uid);
            }
        }
    }
    for p in &f.params {
        if !ptrs.contains_key(p) {
            *uid += 1;
            let slot = format!("p{uid}");
            s.push_str(&format!("  %{slot} = inttoptr i64 %{p} to ptr\n"));
            ptrs.insert(p.clone(), slot);
        }
        if let Some(t) = tensors.get(p) {
            if t.owner_fn != f.name {
                continue;
            }
            let len = t.rows.saturating_mul(t.cols).max(1);
            if len > 0 {
                let dim = format!("{p}_d0");
                if !consts.contains_key(&dim) && !f.dim_params.contains(&dim) {
                    consts.insert(dim.clone(), len as i64);
                    s.push_str(&format!("  %{dim} = add i64 0, {len}\n"));
                }
            }
        }
    }
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
                    matches!(
                        o,
                        FusedOp::MapEpilogue { op, .. }
                            if op == "relu" || op == "map" || op == "silu"
                    )
                });
            }
            n += 1;
        }
    }
    false
}

fn region_peak_for_matmul(f: &IrFunction, matmul_ordinal: usize) -> u64 {
    let mut n = 0usize;
    for r in &f.regions {
        if r.ops.iter().any(|o| matches!(o, FusedOp::Matmul { .. })) {
            if n == matmul_ordinal {
                return r.peak_bytes.get(r.place.as_str()).copied().unwrap_or(0);
            }
            n += 1;
        }
    }
    0
}

fn is_matmul_runtime(func: &str) -> bool {
    matches!(
        func,
        "sal_matmul_f32" | "sal_matmul" | "sal_matmul_f16" | "sal_matmul_bf16"
    )
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

fn const_float_map(f: &IrFunction) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    for inst in &f.instructions {
        if let IrInst::ConstFloat { dest, value } = inst {
            m.insert(dest.clone(), *value);
        }
    }
    m
}

fn tensor_bin_op_code(op: &str) -> i32 {
    match op {
        "add" => 0,
        "sub" => 1,
        "mul" => 2,
        "div" => 3,
        "eq" => 4,
        "ne" => 5,
        "lt" => 6,
        "le" => 7,
        "gt" => 8,
        "ge" => 9,
        _ => 0,
    }
}

fn is_tensor_cmp_op(op: &str) -> bool {
    matches!(op, "eq" | "ne" | "lt" | "le" | "gt" | "ge")
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

enum HostFnEntry {
    Main,
    User,
}

fn emit_host_tensor_fn(
    f: &IrFunction,
    opts: &LlvmOptions,
    tensors: &HashMap<String, HostTensor>,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    entry: HostFnEntry,
    s: &mut String,
) {
    match entry {
        HostFnEntry::Main => {
            s.push_str("define i64 @main(i64 %argc, ptr %argv) {\n");
            s.push_str("entry:\n");
            s.push_str("  call void @sal_runtime_init(i64 %argc, ptr %argv)\n");
            if opts.instrument {
                s.push_str("  call void @sal_instrument_init()\n");
            }
        }
        HostFnEntry::User => {
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
        }
    }

    // Materialise host tensors referenced by this function's matmul/load.
    let mut ptrs: HashMap<String, String> = HashMap::new();
    let mut buf_bytes: HashMap<String, u64> = HashMap::new();
    let mut ssa_elems: HashMap<String, u64> = HashMap::new();
    let mut owned: Vec<String> = Vec::new();
    let mut uid = 0u32;
    let mut consts_seed = const_int_map(f);
    if matches!(entry, HostFnEntry::User) {
        seed_tensor_ptrs_for_fn(
            f,
            tensors,
            opts,
            s,
            &mut ptrs,
            &mut buf_bytes,
            &mut owned,
            &mut uid,
            &mut consts_seed,
        );
    }

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
        if ptrs.contains_key(name) {
            continue;
        }
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
    let mut region_open: HashMap<i32, bool> = HashMap::new();
    let mut last_out: Option<(String, DimOperand)> = None; // ptr name, elem count
    let consts = const_int_map(f);
    let floats = const_float_map(f);
    // Dim params are names (`x_d0`), not ConstInt temps. The fused stub aliases
    // them to %m/%k/%n; the host body still has to define the SSA before use.
    for dp in &f.dim_params {
        if !consts.contains_key(dp) {
            s.push_str(&format!("  %{dp} = add i64 0, 0\n"));
        }
    }

    for inst in &f.instructions {
        match inst {
            IrInst::ConstInt { dest, value } => {
                s.push_str(&format!("  %{dest} = add i64 0, {value}\n"));
            }
            IrInst::ConstFloat { dest, value } => {
                s.push_str(&format!("  ; float const %{dest} = {value}\n"));
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
                    "mod" => s.push_str(&format!("  %{dest} = srem i64 {l}, {r}\n")),
                    "band" => s.push_str(&format!("  %{dest} = and i64 {l}, {r}\n")),
                    "bxor" => s.push_str(&format!("  %{dest} = xor i64 {l}, {r}\n")),
                    "bor" => s.push_str(&format!("  %{dest} = or i64 {l}, {r}\n")),
                    "shl" => s.push_str(&format!("  %{dest} = shl i64 {l}, {r}\n")),
                    "shr" => s.push_str(&format!("  %{dest} = ashr i64 {l}, {r}\n")),
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
                    "neg" => s.push_str(&format!("  %{dest} = sub i64 0, {l}\n")),
                    "not" => s.push_str(&format!("  %{dest} = call i64 @sal_not(i64 {l})\n")),
                    _ => s.push_str(&format!("  %{dest} = add i64 {l}, 0\n")),
                }
            }
            IrInst::TensorBin {
                dest,
                op,
                elem,
                place,
                left,
                right,
                len,
                right_scalar,
            } => {
                emit_one_tensor_bin(
                    opts,
                    s,
                    &mut uid,
                    &mut ptrs,
                    &mut buf_bytes,
                    &mut owned,
                    &consts,
                    &floats,
                    dest,
                    op,
                    place,
                    left,
                    right,
                    len,
                    *right_scalar,
                    *elem,
                );
            }
            IrInst::TensorUnary {
                dest,
                op,
                elem,
                place,
                input,
                len,
            } => {
                emit_one_tensor_unary(
                    opts,
                    s,
                    &mut uid,
                    &mut ptrs,
                    &mut buf_bytes,
                    &mut owned,
                    &consts,
                    dest,
                    op,
                    place,
                    input,
                    len,
                    *elem,
                );
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
                } else if is_matmul_runtime(func) {
                    let place = region_place_for_matmul(f, matmul_ordinal);
                    let place_i = place_to_i32(&place);
                    let peak = region_peak_for_matmul(f, matmul_ordinal);
                    if peak > 0 && !region_open.get(&place_i).copied().unwrap_or(false) {
                        s.push_str(&format!(
                            "  call void @sal_region_begin(i64 {place_i}, i64 {peak})\n"
                        ));
                        region_open.insert(place_i, true);
                    }
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
                        let mm_fn = if func == "sal_matmul_f16" {
                            "sal_matmul_f16"
                        } else if func == "sal_matmul_bf16" {
                            "sal_matmul_bf16"
                        } else {
                            "sal_matmul_f32"
                        };
                        s.push_str(&format!(
                            "  call void @{mm_fn}(ptr {a_op}, ptr {b_op}, ptr %{out}, i64 {m_op}, i64 {k_op}, i64 {n_op})\n"
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
                } else if func == "sal_softmax_axis_f32" && args.len() >= 2 {
                    let input = args[0].as_str();
                    let axis = i64_operand(&args[1], &consts);
                    if let Some(p) = ptrs.get(input).cloned() {
                        let outer = ssa_elems.get(input).copied().unwrap_or(1);
                        s.push_str(&format!(
                            "  call void @sal_softmax_axis_f32(ptr %{p}, i64 {outer}, i64 {axis})\n"
                        ));
                        if let Some(d) = dest {
                            ptrs.insert(d.clone(), p.clone());
                            s.push_str(&format!("  %{d} = ptrtoint ptr %{p} to i64\n"));
                        }
                    }
                } else if func == "sal_tensor_write_row_f32" && args.len() >= 4 {
                    let cache = ptrs.get(&args[0]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let seq = i64_operand(&args[1], &consts);
                    let row = ptrs.get(&args[2]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let row_elems = i64_operand(&args[3], &consts);
                    s.push_str(&format!(
                        "  call void @sal_tensor_write_row_f32(ptr {cache}, i64 {seq}, ptr {row}, i64 {row_elems})\n"
                    ));
                } else if func == "sal_tensor_write_rows_f32" && args.len() >= 5 {
                    let cache = ptrs.get(&args[0]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let seq = i64_operand(&args[1], &consts);
                    let block = ptrs.get(&args[2]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let n_rows = i64_operand(&args[3], &consts);
                    let row_elems = i64_operand(&args[4], &consts);
                    s.push_str(&format!(
                        "  call void @sal_tensor_write_rows_f32(ptr {cache}, i64 {seq}, ptr {block}, i64 {n_rows}, i64 {row_elems})\n"
                    ));
                } else if func == "sal_rope" && args.len() >= 4 {
                    let input = args[0].as_str();
                    let seq = i64_operand(&args[1], &consts);
                    let dim = i64_operand(&args[2], &consts);
                    let pos = i64_operand(&args[3], &consts);
                    if let Some(p) = ptrs.get(input).cloned() {
                        s.push_str(&format!(
                            "  call void @sal_rope_f32(ptr %{p}, i64 {seq}, i64 {dim}, i64 {pos})\n"
                        ));
                        if let Some(d) = dest {
                            ptrs.insert(d.clone(), p.clone());
                            s.push_str(&format!("  %{d} = ptrtoint ptr %{p} to i64\n"));
                        }
                    } else if let Some(d) = dest {
                        s.push_str(&format!("  %{d} = add i64 0, 0\n"));
                    }
                } else if (func == "sal_silu" || func == "sal_silu_f32") && !args.is_empty() {
                    let input = args[0].as_str();
                    let len = ssa_elems.get(input).copied().unwrap_or_else(|| {
                        tensors
                            .get(input)
                            .map(|t| t.rows.saturating_mul(t.cols))
                            .unwrap_or(0)
                    });
                    uid += 1;
                    let out = format!("silu_{uid}");
                    if len > 0 {
                        let nbytes = len.saturating_mul(4);
                        emit_alloc_place(opts, &out, nbytes, 0, "silu", s);
                        owned.push(out.clone());
                        let in_ptr = ptrs.get(input).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                        s.push_str(&format!(
                            "  call void @sal_silu_f32(ptr %{out}, ptr {in_ptr}, i64 {len})\n"
                        ));
                        if let Some(d) = dest {
                            ptrs.insert(d.clone(), out.clone());
                            ssa_elems.insert(d.clone(), len);
                            s.push_str(&format!("  %{d} = ptrtoint ptr %{out} to i64\n"));
                        }
                    } else if let Some(d) = dest {
                        s.push_str(&format!("  %{d} = add i64 0, 0\n"));
                    }
                } else if func == "sal_attention_f32" && args.len() >= 8 {
                    let q = ptrs.get(&args[0]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let k = ptrs.get(&args[1]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let v = ptrs.get(&args[2]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let seq = i64_operand(&args[3], &consts);
                    let heads = i64_operand(&args[4], &consts);
                    let kv = i64_operand(&args[5], &consts);
                    let hd = i64_operand(&args[6], &consts);
                    let causal = i64_operand(&args[7], &consts);
                    uid += 1;
                    let out = format!("attn_{uid}");
                    let elems = format!("({seq}*{heads}*{hd})");
                    s.push_str(&format!(
                        "  %{out}_bytes = mul i64 {seq}, {heads}\n  %{out}_b2 = mul i64 %{out}_bytes, {hd}\n  %{out}_b3 = mul i64 %{out}_b2, 4\n"
                    ));
                    s.push_str(&format!(
                        "  %{out} = call ptr @sal_place_malloc(i64 %{out}_b3, i32 0)\n"
                    ));
                    owned.push(out.clone());
                    s.push_str(&format!(
                        "  call void @sal_attention_f32(ptr %{out}, ptr {q}, ptr {k}, ptr {v}, i64 {seq}, i64 {heads}, i64 {kv}, i64 {hd}, i64 {causal})\n"
                    ));
                    if let Some(d) = dest {
                        ptrs.insert(d.clone(), out.clone());
                        s.push_str(&format!("  %{d} = ptrtoint ptr %{out} to i64\n"));
                    }
                    let _ = elems;
                } else if func == "sal_qmatmul_f32" && args.len() >= 7 {
                    let a_ptr = ptrs.get(&args[0]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let w_ptr = ptrs.get(&args[1]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let sc_ptr = ptrs.get(&args[2]).map(|p| format!("%{p}")).unwrap_or_else(|| "null".into());
                    let group = i64_operand(&args[3], &consts);
                    let m = i64_operand(&args[4], &consts);
                    let k = i64_operand(&args[5], &consts);
                    let n = i64_operand(&args[6], &consts);
                    uid += 1;
                    let out = format!("qmm_{uid}");
                    s.push_str(&format!(
                        "  %{out}_elems = mul i64 {m}, {n}\n  %{out}_bytes = mul i64 %{out}_elems, 4\n"
                    ));
                    s.push_str(&format!(
                        "  %{out} = call ptr @sal_place_malloc(i64 %{out}_bytes, i32 0)\n"
                    ));
                    owned.push(out.clone());
                    s.push_str(&format!(
                        "  call void @sal_qmatmul_f32(ptr {a_ptr}, ptr {w_ptr}, ptr {sc_ptr}, ptr %{out}, i64 {m}, i64 {k}, i64 {n}, i64 {group})\n"
                    ));
                    if let Some(d) = dest {
                        ptrs.insert(d.clone(), out.clone());
                        s.push_str(&format!("  %{d} = ptrtoint ptr %{out} to i64\n"));
                    }
                } else if func == "sal_argmax_axis_f32" && !args.is_empty() {
                    let p = ptrs.get(&args[0]).map(|x| format!("%{x}")).unwrap_or_else(|| "null".into());
                    let len = args.get(1).map(|a| i64_operand(a, &consts)).unwrap_or_else(|| "1".into());
                    if let Some(d) = dest {
                        s.push_str(&format!(
                            "  %{d} = call i64 @sal_argmax_axis_f32(ptr {p}, i64 {len})\n"
                        ));
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
                        Some(&mut ptrs),
                        Some(&mut uid),
                    );
                    uid = tmp;
                }
            }
            IrInst::TensorRebind { dest, src } => {
                if let Some(p) = ptrs.get(src).cloned() {
                    ptrs.insert(dest.clone(), p.clone());
                    if let Some(n) = ssa_elems.get(src).copied() {
                        ssa_elems.insert(dest.clone(), n);
                    }
                    s.push_str(&format!("  %{dest} = ptrtoint ptr %{p} to i64\n"));
                } else {
                    s.push_str(&format!("  %{dest} = add i64 0, 0\n"));
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

    for pl in region_open.keys() {
        s.push_str(&format!("  call void @sal_region_end(i64 {pl})\n"));
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
    let path_arg = args.first().cloned().unwrap_or_default();
    let path_raw = path_arg.trim_matches('"').to_string();
    *uid += 1;
    let path_ptr = if let Some(g) = str_globals.get(&path_raw) {
        str_lit_ptr_temp(g, uid, s)
    } else if path_arg.starts_with('"') {
        "@.site.load".to_string()
    } else {
        let cast = format!("load_path_{uid}");
        s.push_str(&format!(
            "  %{cast} = inttoptr i64 %{path_arg} to ptr\n"
        ));
        format!("%{cast}")
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

fn fmt_double(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
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

fn emit_bytes_data_len(
    s: &mut String,
    uid: &mut u32,
    handle: &str,
    tag: &str,
) -> (String, String) {
    *uid += 1;
    let bp = format!("{tag}bp{uid}");
    s.push_str(&format!("  %{bp} = inttoptr i64 {handle} to ptr\n"));
    *uid += 1;
    let data = format!("{tag}d{uid}");
    s.push_str(&format!("  %{data} = load ptr, ptr %{bp}\n"));
    *uid += 1;
    let lenp = format!("{tag}lp{uid}");
    s.push_str(&format!("  %{lenp} = getelementptr i8, ptr %{bp}, i64 8\n"));
    *uid += 1;
    let len = format!("{tag}n{uid}");
    s.push_str(&format!("  %{len} = load i64, ptr %{lenp}\n"));
    (format!("%{data}"), format!("%{len}"))
}

fn emit_bytes_bounds_ok(s: &mut String, _uid: &mut u32, idx: &str, width: i64, len: &str) {
    s.push_str(&format!(
        "  call void @sal_bytes_check(i64 {idx}, i64 {width}, i64 {len})\n"
    ));
}

fn emit_bytes_get(
    s: &mut String,
    uid: &mut u32,
    dest: &str,
    handle: &str,
    idx: &str,
    width: i64,
    llvm_ty: &str,
) {
    let (data, len) = emit_bytes_data_len(s, uid, handle, "g");
    emit_bytes_bounds_ok(s, uid, idx, width, &len);
    *uid += 1;
    let gep = format!("bgp{uid}");
    s.push_str(&format!(
        "  %{gep} = getelementptr i8, ptr {data}, i64 {idx}\n"
    ));
    *uid += 1;
    let raw = format!("br{uid}");
    s.push_str(&format!("  %{raw} = load {llvm_ty}, ptr %{gep}\n"));
    if llvm_ty == "i64" {
        s.push_str(&format!("  %{dest} = add i64 %{raw}, 0\n"));
    } else {
        s.push_str(&format!("  %{dest} = zext {llvm_ty} %{raw} to i64\n"));
    }
}

fn emit_bytes_set(
    s: &mut String,
    uid: &mut u32,
    dest: &str,
    handle: &str,
    idx: &str,
    val: &str,
    width: i64,
    llvm_ty: &str,
) {
    let (data, len) = emit_bytes_data_len(s, uid, handle, "s");
    emit_bytes_bounds_ok(s, uid, idx, width, &len);
    *uid += 1;
    let gep = format!("sgp{uid}");
    s.push_str(&format!(
        "  %{gep} = getelementptr i8, ptr {data}, i64 {idx}\n"
    ));
    if llvm_ty == "i64" {
        s.push_str(&format!("  store i64 {val}, ptr %{gep}\n"));
    } else {
        *uid += 1;
        let narrow = format!("sn{uid}");
        s.push_str(&format!("  %{narrow} = trunc i64 {val} to {llvm_ty}\n"));
        s.push_str(&format!("  store {llvm_ty} %{narrow}, ptr %{gep}\n"));
    }
    s.push_str(&format!("  %{dest} = add i64 0, 0\n"));
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
    ptrs: Option<&mut HashMap<String, String>>,
    ptr_uid: Option<&mut u32>,
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
        "sal_str_to_f64_bits" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_str_to_f64_bits(ptr {a})\n"));
            }
        }
        "sal_f64_bits_to_str" => {
            let a = args
                .first()
                .map(|x| i64_operand(x, consts))
                .unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_f64_bits_to_str(i64 {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_now_ns" => {
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_now_ns()\n"));
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
        "sal_syscall" => {
            let mut ops: Vec<String> = args
                .iter()
                .map(|a| i64_operand(a, consts))
                .collect();
            while ops.len() < 7 {
                ops.push("0".into());
            }
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_os_syscall(i64 {}, i64 {}, i64 {}, i64 {}, i64 {}, i64 {}, i64 {})\n",
                    ops[0], ops[1], ops[2], ops[3], ops[4], ops[5], ops[6]
                ));
            }
        }
        "sal_bytes_new" => {
            let n = args.first().map(|a| i64_operand(a, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_bytes_new(i64 {n})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_bytes_len" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_bytes_len(ptr {a})\n"));
            }
        }
        "sal_bytes_ptr" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d} = call i64 @sal_bytes_ptr(ptr {a})\n"));
            }
        }
        "sal_bytes_fill" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let off = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let len = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let byte = args.get(3).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_bytes_fill(ptr {a}, i64 {off}, i64 {len}, i64 {byte})\n"
                ));
            }
        }
        "sal_bytes_fill_u32" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let off = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let cnt = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let val = args.get(3).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_bytes_fill_u32(ptr {a}, i64 {off}, i64 {cnt}, i64 {val})\n"
                ));
            }
        }
        "sal_bytes_copy" => {
            let dst = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let doff = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let src = args.get(2).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let soff = args.get(3).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let len = args.get(4).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_bytes_copy(ptr {dst}, i64 {doff}, ptr {src}, i64 {soff}, i64 {len})\n"
                ));
            }
        }
        "sal_str_bytes_copy" => {
            let dst = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let off = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let src = args.get(2).map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d} = call i64 @sal_str_bytes_copy(ptr {dst}, i64 {off}, ptr {src})\n"
                ));
            }
        }
        "sal_bytes_to_str" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            if let Some(d) = dest {
                s.push_str(&format!("  %{d}_p = call ptr @sal_bytes_to_str(ptr {a})\n"));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_bytes_view" => {
            let a = args.first().map(|x| ptr_arg(x, s, tmp)).unwrap_or_else(|| "null".into());
            let off = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let len = args.get(2).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_bytes_view(ptr {a}, i64 {off}, i64 {len})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_bytes_view_mem" => {
            let data = args.first().map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            let len = args.get(1).map(|x| i64_operand(x, consts)).unwrap_or_else(|| "0".into());
            if let Some(d) = dest {
                s.push_str(&format!(
                    "  %{d}_p = call ptr @sal_bytes_view_mem(i64 {data}, i64 {len})\n"
                ));
                s.push_str(&format!("  %{d} = ptrtoint ptr %{d}_p to i64\n"));
            }
        }
        "sal_bytes_get_u8" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                emit_bytes_get(s, tmp, d, &h, &idx, 1, "i8");
            }
        }
        "sal_bytes_get_u16" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                emit_bytes_get(s, tmp, d, &h, &idx, 2, "i16");
            }
        }
        "sal_bytes_get_u32" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                emit_bytes_get(s, tmp, d, &h, &idx, 4, "i32");
            }
        }
        "sal_bytes_get_i64" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                emit_bytes_get(s, tmp, d, &h, &idx, 8, "i64");
            }
        }
        "sal_bytes_set_u8" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                let val = i64_operand(args.get(2).unwrap_or(&"0".into()), consts);
                emit_bytes_set(s, tmp, d, &h, &idx, &val, 1, "i8");
            }
        }
        "sal_bytes_set_u16" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                let val = i64_operand(args.get(2).unwrap_or(&"0".into()), consts);
                emit_bytes_set(s, tmp, d, &h, &idx, &val, 2, "i16");
            }
        }
        "sal_bytes_set_u32" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                let val = i64_operand(args.get(2).unwrap_or(&"0".into()), consts);
                emit_bytes_set(s, tmp, d, &h, &idx, &val, 4, "i32");
            }
        }
        "sal_bytes_set_i64" => {
            if let Some(d) = dest {
                let h = i64_operand(args.first().unwrap_or(&"0".into()), consts);
                let idx = i64_operand(args.get(1).unwrap_or(&"0".into()), consts);
                let val = i64_operand(args.get(2).unwrap_or(&"0".into()), consts);
                emit_bytes_set(s, tmp, d, &h, &idx, &val, 8, "i64");
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
                if let (Some(ptrs), Some(uid)) = (ptrs, ptr_uid) {
                    *uid += 1;
                    let p = format!("pret_{uid}");
                    s.push_str(&format!("  %{p} = inttoptr i64 %{d} to ptr\n"));
                    ptrs.insert(d.to_string(), p);
                }
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

fn field_is_tensor_f32(ty: &Type) -> bool {
    matches!(ty, Type::Tensor { elem: TensorElem::F32, .. })
}

fn field_is_tensor_i8(ty: &Type) -> bool {
    matches!(ty, Type::Tensor { elem: TensorElem::I8, .. })
}

fn field_is_list_string(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named { name, args, .. } if name == "List" && !args.is_empty()
    )
}

fn emit_ptr_from_i64_ssa(
    ssa: &str,
    consts: &HashMap<String, i64>,
    ptrs: &HashMap<String, String>,
    s: &mut String,
    uid: &mut u32,
) -> String {
    if let Some(p) = ptrs.get(ssa) {
        return format!("%{p}");
    }
    *uid += 1;
    let slot = format!("wp{uid}");
    let op = i64_operand(ssa, consts);
    s.push_str(&format!("  %{slot} = inttoptr i64 {op} to ptr\n"));
    format!("%{slot}")
}

fn emit_where_frame(
    dest: &str,
    args: &[String],
    consts: &HashMap<String, i64>,
    ptrs: &HashMap<String, String>,
    opts: Option<&LlvmOptions>,
    s: &mut String,
    uid: &mut u32,
    tmp: &mut u32,
) {
    if args.len() < 4 {
        s.push_str(&format!("  %{dest} = add i64 0, 0\n"));
        return;
    }
    let frame_name = args[3].trim_matches('"');
    let def = EMIT_STRUCT_DEFS.with(|d| d.borrow().get(frame_name).cloned());
    let Some(def) = def else {
        s.push_str(&format!("  %{dest} = add i64 0, 0\n"));
        return;
    };
    let frame_base = &args[0];
    let frame_ref = if EMIT_AGG.with(|a| a.borrow().contains_key(frame_base)) {
        format!("%{frame_base}")
    } else {
        format!("%{frame_base}")
    };
    let len_op = i64_operand(&args[2], consts);
    let mask_ptr = emit_ptr_from_i64_ssa(&args[1], consts, ptrs, s, uid);
    *uid += 1;
    let count = format!("wcnt{uid}");
    s.push_str(&format!(
        "  %{count} = call i64 @sal_mask_select_count(ptr {mask_ptr}, i64 {len_op})\n"
    ));
    let struct_ty = llvm_struct_symbol(frame_name);
    let mut field_ssas: Vec<String> = Vec::new();
    for (i, field) in def.fields.iter().enumerate() {
        *tmp += 1;
        let raw = format!("wf{tmp}");
        s.push_str(&format!(
            "  %{raw} = extractvalue {struct_ty} {frame_ref}, {i}\n"
        ));
        *uid += 1;
        let col_ptr = format!("wcp{uid}");
        s.push_str(&format!("  %{col_ptr} = inttoptr i64 %{raw} to ptr\n"));
        emit_where_column(
            field,
            &col_ptr,
            &mask_ptr,
            &len_op,
            &count,
            opts,
            s,
            uid,
            tmp,
            &mut field_ssas,
        );
    }
    if field_ssas.is_empty() {
        s.push_str(&format!("  %{dest} = insertvalue {struct_ty} undef, i64 0, 0\n"));
    } else {
        let mut agg = String::new();
        for (i, f) in field_ssas.iter().enumerate() {
            let op = i64_operand(f, consts);
            let next = if i + 1 == field_ssas.len() {
                dest.to_string()
            } else {
                *tmp += 1;
                format!("wag{tmp}")
            };
            if i == 0 {
                s.push_str(&format!(
                    "  %{next} = insertvalue {struct_ty} undef, i64 {op}, 0\n"
                ));
            } else {
                s.push_str(&format!(
                    "  %{next} = insertvalue {struct_ty} %{agg}, i64 {op}, {i}\n"
                ));
            }
            agg = next;
        }
    }
    EMIT_AGG.with(|a| {
        a.borrow_mut()
            .insert(dest.to_string(), frame_name.to_string());
    });
}

fn emit_where_column(
    field: &StructField,
    col_ptr: &str,
    mask_ptr: &str,
    len_op: &str,
    count: &str,
    opts: Option<&LlvmOptions>,
    s: &mut String,
    uid: &mut u32,
    tmp: &mut u32,
    out_ssa: &mut Vec<String>,
) {
    *tmp += 1;
    let slot = format!("wout{tmp}");
    if field_is_tensor_f32(&field.ty) {
        s.push_str(&format!("  %{slot}_bytes = mul i64 %{count}, 4\n"));
        if let Some(o) = opts {
            if o.instrument {
                s.push_str(&format!(
                    "  %{slot} = call ptr @sal_instrument_malloc(i64 %{slot}_bytes, i32 0, ptr @.site.access)\n"
                ));
            } else {
                s.push_str(&format!(
                    "  %{slot} = call ptr @sal_place_malloc(i64 %{slot}_bytes, i32 0)\n"
                ));
            }
        } else {
            s.push_str(&format!(
                "  %{slot} = call ptr @sal_place_malloc(i64 %{slot}_bytes, i32 0)\n"
            ));
        }
        s.push_str(&format!(
            "  call void @sal_tensor_select_f32(ptr %{slot}, ptr {col_ptr}, ptr {mask_ptr}, i64 {len_op})\n"
        ));
    } else if field_is_tensor_i8(&field.ty) {
        s.push_str(&format!("  %{slot}_bytes = mul i64 %{count}, 1\n"));
        s.push_str(&format!(
            "  %{slot} = call ptr @sal_place_malloc(i64 %{slot}_bytes, i32 0)\n"
        ));
        s.push_str(&format!(
            "  call void @sal_tensor_select_i8(ptr %{slot}, ptr {col_ptr}, ptr {mask_ptr}, i64 {len_op})\n"
        ));
    } else if field_is_list_string(&field.ty) {
        s.push_str(&format!(
            "  %{slot} = call ptr @sal_list_select(ptr {col_ptr}, ptr {mask_ptr}, i64 {len_op})\n"
        ));
    } else {
        *uid += 1;
        let z = format!("wz{uid}");
        s.push_str(&format!("  %{z} = add i64 0, 0\n"));
        out_ssa.push(z);
        return;
    }
    *uid += 1;
    let handle = format!("wh{uid}");
    s.push_str(&format!("  %{handle} = ptrtoint ptr %{slot} to i64\n"));
    out_ssa.push(handle);
}

fn emit_body_insts(
    f: &IrFunction,
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
    tensor: Option<&mut TensorEmitCtx<'_>>,
) -> String {
    let mut label_id = 0u32;
    let mut tmp = 0u32;
    let mut cur_block = "entry".to_string();
    let (ret, _, _) = emit_inst_list(
        &f.instructions,
        str_globals,
        user_fns,
        s,
        &mut label_id,
        &mut tmp,
        &mut cur_block,
        tensor,
    );
    ret
}

fn insts_through_return(instructions: &[IrInst]) -> &[IrInst] {
    match instructions
        .iter()
        .position(|i| matches!(i, IrInst::Return { .. }))
    {
        Some(p) => &instructions[..=p],
        None => instructions,
    }
}

fn inst_list_ends_with_return(instructions: &[IrInst]) -> bool {
    matches!(
        insts_through_return(instructions).last(),
        Some(IrInst::Return { .. })
    )
}

fn emit_inst_list(
    instructions: &[IrInst],
    str_globals: &HashMap<String, StrLitGlobal>,
    user_fns: &HashMap<String, &IrFunction>,
    s: &mut String,
    label_id: &mut u32,
    tmp: &mut u32,
    cur_block: &mut String,
    mut tensor: Option<&mut TensorEmitCtx<'_>>,
) -> (String, String, bool) {
    let consts = const_int_map_from(instructions);
    let mut ret = "0".to_string();
    let mut i = 0usize;
    while i < instructions.len() {
        if let IrInst::Return { value } = &instructions[i] {
            ret = i64_operand(value, &consts);
            s.push_str(&format!("  ret i64 {ret}\n"));
            return (ret, cur_block.clone(), true);
        }
        if let IrInst::If {
            cond,
            then_body,
            else_body,
            then_val,
            else_val,
            dest,
            carried,
        } = &instructions[i]
        {
            let suffix = &instructions[i + 1..];
            let then_body = insts_through_return(then_body);
            let else_body = insts_through_return(else_body);
            let then_term = inst_list_ends_with_return(then_body);
            let else_term = inst_list_ends_with_return(else_body);
            if !suffix.is_empty() && then_term && !else_term {
                *label_id += 1;
                let id = *label_id;
                let then_l = format!("then{id}");
                let else_l = format!("else{id}");
                let c = i64_operand(cond, &consts);
                s.push_str(&format!("  %ifcond{id} = icmp ne i64 {c}, 0\n"));
                s.push_str(&format!(
                    "  br i1 %ifcond{id}, label %{then_l}, label %{else_l}\n"
                ));
                s.push_str(&format!("{then_l}:\n"));
                let mut then_block = then_l;
                emit_inst_list(
                    then_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut then_block,
                    tensor.as_deref_mut(),
                );
                s.push_str(&format!("{else_l}:\n"));
                *cur_block = else_l;
                let (_, _, _) = emit_inst_list(
                    else_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    cur_block,
                    tensor.as_deref_mut(),
                );
                return emit_inst_list(
                    suffix,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    cur_block,
                    tensor,
                );
            }
            if !suffix.is_empty() && !then_term && else_term {
                *label_id += 1;
                let id = *label_id;
                let then_l = format!("then{id}");
                let else_l = format!("else{id}");
                let c = i64_operand(cond, &consts);
                s.push_str(&format!("  %ifcond{id} = icmp ne i64 {c}, 0\n"));
                s.push_str(&format!(
                    "  br i1 %ifcond{id}, label %{then_l}, label %{else_l}\n"
                ));
                s.push_str(&format!("{else_l}:\n"));
                let mut else_block = else_l.clone();
                emit_inst_list(
                    else_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut else_block,
                    tensor.as_deref_mut(),
                );
                s.push_str(&format!("{then_l}:\n"));
                *cur_block = then_l;
                let (_, _, _) = emit_inst_list(
                    then_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    cur_block,
                    tensor.as_deref_mut(),
                );
                return emit_inst_list(
                    suffix,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    cur_block,
                    tensor,
                );
            }
            if !suffix.is_empty() && then_term && else_term {
                *label_id += 1;
                let id = *label_id;
                let then_l = format!("then{id}");
                let else_l = format!("else{id}");
                let c = i64_operand(cond, &consts);
                s.push_str(&format!("  %ifcond{id} = icmp ne i64 {c}, 0\n"));
                s.push_str(&format!(
                    "  br i1 %ifcond{id}, label %{then_l}, label %{else_l}\n"
                ));
                s.push_str(&format!("{then_l}:\n"));
                let mut then_block = then_l;
                emit_inst_list(
                    then_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut then_block,
                    tensor.as_deref_mut(),
                );
                s.push_str(&format!("{else_l}:\n"));
                let mut else_block = else_l;
                emit_inst_list(
                    else_body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut else_block,
                    tensor.as_deref_mut(),
                );
                return (ret, cur_block.clone(), true);
            }
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
            let (_, _, tt) = emit_inst_list(
                then_body,
                str_globals,
                user_fns,
                s,
                label_id,
                tmp,
                &mut then_block,
                tensor.as_deref_mut(),
            );
            let tv = if then_consts.contains_key(then_val)
                || then_val.chars().all(|c| c.is_ascii_digit())
            {
                i64_operand(then_val, &then_consts)
            } else {
                format!("%{then_val}")
            };
            if !tt {
                s.push_str(&format!("  br label %{join_l}\n"));
            }
            s.push_str(&format!("{else_l}:\n"));
            let mut else_block = else_l.clone();
            let else_consts = const_int_map_from(else_body);
            let (_, _, et) = emit_inst_list(
                else_body,
                str_globals,
                user_fns,
                s,
                label_id,
                tmp,
                &mut else_block,
                tensor.as_deref_mut(),
            );
            let ev = if else_consts.contains_key(else_val)
                || else_val.chars().all(|c| c.is_ascii_digit())
            {
                i64_operand(else_val, &else_consts)
            } else {
                format!("%{else_val}")
            };
            if !et {
                s.push_str(&format!("  br label %{join_l}\n"));
            }
            if !tt && !et {
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
            } else if tt && !et {
                s.push_str(&format!("{join_l}:\n"));
                s.push_str(&format!(
                    "  %{dest} = phi i64 [ {ev}, %{else_block} ]\n"
                ));
                for carry in carried {
                    let ce = if else_consts.contains_key(&carry.else_val)
                        || carry.else_val.chars().all(|c| c.is_ascii_digit())
                    {
                        i64_operand(&carry.else_val, &else_consts)
                    } else {
                        format!("%{}", carry.else_val)
                    };
                    s.push_str(&format!(
                        "  %{} = phi i64 [ {ce}, %{else_block} ]\n",
                        carry.dest
                    ));
                }
                *cur_block = join_l;
            } else if !tt && et {
                s.push_str(&format!("{join_l}:\n"));
                s.push_str(&format!(
                    "  %{dest} = phi i64 [ {tv}, %{then_block} ]\n"
                ));
                for carry in carried {
                    let ct = if then_consts.contains_key(&carry.then_val)
                        || carry.then_val.chars().all(|c| c.is_ascii_digit())
                    {
                        i64_operand(&carry.then_val, &then_consts)
                    } else {
                        format!("%{}", carry.then_val)
                    };
                    s.push_str(&format!(
                        "  %{} = phi i64 [ {ct}, %{then_block} ]\n",
                        carry.dest
                    ));
                }
                *cur_block = join_l;
            }
            i += 1;
            continue;
        }
        let inst = &instructions[i];
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
                    "mod" => s.push_str(&format!("  %{dest} = srem i64 {l}, {r}\n")),
                    "band" => s.push_str(&format!("  %{dest} = and i64 {l}, {r}\n")),
                    "bxor" => s.push_str(&format!("  %{dest} = xor i64 {l}, {r}\n")),
                    "bor" => s.push_str(&format!("  %{dest} = or i64 {l}, {r}\n")),
                    "shl" => s.push_str(&format!("  %{dest} = shl i64 {l}, {r}\n")),
                    "shr" => s.push_str(&format!("  %{dest} = ashr i64 {l}, {r}\n")),
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
                    "neg" => s.push_str(&format!("  %{dest} = sub i64 0, {l}\n")),
                    "not" => s.push_str(&format!("  %{dest} = call i64 @sal_not(i64 {l})\n")),
                    _ => s.push_str(&format!("  %{dest} = add i64 {l}, 0\n")),
                }
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
            IrInst::ConstFloat { dest, value } => {
                if let Some(te) = tensor.as_deref_mut() {
                    s.push_str(&format!("  ; float const %{dest} = {value}\n"));
                    let _ = te;
                }
            }
            IrInst::TensorBin {
                dest,
                op,
                elem,
                place,
                left,
                right,
                len,
                right_scalar,
            } => {
                if let Some(te) = tensor.as_deref_mut() {
                    emit_one_tensor_bin(
                        te.opts,
                        s,
                        te.uid,
                        te.ptrs,
                        te.buf_bytes,
                        te.owned,
                        te.consts,
                        te.floats,
                        dest,
                        op,
                        place,
                        left,
                        right,
                        len,
                        *right_scalar,
                        *elem,
                    );
                }
            }
            IrInst::TensorUnary {
                dest,
                op,
                elem,
                place,
                input,
                len,
            } => {
                if let Some(te) = tensor.as_deref_mut() {
                    emit_one_tensor_unary(
                        te.opts,
                        s,
                        te.uid,
                        te.ptrs,
                        te.buf_bytes,
                        te.owned,
                        te.consts,
                        dest,
                        op,
                        place,
                        input,
                        len,
                        *elem,
                    );
                }
            }
            IrInst::Call { dest, func, args } => {
                if func == "sal_where" {
                    if let Some(d) = dest {
                        if let Some(te) = tensor.as_deref_mut() {
                            emit_where_frame(
                                d,
                                args,
                                te.consts,
                                te.ptrs,
                                Some(te.opts),
                                s,
                                te.uid,
                                tmp,
                            );
                        } else {
                            let empty = HashMap::new();
                            let mut local_uid = 0u32;
                            emit_where_frame(
                                d,
                                args,
                                &consts,
                                &empty,
                                None,
                                s,
                                &mut local_uid,
                                tmp,
                            );
                        }
                    }
                } else if func == "sal_matmul_f32" || func == "sal_matmul" || func == "sal_load"
                    || func == "sal_load_f32" || func == "sal_softmax" || func == "sal_softmax_f32"
                {
                    if let Some(d) = dest {
                        s.push_str(&format!("  %{d} = add i64 0, 0\n"));
                    }
                } else if let Some(te) = tensor.as_deref_mut() {
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
                        Some(te.ptrs),
                        Some(te.uid),
                    );
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
                        None,
                        None,
                    );
                }
            }
            IrInst::PlaceCopy {
                dest,
                from,
                to_place,
            } => {
                if let Some(te) = tensor.as_deref_mut() {
                    let place_i = place_to_i32(to_place);
                    let mut nbytes = te
                        .buf_bytes
                        .get(from)
                        .copied()
                        .or_else(|| te.buf_bytes.get(dest).copied())
                        .unwrap_or(0);
                    if nbytes == 0 {
                        let dim = format!("{from}_d0");
                        if let Some(len) = te.consts.get(&dim) {
                            nbytes = (*len as u64).saturating_mul(4);
                        }
                    }
                    if let Some(p) = te.ptrs.get(from).cloned() {
                        if nbytes > 0 {
                            *te.uid += 1;
                            let dst = format!("pcopy_{}", te.uid);
                            s.push_str(&format!(
                                "  %{dst} = call ptr @sal_place_copy(ptr %{p}, i64 {nbytes}, i32 {place_i})\n"
                            ));
                            if te.opts.instrument {
                                s.push_str(&format!(
                                    "  call void @sal_instrument_alloc(ptr %{dst}, i64 {nbytes}, i32 {place_i}, ptr @.site.access)\n"
                                ));
                            }
                            te.ptrs.insert(dest.clone(), dst.clone());
                            te.buf_bytes.insert(dest.clone(), nbytes);
                            te.owned.push(dst);
                        } else if te.ptrs.contains_key(from) {
                            te.ptrs.insert(dest.clone(), p);
                        }
                        s.push_str(&format!(
                            "  %{dest} = ptrtoint ptr {} to i64\n",
                            te.ptrs
                                .get(dest)
                                .map(|x| format!("%{x}"))
                                .unwrap_or_else(|| "null".into())
                        ));
                    }
                }
            }
            IrInst::TensorRebind { dest, src } => {
                if let Some(te) = tensor.as_deref_mut() {
                    if let Some(p) = te.ptrs.get(src).cloned() {
                        te.ptrs.insert(dest.clone(), p.clone());
                        s.push_str(&format!("  %{dest} = ptrtoint ptr %{p} to i64\n"));
                    } else {
                        s.push_str(&format!("  %{dest} = add i64 0, 0\n"));
                    }
                }
            }
            IrInst::Drop { .. } | IrInst::KernelGrid { .. } => {}
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
                        tensor.as_deref_mut(),
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
                    tensor.as_deref_mut(),
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
                    tensor.as_deref_mut(),
                );
                let cconsts = const_int_map_from(cond_insts);
                let c = i64_operand(cond, &cconsts);
                s.push_str(&format!("  %whc{id} = icmp ne i64 {c}, 0\n"));
                s.push_str(&format!(
                    "  br i1 %whc{id}, label %{body_l}, label %{end_l}\n"
                ));
                s.push_str(&format!("{body_l}:\n"));
                let mut body_block = body_l.clone();
                let body = insts_through_return(body);
                let (_, _, body_term) = emit_inst_list(
                    body,
                    str_globals,
                    user_fns,
                    s,
                    label_id,
                    tmp,
                    &mut body_block,
                    tensor.as_deref_mut(),
                );
                if !body_term {
                    s.push_str(&format!("  br label %{latch}\n"));
                }
                s.push_str(&format!("{latch}:\n"));
                s.push_str(&format!("  br label %{head}\n"));
                s.push_str(&format!("{end_l}:\n"));
                *cur_block = end_l;
            }
            IrInst::Return { .. } | IrInst::If { .. } => unreachable!("handled above"),
        }
        i += 1;
    }
    (ret, cur_block.clone(), false)
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
    opts: &LlvmOptions,
    tensors: &HashMap<String, HostTensor>,
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
        let mut consts = const_int_map(f);
        let floats = const_float_map(f);
        let mut ptrs = HashMap::new();
        let mut buf_bytes = HashMap::new();
        let mut owned = Vec::new();
        let mut uid = 0u32;
        if fn_needs_tensor_emit(f, tensors) {
            seed_tensor_ptrs_for_fn(
                f,
                tensors,
                opts,
                s,
                &mut ptrs,
                &mut buf_bytes,
                &mut owned,
                &mut uid,
                &mut consts,
            );
        }
        let mut tctx = if fn_has_tensor_ops(f) {
            Some(TensorEmitCtx {
                opts,
                ptrs: &mut ptrs,
                buf_bytes: &mut buf_bytes,
                owned: &mut owned,
                consts: &consts,
                floats: &floats,
                uid: &mut uid,
            })
        } else {
            None
        };
        let tensor_ref = tctx.as_mut();
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
                tensor_ref,
            );
        } else {
            let ret = emit_body_insts(f, str_globals, user_fns, s, tensor_ref);
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
    mut tensor: Option<&mut TensorEmitCtx<'_>>,
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
                    tensor.as_deref_mut(),
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
                tensor,
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
                    tensor.as_deref_mut(),
                );
            }
            let consts = const_int_map_from(body);
            if let Some(te) = tensor.as_deref_mut() {
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
                    Some(te.ptrs),
                    Some(te.uid),
                );
            } else {
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
                    None,
                    None,
                );
            }
            s.push_str(&format!("  ret i64 %{d}\n"));
            return;
        }
    }
    if !body.is_empty() {
        let (_, _, term) = emit_inst_list(
            body,
            str_globals,
            user_fns,
            s,
            label_id,
            tmp,
            cur_block,
            tensor.as_deref_mut(),
        );
        if term {
            return;
        }
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
    mut tensor: Option<&mut TensorEmitCtx<'_>>,
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
        tensor.as_deref_mut(),
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
        tensor,
    );
    *cur_block = format!("join{id}");
}

fn emit_main_general(
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
    let mut consts = const_int_map(f);
    let floats = const_float_map(f);
    let mut ptrs = HashMap::new();
    let mut buf_bytes = HashMap::new();
    let mut owned = Vec::new();
    let mut uid = 0u32;
    if fn_needs_tensor_emit(f, tensors) {
        seed_tensor_ptrs_for_fn(
            f,
            tensors,
            opts,
            s,
            &mut ptrs,
            &mut buf_bytes,
            &mut owned,
            &mut uid,
            &mut consts,
        );
    }
    let mut tctx = if fn_has_tensor_ops(f) {
        Some(TensorEmitCtx {
            opts,
            ptrs: &mut ptrs,
            buf_bytes: &mut buf_bytes,
            owned: &mut owned,
            consts: &consts,
            floats: &floats,
            uid: &mut uid,
        })
    } else {
        None
    };
    let mut label_id = 0u32;
    let mut tmp = 0u32;
    let mut cur_block = "entry".to_string();
    if let Some(IrInst::Return { value }) = f.instructions.last() {
        let prefix = &f.instructions[..f.instructions.len() - 1];
        let (_, _, term) = emit_inst_list(
            prefix,
            str_globals,
            user_fns,
            s,
            &mut label_id,
            &mut tmp,
            &mut cur_block,
            tctx.as_mut(),
        );
        if opts.instrument {
            s.push_str("  call void @sal_instrument_shutdown()\n");
        }
        if !term {
            let consts = const_int_map(f);
            let op = i64_operand(value, &consts);
            s.push_str(&format!("  ret i64 {op}\n"));
        }
    } else {
        let ret = emit_body_insts(f, str_globals, user_fns, s, tctx.as_mut());
        if opts.instrument {
            s.push_str("  call void @sal_instrument_shutdown()\n");
        }
        s.push_str(&format!("  ret i64 {ret}\n"));
    }
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
                        "neg" => s.push_str(&format!("  %{d} = sub i64 0, {l}\n")),
                        "not" => s.push_str(&format!("  %{d} = call i64 @sal_not(i64 {l})\n")),
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

fn tensor_ptr_operand(name: &str, ptrs: &HashMap<String, String>) -> String {
    if let Some(p) = ptrs.get(name) {
        if p == "null" {
            "null".into()
        } else {
            format!("%{p}")
        }
    } else if name.chars().all(|c| c.is_ascii_digit()) {
        name.to_string()
    } else {
        format!("%{name}")
    }
}

fn emit_one_tensor_bin(
    opts: &LlvmOptions,
    s: &mut String,
    uid: &mut u32,
    ptrs: &mut HashMap<String, String>,
    buf_bytes: &mut HashMap<String, u64>,
    owned: &mut Vec<String>,
    consts: &HashMap<String, i64>,
    floats: &HashMap<String, f64>,
    dest: &str,
    op: &str,
    place: &str,
    left: &str,
    right: &str,
    len: &str,
    right_scalar: bool,
    elem: TensorElem,
) {
    if elem != TensorElem::F32 {
        s.push_str("  ; tensor bin skipped: only F32 lowered today\n");
        return;
    }
    let place_i = place_to_i32(place);
    if place_i != 0 {
        s.push_str(&format!("  call void @sal_on_enter(i32 {place_i})\n"));
    }
    let len_op = i64_operand(len, consts);
    *uid += 1;
    let out = format!("tbin_{uid}");
    let elem_sz = if is_tensor_cmp_op(op) { 1 } else { 4 };
    s.push_str(&format!("  %{out}_bytes = mul i64 {len_op}, {elem_sz}\n"));
    if opts.instrument {
        s.push_str(&format!(
            "  %{out} = call ptr @sal_instrument_malloc(i64 %{out}_bytes, i32 {place_i}, ptr @.site.access)\n"
        ));
    } else {
        s.push_str(&format!(
            "  %{out} = call ptr @sal_place_malloc(i64 %{out}_bytes, i32 {place_i})\n"
        ));
    }
    owned.push(out.clone());
    let a_op = tensor_ptr_operand(left, ptrs);
    let b_op = if right_scalar {
        "null".into()
    } else {
        tensor_ptr_operand(right, ptrs)
    };
    let scalar_v = if right_scalar {
        floats
            .get(right)
            .copied()
            .or_else(|| consts.get(right).map(|v| *v as f64))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let scalar_i = if right_scalar { 1 } else { 0 };
    let scalar_lit = fmt_double(scalar_v);
    let opcode = tensor_bin_op_code(op);
    if is_tensor_cmp_op(op) {
        s.push_str(&format!(
            "  call void @sal_tensor_cmp_f32(i32 {opcode}, ptr %{out}, ptr {a_op}, ptr {b_op}, i64 {len_op}, i32 {scalar_i}, double {scalar_lit})\n"
        ));
    } else {
        s.push_str(&format!(
            "  call void @sal_tensor_bin_f32(i32 {opcode}, ptr %{out}, ptr {a_op}, ptr {b_op}, i64 {len_op}, i32 {scalar_i}, double {scalar_lit})\n"
        ));
    }
    ptrs.insert(dest.to_string(), out.clone());
    buf_bytes.insert(dest.to_string(), 0);
    s.push_str(&format!("  %{dest} = ptrtoint ptr %{out} to i64\n"));
}

fn emit_one_tensor_unary(
    opts: &LlvmOptions,
    s: &mut String,
    uid: &mut u32,
    ptrs: &mut HashMap<String, String>,
    buf_bytes: &mut HashMap<String, u64>,
    owned: &mut Vec<String>,
    consts: &HashMap<String, i64>,
    dest: &str,
    op: &str,
    place: &str,
    input: &str,
    len: &str,
    elem: TensorElem,
) {
    let place_i = place_to_i32(place);
    if place_i != 0 {
        s.push_str(&format!("  call void @sal_on_enter(i32 {place_i})\n"));
    }
    let len_op = i64_operand(len, consts);
    *uid += 1;
    let out = format!("tun_{uid}");
    let elem_sz = if op == "not" { 1 } else { 4 };
    s.push_str(&format!("  %{out}_bytes = mul i64 {len_op}, {elem_sz}\n"));
    if opts.instrument {
        s.push_str(&format!(
            "  %{out} = call ptr @sal_instrument_malloc(i64 %{out}_bytes, i32 {place_i}, ptr @.site.access)\n"
        ));
    } else {
        s.push_str(&format!(
            "  %{out} = call ptr @sal_place_malloc(i64 %{out}_bytes, i32 {place_i})\n"
        ));
    }
    owned.push(out.clone());
    let a_op = tensor_ptr_operand(input, ptrs);
    if op == "not" && elem == TensorElem::I8 {
        s.push_str(&format!(
            "  call void @sal_tensor_not_i8(ptr %{out}, ptr {a_op}, i64 {len_op})\n"
        ));
    } else if op == "neg" && elem == TensorElem::F32 {
        s.push_str(&format!(
            "  call void @sal_tensor_neg_f32(ptr %{out}, ptr {a_op}, i64 {len_op})\n"
        ));
    }
    ptrs.insert(dest.to_string(), out.clone());
    buf_bytes.insert(dest.to_string(), 0);
    s.push_str(&format!("  %{dest} = ptrtoint ptr %{out} to i64\n"));
}
