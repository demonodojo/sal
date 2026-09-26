use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::ast::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IrModule {
    pub functions: Vec<IrFunction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IrFunction {
    pub name: String,
    pub params: Vec<String>,
    pub instructions: Vec<IrInst>,
    pub regions: Vec<FusedRegion>,
    /// Runtime i64 lengths for each `?` axis on tensor params (`{param}_d{axis}`).
    #[serde(default)]
    pub dim_params: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IrInst {
    ConstInt { dest: String, value: i64 },
    ConstString { dest: String, value: String },
    Binary {
        dest: String,
        op: String,
        left: String,
        right: String,
    },
    Call {
        dest: Option<String>,
        func: String,
        args: Vec<String>,
    },
    PlaceCopy {
        dest: String,
        from: String,
        to_place: String,
    },
    Return { value: String },
    Drop { name: String },
    /// Structured if: then/else are nested instruction lists; `dest` is the phi result.
    If {
        cond: String,
        then_body: Vec<IrInst>,
        else_body: Vec<IrInst>,
        then_val: String,
        else_val: String,
        dest: String,
    },
    /// Condition instructions run on every iteration. Loop-carried state is a vec.
    While {
        cond_insts: Vec<IrInst>,
        cond: String,
        body: Vec<IrInst>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FusedRegion {
    pub place: String,
    pub ops: Vec<FusedOp>,
    /// Static peak bytes per place (element bytes of live tensors). 0 when fully symbolic.
    pub peak_bytes: BTreeMap<String, u64>,
    /// When any axis is `?`, human-readable formula documenting the symbolic size.
    #[serde(default)]
    pub peak_symbolic: BTreeMap<String, String>,
    pub fused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FusedOp {
    Matmul { lhs: String, rhs: String, dest: String },
    /// `map`/`relu` fused into the matmul epilogue; no separate intermediate buffer.
    MapEpilogue { op: String, input: String, dest: String },
    Softmax { input: String, dest: String },
    Load { path: String, dest: String },
}

#[derive(Debug, Clone)]
struct TensorInfo {
    elem: TensorElem,
    dims: Vec<Dim>,
    /// When a literal (or other known extent) resolves a `?` axis, that length lives here.
    concrete: Vec<Option<u64>>,
}

pub fn lower_program(prog: &Program) -> IrModule {
    let fn_names: HashMap<String, ()> = prog
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(f) => Some((f.name.clone(), ())),
            _ => None,
        })
        .collect();
    let mut functions = Vec::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            functions.push(lower_fn(f, &fn_names));
        }
    }
    IrModule { functions }
}

fn lower_fn(f: &FnDef, fn_names: &HashMap<String, ()>) -> IrFunction {
    let mut instructions = Vec::new();
    let mut regions = Vec::new();
    let mut counter = 0u32;
    let mut env: HashMap<String, String> = HashMap::new();
    let mut tensors: HashMap<String, TensorInfo> = HashMap::new();
    let mut dim_params: Vec<String> = Vec::new();

    for p in &f.params {
        env.insert(p.name.clone(), p.name.clone());
        if let Type::Tensor { elem, dims, .. } = &p.ty {
            let concrete = dims.iter().map(|_| None).collect();
            for (i, d) in dims.iter().enumerate() {
                if matches!(d, Dim::Dynamic) {
                    let name = format!("{}_d{i}", p.name);
                    if !dim_params.contains(&name) {
                        dim_params.push(name);
                    }
                }
            }
            tensors.insert(
                p.name.clone(),
                TensorInfo {
                    elem: *elem,
                    dims: dims.clone(),
                    concrete,
                },
            );
        }
    }

    for st in &f.body.stmts {
        lower_stmt(
            st,
            &mut instructions,
            &mut env,
            &mut counter,
            &mut regions,
            &mut tensors,
            &dim_params,
            fn_names,
        );
    }
    if let Some(t) = &f.body.tail {
        let v = lower_expr(
            t,
            &mut instructions,
            &mut env,
            &mut counter,
            &mut regions,
            &mut tensors,
            &dim_params,
            fn_names,
        );
        instructions.push(IrInst::Return { value: v });
    }

    IrFunction {
        name: f.name.clone(),
        params: f.params.iter().map(|p| p.name.clone()).collect(),
        instructions,
        regions,
        dim_params,
    }
}

fn lower_stmt(
    st: &Stmt,
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    regions: &mut Vec<FusedRegion>,
    tensors: &mut HashMap<String, TensorInfo>,
    dim_params: &[String],
    fn_names: &HashMap<String, ()>,
) {
    match st {
        Stmt::Let { name, ty, init, .. } => {
            let v = lower_expr(
                init,
                instructions,
                env,
                counter,
                regions,
                tensors,
                dim_params,
                fn_names,
            );
            register_let_tensor(name, ty.as_ref(), init, &v, tensors);
            env.insert(name.clone(), v);
        }
        Stmt::Assign { target, value, .. } => {
            if let Expr::Ident { name, .. } = target {
                let v = lower_expr(
                    value,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                );
                register_let_tensor(name, None, value, &v, tensors);
                env.insert(name.clone(), v);
            } else {
                lower_expr(
                    value,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                );
            }
        }
        Stmt::Expr(e) => {
            lower_expr(
                e,
                instructions,
                env,
                counter,
                regions,
                tensors,
                dim_params,
                fn_names,
            );
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                let val = lower_expr(
                    v,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                );
                instructions.push(IrInst::Return { value: val });
            }
        }
        Stmt::While { cond, body, .. } => {
            let mut cond_insts = Vec::new();
            let c = lower_expr(
                cond,
                &mut cond_insts,
                env,
                counter,
                regions,
                tensors,
                dim_params,
                fn_names,
            );
            let mut body_insts = Vec::new();
            let mut body_env = env.clone();
            for st in &body.stmts {
                lower_stmt(
                    st,
                    &mut body_insts,
                    &mut body_env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                );
            }
            if let Some(t) = &body.tail {
                lower_expr(
                    t,
                    &mut body_insts,
                    &mut body_env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                );
            }
            instructions.push(IrInst::While {
                cond_insts,
                cond: c,
                body: body_insts,
            });
        }
    }
}

fn register_let_tensor(
    name: &str,
    ty: Option<&Type>,
    init: &Expr,
    ir_name: &str,
    tensors: &mut HashMap<String, TensorInfo>,
) {
    let lit = tensor_lit_shape(init);
    let load_ty = load_type_arg(init);
    let annotated = ty.or(load_ty.as_ref());
    let info = if let Some(Type::Tensor { elem, dims, .. }) = annotated {
        let mut concrete: Vec<Option<u64>> = dims
            .iter()
            .map(|d| match d {
                Dim::Static(n) => Some(*n),
                Dim::Dynamic => None,
            })
            .collect();
        if let Some((rows, cols)) = lit {
            // Literal resolves `?` (and fills static axes) with real extents.
            if dims.len() >= 2 {
                let r_i = dims.len() - 2;
                let c_i = dims.len() - 1;
                concrete[r_i] = Some(rows);
                concrete[c_i] = Some(cols);
            }
        }
        Some(TensorInfo {
            elem: *elem,
            dims: dims.clone(),
            concrete,
        })
    } else if let Some((rows, cols)) = lit {
        Some(TensorInfo {
            elem: TensorElem::F32,
            dims: vec![Dim::Static(rows), Dim::Static(cols)],
            concrete: vec![Some(rows), Some(cols)],
        })
    } else {
        None
    };
    if let Some(info) = info {
        tensors.insert(ir_name.to_string(), info.clone());
        tensors.insert(name.to_string(), info);
    }
}

/// `load[F32, 4, 4](…)` o `load[Tensor[F32, m, n] on …](…)`.
fn load_type_arg(e: &Expr) -> Option<Type> {
    let Expr::Call {
        func, type_args, span, ..
    } = e
    else {
        return None;
    };
    let Expr::Ident { name, .. } = func.as_ref() else {
        return None;
    };
    if name != "load" || type_args.is_empty() {
        return None;
    }
    match type_args.first() {
        Some(TypeArg::Type(Type::Tensor { .. })) => match &type_args[0] {
            TypeArg::Type(ty) => Some(ty.clone()),
            TypeArg::Dim(_) => None,
        },
        Some(TypeArg::Type(Type::Named { name, .. })) => {
            let elem = match name.as_str() {
                "F32" => TensorElem::F32,
                "F16" => TensorElem::F16,
                "BF16" => TensorElem::BF16,
                "I8" => TensorElem::I8,
                _ => return None,
            };
            let mut dims = Vec::new();
            for a in type_args.iter().skip(1) {
                match a {
                    TypeArg::Dim(d) => dims.push(d.clone()),
                    _ => return None,
                }
            }
            Some(Type::Tensor {
                elem,
                dims,
                place: Place::Cpu,
                span: *span,
            })
        }
        _ => None,
    }
}

fn tensor_lit_shape(e: &Expr) -> Option<(u64, u64)> {
    let Expr::TensorLit { rows, .. } = e else {
        return None;
    };
    let nrows = rows.len() as u64;
    let ncols = rows.first().map(|r| r.len() as u64).unwrap_or(0);
    Some((nrows, ncols))
}

fn lower_expr_simple(
    e: &Expr,
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    tensors: &mut HashMap<String, TensorInfo>,
    fn_names: &HashMap<String, ()>,
) -> String {
    lower_expr(
        e,
        instructions,
        env,
        counter,
        &mut Vec::new(),
        tensors,
        &[],
        fn_names,
    )
}

fn runtime_call_name(fname: &str, fn_names: &HashMap<String, ()>) -> String {
    // List ops always hit the runtime; prelude bodies are signatures only.
    if matches!(fname, "list_new" | "list_push" | "list_len" | "list_get") {
        return format!("sal_{fname}");
    }
    if fn_names.contains_key(fname) {
        return fname.to_string();
    }
    if fname == "matmul" {
        return "sal_matmul_f32".into();
    }
    if is_runtime_builtin(fname) {
        format!("sal_{fname}")
    } else {
        format!("sal_{fname}")
    }
}

fn is_runtime_builtin(name: &str) -> bool {
    matches!(
        name,
        "print"
            | "load"
            | "matmul"
            | "softmax"
            | "relu"
            | "map"
            | "reduce"
            | "index"
            | "read_file"
            | "write_file"
            | "write_png"
            | "image_new"
            | "print_str"
            | "eprint_str"
            | "getenv"
            | "mkdir_p"
            | "argc"
            | "argv"
            | "str_eq"
            | "str_contains"
            | "str_concat"
            | "str_append"
            | "str_len"
            | "str_char"
            | "str_skip"
            | "str_hash"
            | "map_new"
            | "map_get"
            | "map_put"
            | "ir_text"
            | "lex_src"
            | "str_slice"
            | "int_to_str"
            | "char_to_str"
            | "strdup"
            | "free"
            | "select_str"
            | "copy_file"
            | "copy_self"
            | "not"
            | "gated_print_str"
            | "gated_copy_self"
            | "vec_new"
            | "vec_push"
            | "vec_get"
            | "vec_set"
            | "vec_len"
            | "vec_free"
            | "list_new"
            | "list_push"
            | "list_len"
            | "list_get"
            | "clang"
            | "tmp_path"
            | "exec_capture"
            | "exec_compile"
            | "gated_exec_compile"
            | "place_launches"
            | "panic"
    )
}

fn lower_expr(
    e: &Expr,
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    regions: &mut Vec<FusedRegion>,
    tensors: &mut HashMap<String, TensorInfo>,
    dim_params: &[String],
    fn_names: &HashMap<String, ()>,
) -> String {
    match e {
        Expr::Int { value, .. } => {
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::ConstInt {
                dest: dest.clone(),
                value: *value,
            });
            dest
        }
        Expr::String { value, .. } => {
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::ConstString {
                dest: dest.clone(),
                value: value.clone(),
            });
            dest
        }
        Expr::Bool { value, .. } => {
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::ConstInt {
                dest: dest.clone(),
                value: if *value { 1 } else { 0 },
            });
            dest
        }
        Expr::Ident { name, .. } => env.get(name).cloned().unwrap_or_else(|| name.clone()),
        Expr::Binary {
            op, left, right, ..
        } => {
            let l = lower_expr_simple(left, instructions, env, counter, tensors, fn_names);
            let r = lower_expr_simple(right, instructions, env, counter, tensors, fn_names);
            *counter += 1;
            let dest = format!("t{counter}");
            let op_s = match op {
                BinOp::Add => "add",
                BinOp::Sub => "sub",
                BinOp::Mul => "mul",
                BinOp::Div => "div",
                BinOp::Eq => "eq",
                BinOp::Ne => "ne",
                BinOp::Lt => "lt",
                BinOp::Le => "le",
                BinOp::Gt => "gt",
                BinOp::Ge => "ge",
            };
            instructions.push(IrInst::Binary {
                dest: dest.clone(),
                op: op_s.into(),
                left: l,
                right: r,
            });
            dest
        }
        Expr::Call {
            func,
            type_args: _,
            args,
            ..
        } => {
            let fname = match func.as_ref() {
                Expr::Ident { name, .. } => name.clone(),
                _ => "unknown".into(),
            };
            let mut arg_names = Vec::new();
            for a in args {
                arg_names.push(lower_expr_simple(
                    a,
                    instructions,
                    env,
                    counter,
                    tensors,
                    fn_names,
                ));
            }
            *counter += 1;
            let dest = format!("t{counter}");
            if fname == "load" {
                if let Some(Expr::String { value, .. }) = args.first() {
                    instructions.push(IrInst::Call {
                        dest: Some(dest.clone()),
                        func: "sal_load".into(),
                        args: vec![format!("\"{value}\"")],
                    });
                    return dest;
                }
            }
            let func_name = runtime_call_name(&fname, fn_names);
            if fname == "matmul" && arg_names.len() >= 2 {
                let lhs = arg_names[0].clone();
                let rhs = arg_names[1].clone();
                let (m, k, n) =
                    emit_matmul_dim_args(&lhs, &rhs, tensors, dim_params, instructions, counter);
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: func_name,
                    args: vec![lhs, rhs, m, k, n],
                });
            } else {
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: func_name,
                    args: arg_names,
                });
            }
            dest
        }
        Expr::Lambda { body, .. } => {
            let _ = body;
            *counter += 1;
            format!("t{counter}")
        }
        Expr::On { place, body, .. } => {
            let place_s = place_str(place);
            let mut ops = Vec::new();
            if let Some(t) = &body.tail {
                collect_fused_ops(t, &mut ops, env);
            }
            let has_epilogue = ops
                .iter()
                .any(|o| matches!(o, FusedOp::MapEpilogue { .. }));
            let fused = has_epilogue
                || ops.len() > 1
                || ops.iter().any(|o| matches!(o, FusedOp::Matmul { .. }));
            let (peak_bytes, peak_symbolic) = estimate_peak_for_region(&ops, tensors, &place_s);
            regions.push(FusedRegion {
                place: place_s,
                ops: ops.clone(),
                peak_bytes,
                peak_symbolic,
                fused,
            });

            *counter += 1;
            let dest = format!("t{counter}");
            if let Some(FusedOp::Matmul { lhs, rhs, .. }) =
                ops.iter().find(|o| matches!(o, FusedOp::Matmul { .. }))
            {
                let (m, k, n) =
                    emit_matmul_dim_args(lhs, rhs, tensors, dim_params, instructions, counter);
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: "sal_matmul_f32".into(),
                    args: vec![lhs.clone(), rhs.clone(), m, k, n],
                });
            }
            dest
        }
        Expr::To { place, expr, .. } => {
            let src = lower_expr_simple(expr, instructions, env, counter, tensors, fn_names);
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::PlaceCopy {
                dest: dest.clone(),
                from: src.clone(),
                to_place: place_str(place),
            });
            // Drop records the source binding name (corpus / stage1 IR); LLVM frees via `from` ptrs.
            instructions.push(IrInst::Drop {
                name: expr_name(expr),
            });
            if let Some(info) = tensors.get(&src).cloned() {
                tensors.insert(dest.clone(), info);
            }
            dest
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            let c = lower_expr_simple(cond, instructions, env, counter, tensors, fn_names);
            let mut then_body = Vec::new();
            let mut else_body = Vec::new();
            let mut then_env = env.clone();
            let mut else_env = env.clone();
            for st in &then_block.stmts {
                lower_stmt(
                    st,
                    &mut then_body,
                    &mut then_env,
                    counter,
                    &mut Vec::new(),
                    tensors,
                    dim_params,
                    fn_names,
                );
            }
            let then_val = if let Some(t) = &then_block.tail {
                lower_expr(
                    t,
                    &mut then_body,
                    &mut then_env,
                    counter,
                    &mut Vec::new(),
                    tensors,
                    dim_params,
                    fn_names,
                )
            } else {
                *counter += 1;
                let d = format!("t{counter}");
                then_body.push(IrInst::ConstInt {
                    dest: d.clone(),
                    value: 0,
                });
                d
            };
            let else_val = if let Some(else_block) = else_block {
                for st in &else_block.stmts {
                    lower_stmt(
                        st,
                        &mut else_body,
                        &mut else_env,
                        counter,
                        &mut Vec::new(),
                        tensors,
                        dim_params,
                        fn_names,
                    );
                }
                if let Some(t) = &else_block.tail {
                    lower_expr(
                        t,
                        &mut else_body,
                        &mut else_env,
                        counter,
                        &mut Vec::new(),
                        tensors,
                        dim_params,
                        fn_names,
                    )
                } else {
                    *counter += 1;
                    let d = format!("t{counter}");
                    else_body.push(IrInst::ConstInt {
                        dest: d.clone(),
                        value: 0,
                    });
                    d
                }
            } else {
                *counter += 1;
                let d = format!("t{counter}");
                else_body.push(IrInst::ConstInt {
                    dest: d.clone(),
                    value: 0,
                });
                d
            };
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::If {
                cond: c,
                then_body,
                else_body,
                then_val,
                else_val,
                dest: dest.clone(),
            });
            dest
        }
        _ => {
            *counter += 1;
            format!("t{counter}")
        }
    }
}

/// Emit SSA names for matmul `(m, k, n)`: ConstInt when known, else `{tensor}_d{axis}` for `?`.
fn emit_matmul_dim_args(
    lhs: &str,
    rhs: &str,
    tensors: &HashMap<String, TensorInfo>,
    dim_params: &[String],
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> (String, String, String) {
    let li = tensors.get(lhs);
    let ri = tensors.get(rhs);
    let m = axis_len_ssa(li, lhs, /*row*/ true, dim_params, instructions, counter);
    let k = axis_len_ssa(li, lhs, /*row*/ false, dim_params, instructions, counter);
    let n = axis_len_ssa(ri, rhs, /*row*/ false, dim_params, instructions, counter);
    (m, k, n)
}

fn axis_len_ssa(
    info: Option<&TensorInfo>,
    tensor: &str,
    take_row: bool,
    dim_params: &[String],
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> String {
    let Some(t) = info else {
        // Unknown shape: still name a runtime length (never hardcode 0 here).
        let name = format!("{tensor}_d{}", if take_row { 0 } else { 1 });
        return name;
    };
    if t.dims.len() < 2 {
        let name = format!("{tensor}_d0");
        return name;
    }
    let idx = if take_row {
        t.dims.len() - 2
    } else {
        t.dims.len() - 1
    };
    match (&t.dims[idx], t.concrete.get(idx).copied().flatten()) {
        (Dim::Static(n), _) => push_const_dim(*n, instructions, counter),
        (Dim::Dynamic, Some(n)) => push_const_dim(n, instructions, counter),
        (Dim::Dynamic, None) => {
            let name = format!("{tensor}_d{idx}");
            debug_assert!(
                dim_params.is_empty() || dim_params.iter().any(|d| d == &name),
                "dynamic axis {name} should be listed in dim_params"
            );
            let _ = dim_params;
            name
        }
    }
}

fn push_const_dim(n: u64, instructions: &mut Vec<IrInst>, counter: &mut u32) -> String {
    *counter += 1;
    let dest = format!("t{counter}");
    instructions.push(IrInst::ConstInt {
        dest: dest.clone(),
        value: n as i64,
    });
    dest
}

fn expr_name(e: &Expr) -> String {
    match e {
        Expr::Ident { name, .. } => name.clone(),
        _ => "tmp".into(),
    }
}

fn place_str(p: &Place) -> String {
    match p {
        Place::Cpu => "cpu".into(),
        Place::Gpu => "gpu".into(),
        Place::Tpu => "tpu".into(),
        Place::Param(n) => n.clone(),
    }
}

fn collect_fused_ops(e: &Expr, ops: &mut Vec<FusedOp>, env: &HashMap<String, String>) {
    match e {
        Expr::Call { func, args, .. } => {
            for a in args {
                collect_fused_ops(a, ops, env);
            }
            let fname = match func.as_ref() {
                Expr::Ident { name, .. } => name.as_str(),
                _ => return,
            };
            match fname {
                "relu" | "map" => {
                    if let Some(FusedOp::Matmul { dest, .. }) = ops.last() {
                        let mm_dest = dest.clone();
                        ops.push(FusedOp::MapEpilogue {
                            op: fname.into(),
                            input: mm_dest.clone(),
                            dest: format!("{mm_dest}_epilogue"),
                        });
                    }
                }
                "matmul" => {
                    if args.len() == 2 {
                        ops.push(FusedOp::Matmul {
                            lhs: expr_id(&args[0], env),
                            rhs: expr_id(&args[1], env),
                            dest: "mm".into(),
                        });
                    }
                }
                "softmax" => {
                    ops.push(FusedOp::Softmax {
                        input: expr_id(args.first().unwrap(), env),
                        dest: "sm".into(),
                    });
                }
                _ => {}
            }
        }
        Expr::Lambda { .. } => {
            // Callback body is not expanded; map epilogue is keyed off the call name.
        }
        _ => {}
    }
}

fn expr_id(e: &Expr, env: &HashMap<String, String>) -> String {
    match e {
        Expr::Ident { name, .. } => env.get(name).cloned().unwrap_or_else(|| name.clone()),
        _ => "tmp".into(),
    }
}

fn elem_bytes(elem: TensorElem) -> u64 {
    match elem {
        TensorElem::F32 => 4,
        TensorElem::F16 | TensorElem::BF16 => 2,
        TensorElem::I8 => 1,
    }
}

/// Product of dims as either a concrete element count or a symbolic expression.
fn dims_elems(info: &TensorInfo) -> Result<u64, String> {
    let mut product = 1u64;
    let mut parts: Vec<String> = Vec::new();
    let mut any_dyn = false;
    for (i, d) in info.dims.iter().enumerate() {
        match d {
            Dim::Static(n) => {
                product = product.saturating_mul(*n);
                parts.push(n.to_string());
            }
            Dim::Dynamic => {
                if let Some(Some(n)) = info.concrete.get(i).copied() {
                    product = product.saturating_mul(n);
                    parts.push(n.to_string());
                } else {
                    any_dyn = true;
                    parts.push("?".into());
                }
            }
        }
    }
    if any_dyn {
        Err(parts.join("*"))
    } else {
        Ok(product)
    }
}

fn tensor_nbytes(info: &TensorInfo) -> Result<u64, String> {
    match dims_elems(info) {
        Ok(n) => Ok(n.saturating_mul(elem_bytes(info.elem))),
        Err(sym) => Err(format!("{sym}*{}", elem_bytes(info.elem))),
    }
}

fn matmul_out_info(lhs: &TensorInfo, rhs: &TensorInfo) -> Option<TensorInfo> {
    if lhs.dims.len() < 2 || rhs.dims.len() < 2 {
        return None;
    }
    let mut out_dims = lhs.dims[..lhs.dims.len() - 1].to_vec();
    out_dims.extend_from_slice(&rhs.dims[rhs.dims.len() - 1..]);
    let mut concrete: Vec<Option<u64>> = out_dims.iter().map(|_| None).collect();
    // Copy concrete for kept lhs batch dims + rhs n.
    for (i, _) in out_dims.iter().enumerate().take(lhs.dims.len() - 1) {
        concrete[i] = lhs.concrete.get(i).copied().flatten();
    }
    let n_idx = out_dims.len() - 1;
    let rhs_n = rhs.dims.len() - 1;
    concrete[n_idx] = match &rhs.dims[rhs_n] {
        Dim::Static(n) => Some(*n),
        Dim::Dynamic => rhs.concrete.get(rhs_n).copied().flatten(),
    };
    Some(TensorInfo {
        elem: lhs.elem,
        dims: out_dims,
        concrete,
    })
}

/// Peak = simultaneous live weights + activations + work buffer (fused epilogue adds 0).
fn estimate_peak_for_region(
    ops: &[FusedOp],
    tensors: &HashMap<String, TensorInfo>,
    place: &str,
) -> (BTreeMap<String, u64>, BTreeMap<String, String>) {
    let mut peak_bytes = BTreeMap::new();
    let mut peak_symbolic = BTreeMap::new();

    let mut static_total: u64 = 0;
    let mut sym_parts: Vec<String> = Vec::new();
    let mut saw_shaped = false;

    for op in ops {
        match op {
            FusedOp::Matmul { lhs, rhs, .. } => {
                let li = tensors.get(lhs);
                let ri = tensors.get(rhs);
                if let (Some(l), Some(r)) = (li, ri) {
                    saw_shaped = true;
                    // activations (lhs) + weights (rhs) + output work buffer
                    for (label, info) in [("act", l), ("w", r)] {
                        match tensor_nbytes(info) {
                            Ok(n) => static_total = static_total.saturating_add(n),
                            Err(s) => sym_parts.push(format!("{label}:{s}")),
                        }
                    }
                    if let Some(out) = matmul_out_info(l, r) {
                        match tensor_nbytes(&out) {
                            Ok(n) => static_total = static_total.saturating_add(n),
                            Err(s) => sym_parts.push(format!("out:{s}")),
                        }
                    }
                } else {
                    // Unknown shapes: document symbolic placeholder (no magic 4096).
                    sym_parts.push(format!("matmul({lhs},{rhs}):symbolic"));
                }
            }
            FusedOp::MapEpilogue { .. } => {
                // Fused into matmul epilogue — no extra intermediate buffer.
            }
            FusedOp::Softmax { input, .. } => {
                if let Some(info) = tensors.get(input) {
                    saw_shaped = true;
                    match tensor_nbytes(info) {
                        Ok(n) => static_total = static_total.saturating_add(n),
                        Err(s) => sym_parts.push(format!("softmax:{s}")),
                    }
                } else {
                    sym_parts.push(format!("softmax({input}):symbolic"));
                }
            }
            FusedOp::Load { .. } => {}
        }
    }

    if !sym_parts.is_empty() {
        let formula = if static_total > 0 {
            format!("{static_total}+{}", sym_parts.join("+"))
        } else {
            sym_parts.join("+")
        };
        peak_symbolic.insert(place.to_string(), formula);
        // Static contribution still reported when partial shapes are known.
        peak_bytes.insert(place.to_string(), static_total);
    } else if saw_shaped || static_total > 0 {
        peak_bytes.insert(place.to_string(), static_total);
    } else if !ops.is_empty() {
        peak_symbolic.insert(place.to_string(), "symbolic(unknown_shapes)".into());
        peak_bytes.insert(place.to_string(), 0);
    }

    (peak_bytes, peak_symbolic)
}

pub fn ir_to_text(m: &IrModule) -> String {
    let mut s = String::new();
    for f in &m.functions {
        s.push_str(&format!("fn {}:\n", f.name));
        if !f.dim_params.is_empty() {
            s.push_str(&format!("  dim_params={:?}\n", f.dim_params));
        }
        for i in &f.instructions {
            s.push_str("  ");
            s.push_str(&format!("{i:?}\n"));
        }
        for r in &f.regions {
            s.push_str(&format!(
                "  region on {} fused={} peak_bytes={:?}"
                ,
                r.place, r.fused, r.peak_bytes
            ));
            if !r.peak_symbolic.is_empty() {
                s.push_str(&format!(" peak_symbolic={:?}", r.peak_symbolic));
            }
            s.push('\n');
            for op in &r.ops {
                match op {
                    FusedOp::MapEpilogue { op: eop, input, dest } => {
                        s.push_str(&format!(
                            "    MapEpilogue {{ op: \"{eop}\", input: \"{input}\", dest: \"{dest}\" }}  # fused epilogue, no intermediate buffer\n"
                        ));
                    }
                    other => s.push_str(&format!("    {other:?}\n")),
                }
            }
        }
    }
    s
}
