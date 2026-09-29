use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ast::*;
use crate::layout::{enum_variant_index, is_transparent_struct, struct_field_index};
use crate::string_expr::{
    add_chain_leftmost, add_right_is_fresh_temp, is_string_type, StringEnv,
};

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
    ConstFloat {
        dest: String,
        value: f64,
    },
    /// Element-wise tensor op; `right_scalar` when the RHS is Int/Float, not a tensor buffer.
    TensorBin {
        dest: String,
        op: String,
        elem: TensorElem,
        place: String,
        left: String,
        right: String,
        len: String,
        right_scalar: bool,
    },
    TensorUnary {
        dest: String,
        op: String,
        elem: TensorElem,
        place: String,
        input: String,
        len: String,
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
    /// `carried` are assignments visible after the if.
    If {
        cond: String,
        then_body: Vec<IrInst>,
        else_body: Vec<IrInst>,
        then_val: String,
        else_val: String,
        dest: String,
        #[serde(default)]
        carried: Vec<IfCarry>,
    },
    /// Condition instructions run on every iteration. `carried` are the phi values.
    While {
        cond_insts: Vec<IrInst>,
        cond: String,
        body: Vec<IrInst>,
        #[serde(default)]
        carried: Vec<WhileCarry>,
    },
    /// Explicit grid for `on gpu kernel i, j in …`: nested loops over static bounds.
    KernelGrid {
        place: String,
        index_names: Vec<String>,
        bounds: Vec<u64>,
        body: Vec<IrInst>,
        tail: String,
        dest: String,
    },
    /// Multi-field struct value (LLVM aggregate lowered to i64 slots packed in one SSA name).
    Aggregate {
        dest: String,
        struct_name: String,
        fields: Vec<String>,
    },
    Extract {
        dest: String,
        base: String,
        struct_name: String,
        field_index: u32,
    },
    /// Enum as one i64: unit variants use the tag alone; payloads use `(payload << 8) | tag`.
    EnumMake {
        dest: String,
        enum_name: String,
        variant_index: u32,
        payload: Option<String>,
    },
    BitAnd {
        dest: String,
        left: String,
        right: String,
    },
    /// `match` on an enum: dispatch on the low tag byte.
    Switch {
        scrut: String,
        arms: Vec<SwitchArm>,
        default_body: Vec<IrInst>,
        default_val: String,
        dest: String,
    },
}

/// One `match` arm lowered to a switch case.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwitchArm {
    pub tag: i64,
    pub body: Vec<IrInst>,
    pub value: String,
}

/// Assignment merged at the join of an `if`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IfCarry {
    pub dest: String,
    pub then_val: String,
    pub else_val: String,
}

/// Assignment fed back into the next `while` iteration and visible after the loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WhileCarry {
    pub dest: String,
    pub incoming: String,
    pub updated: String,
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
    /// Fused element-wise binop inside `on` (no separate intermediate buffer).
    ColumnBin {
        op: String,
        left: String,
        right: String,
        dest: String,
        scalar: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct TensorInfo {
    elem: TensorElem,
    dims: Vec<Dim>,
    place: Place,
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
    lower_program_with_callables(prog, &fn_names)
}

pub fn lower_program_with_callables(
    prog: &Program,
    callable_fns: &HashMap<String, ()>,
) -> IrModule {
    lower_program_with_callables_env(prog, callable_fns, StringEnv::from_program_inferred(prog))
}

/// `x = x + ...` where the chain is a string concatenation starting at `x`.
fn assign_accumulates_string(target: &str, value: &Expr, strings: &StringEnv) -> bool {
    strings.add_is_string_concat(value)
        && matches!(add_chain_leftmost(value), Expr::Ident { name, .. } if name == target)
}

pub fn lower_program_with_callables_env(
    prog: &Program,
    callable_fns: &HashMap<String, ()>,
    strings: StringEnv,
) -> IrModule {
    let fn_names = callable_fns;
    let mut param_modes: HashMap<String, Vec<ParamMode>> = HashMap::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            param_modes.insert(
                f.name.clone(),
                f.params.iter().map(|p| p.mode).collect(),
            );
        }
    }
    let mut functions = Vec::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            functions.push(lower_fn(f, &fn_names, &strings, &param_modes));
        }
    }
    IrModule { functions }
}

fn lower_fn(
    f: &FnDef,
    fn_names: &HashMap<String, ()>,
    base: &StringEnv,
    param_modes: &HashMap<String, Vec<ParamMode>>,
) -> IrFunction {
    let mut instructions = Vec::new();
    let mut regions = Vec::new();
    let mut counter = 0u32;
    let mut env: HashMap<String, String> = HashMap::new();
    let mut tensors: HashMap<String, TensorInfo> = HashMap::new();
    let mut dim_params: Vec<String> = Vec::new();
    let mut strings = base.clone();
    strings.seed_params(&f.params);

    for p in &f.params {
        env.insert(p.name.clone(), p.name.clone());
        if let Type::Tensor {
            elem, dims, place, ..
        } = &p.ty
        {
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
                    place: place.clone(),
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
            &mut strings,
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
            &mut strings,
        );
        instructions.push(IrInst::Return { value: v });
    }
    let ret = match instructions.last() {
        Some(IrInst::Return { .. }) => instructions.pop(),
        _ => None,
    };
    emit_struct_string_drops(
        f,
        &mut instructions,
        &env,
        &mut counter,
        &strings,
        param_modes,
    );
    if let Some(r) = ret {
        instructions.push(r);
    }

    IrFunction {
        name: f.name.clone(),
        params: f.params.iter().map(|p| p.name.clone()).collect(),
        instructions,
        regions,
        dim_params,
    }
}

fn emit_struct_string_drops(
    f: &FnDef,
    instructions: &mut Vec<IrInst>,
    env: &HashMap<String, String>,
    counter: &mut u32,
    strings: &StringEnv,
    param_modes: &HashMap<String, Vec<ParamMode>>,
) {
    let moved = moved_bindings(f, param_modes);
    let own_mode: HashMap<&str, ParamMode> = f
        .params
        .iter()
        .map(|p| (p.name.as_str(), p.mode))
        .collect();
    let mut names: Vec<String> = strings.struct_types.keys().cloned().collect();
    names.sort();
    for name in names {
        if moved.contains(&name) {
            continue;
        }
        if let Some(mode) = own_mode.get(name.as_str()) {
            if *mode != ParamMode::Take {
                continue;
            }
        }
        let Some(sn) = strings.struct_types.get(&name).cloned() else {
            continue;
        };
        let Some(sdef) = strings.structs.get(&sn) else {
            continue;
        };
        let Some(base) = env.get(&name).cloned() else {
            continue;
        };
        let transparent = is_transparent_struct(sdef);
        for (i, field) in sdef.fields.iter().enumerate() {
            if !is_string_type(&field.ty) {
                continue;
            }
            let ssa = if transparent {
                base.clone()
            } else {
                *counter += 1;
                let d = format!("t{counter}");
                instructions.push(IrInst::Extract {
                    dest: d.clone(),
                    base: base.clone(),
                    struct_name: sn.clone(),
                    field_index: i as u32,
                });
                d
            };
            instructions.push(IrInst::Call {
                dest: None,
                func: "sal_free".into(),
                args: vec![ssa],
            });
        }
    }
}

fn moved_bindings(f: &FnDef, param_modes: &HashMap<String, Vec<ParamMode>>) -> HashSet<String> {
    let mut moved = HashSet::new();
    collect_moved_block(&f.body, param_modes, &mut moved);
    moved
}

fn collect_moved_block(
    b: &Block,
    param_modes: &HashMap<String, Vec<ParamMode>>,
    moved: &mut HashSet<String>,
) {
    for st in &b.stmts {
        collect_moved_stmt(st, param_modes, moved);
    }
    if let Some(t) = &b.tail {
        if let Expr::Ident { name, .. } = t.as_ref() {
            moved.insert(name.clone());
        }
        collect_moved_expr(t, param_modes, moved);
    }
}

fn collect_moved_stmt(
    st: &Stmt,
    param_modes: &HashMap<String, Vec<ParamMode>>,
    moved: &mut HashSet<String>,
) {
    match st {
        Stmt::Let { init, .. } => {
            if let Expr::Ident { name, .. } = init {
                moved.insert(name.clone());
            }
            collect_moved_expr(init, param_modes, moved);
        }
        Stmt::Assign { value, .. } => {
            if let Expr::Ident { name, .. } = value {
                moved.insert(name.clone());
            }
            collect_moved_expr(value, param_modes, moved);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                if let Expr::Ident { name, .. } = v {
                    moved.insert(name.clone());
                }
                collect_moved_expr(v, param_modes, moved);
            }
        }
        Stmt::Expr(e) => collect_moved_expr(e, param_modes, moved),
        Stmt::While { cond, body, .. } => {
            collect_moved_expr(cond, param_modes, moved);
            collect_moved_block(body, param_modes, moved);
        }
    }
}

fn collect_moved_expr(
    e: &Expr,
    param_modes: &HashMap<String, Vec<ParamMode>>,
    moved: &mut HashSet<String>,
) {
    match e {
        Expr::Call { func, args, .. } => {
            if let Expr::Ident { name, .. } = func.as_ref() {
                if let Some(modes) = param_modes.get(name) {
                    for (i, a) in args.iter().enumerate() {
                        if modes.get(i) == Some(&ParamMode::Take) {
                            if let Expr::Ident { name: arg, .. } = a {
                                moved.insert(arg.clone());
                            }
                        }
                    }
                }
            }
            for a in args {
                collect_moved_expr(a, param_modes, moved);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_moved_expr(left, param_modes, moved);
            collect_moved_expr(right, param_modes, moved);
        }
        Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } | Expr::Field { base: expr, .. } => {
            collect_moved_expr(expr, param_modes, moved);
        }
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            ..
        } => {
            collect_moved_expr(cond, param_modes, moved);
            collect_moved_block(then_block, param_modes, moved);
            for arm in elsifs {
                collect_moved_expr(&arm.cond, param_modes, moved);
                collect_moved_block(&arm.body, param_modes, moved);
            }
            if let Some(b) = else_block {
                collect_moved_block(b, param_modes, moved);
            }
        }
        Expr::Block(b) | Expr::On { body: b, .. } => collect_moved_block(b, param_modes, moved),
        Expr::Match { scrutinee, arms, .. } => {
            collect_moved_expr(scrutinee, param_modes, moved);
            for arm in arms {
                collect_moved_expr(&arm.body, param_modes, moved);
            }
        }
        _ => {}
    }
}

fn merge_if_carries(
    parent: &HashMap<String, String>,
    then_env: &HashMap<String, String>,
    else_env: &HashMap<String, String>,
    counter: &mut u32,
    env: &mut HashMap<String, String>,
    tensors: &mut HashMap<String, TensorInfo>,
) -> Vec<IfCarry> {
    let mut names: Vec<String> = parent.keys().cloned().collect();
    names.sort();
    let mut carried = Vec::new();
    for name in names {
        let base = &parent[&name];
        let t = then_env.get(&name).unwrap_or(base);
        let e = else_env.get(&name).unwrap_or(base);
        if t == base && e == base {
            continue;
        }
        if t == e {
            env.insert(name, t.clone());
            continue;
        }
        *counter += 1;
        let dest = format!("t{counter}");
        if let Some(info) = tensors
            .get(t)
            .cloned()
            .or_else(|| tensors.get(e).cloned())
        {
            tensors.insert(dest.clone(), info);
        }
        env.insert(name, dest.clone());
        carried.push(IfCarry {
            dest,
            then_val: t.clone(),
            else_val: e.clone(),
        });
    }
    carried
}

fn collect_assigned_block(body: &Block, names: &mut HashSet<String>) {
    for st in &body.stmts {
        collect_assigned_stmt(st, names);
    }
    if let Some(tail) = &body.tail {
        collect_assigned_expr(tail, names);
    }
}

fn collect_assigned_stmt(st: &Stmt, names: &mut HashSet<String>) {
    match st {
        Stmt::Assign { target, value, .. } => {
            if let Expr::Ident { name, .. } = target {
                names.insert(name.clone());
            }
            collect_assigned_expr(value, names);
        }
        Stmt::Let { init, .. } => collect_assigned_expr(init, names),
        Stmt::Expr(e) => collect_assigned_expr(e, names),
        Stmt::Return { value: Some(e), .. } => collect_assigned_expr(e, names),
        Stmt::Return { value: None, .. } => {}
        Stmt::While { cond, body, .. } => {
            collect_assigned_expr(cond, names);
            collect_assigned_block(body, names);
        }
    }
}

fn collect_assigned_expr(e: &Expr, names: &mut HashSet<String>) {
    match e {
        Expr::If {
            cond,
            then_block,
            elsifs,
            else_block,
            ..
        } => {
            collect_assigned_expr(cond, names);
            collect_assigned_block(then_block, names);
            for arm in elsifs {
                collect_assigned_expr(&arm.cond, names);
                collect_assigned_block(&arm.body, names);
            }
            if let Some(b) = else_block {
                collect_assigned_block(b, names);
            }
        }
        Expr::On { body, .. } | Expr::Block(body) => collect_assigned_block(body, names),
        Expr::Call { func, args, .. } => {
            collect_assigned_expr(func, names);
            for a in args {
                collect_assigned_expr(a, names);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_assigned_expr(left, names);
            collect_assigned_expr(right, names);
        }
        Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } => {
            collect_assigned_expr(expr, names);
        }
        _ => {}
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
    strings: &mut StringEnv,
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
                strings,
            );
            register_let_tensor(name, ty.as_ref(), init, &v, tensors);
            env.insert(name.clone(), v);
            strings.note_init(name, ty.as_ref(), init);
        }
        Stmt::Assign { target, value, .. } => {
            if let Expr::Field { base, field, .. } = target {
                if let Expr::Ident { name, .. } = base.as_ref() {
                    if let Some(sn) = strings.struct_types.get(name).cloned() {
                        if let Some(sdef) = strings.structs.get(&sn).cloned() {
                            if is_transparent_struct(&sdef) {
                                let v = lower_expr(
                                    value,
                                    instructions,
                                    env,
                                    counter,
                                    regions,
                                    tensors,
                                    dim_params,
                                    fn_names,
                                    strings,
                                );
                                env.insert(name.clone(), v);
                                return;
                            }
                            if let Some(idx) = struct_field_index(&sdef, field) {
                                let new_val = lower_expr(
                                    value,
                                    instructions,
                                    env,
                                    counter,
                                    regions,
                                    tensors,
                                    dim_params,
                                    fn_names,
                                    strings,
                                );
                                let base_ssa = env.get(name).cloned().unwrap_or_else(|| "0".into());
                                let mut field_ssas = Vec::new();
                                for i in 0..sdef.fields.len() {
                                    if i == idx {
                                        field_ssas.push(new_val.clone());
                                    } else {
                                        *counter += 1;
                                        let d = format!("t{counter}");
                                        instructions.push(IrInst::Extract {
                                            dest: d.clone(),
                                            base: base_ssa.clone(),
                                            struct_name: sn.clone(),
                                            field_index: i as u32,
                                        });
                                        field_ssas.push(d);
                                    }
                                }
                                *counter += 1;
                                let dest = format!("t{counter}");
                                instructions.push(IrInst::Aggregate {
                                    dest: dest.clone(),
                                    struct_name: sn,
                                    fields: field_ssas,
                                });
                                env.insert(name.clone(), dest);
                                return;
                            }
                        }
                    }
                }
            }
            if let Expr::Ident { name, .. } = target {
                strings.accumulate = if assign_accumulates_string(name, value, strings) {
                    Some(name.clone())
                } else {
                    None
                };
                let v = lower_expr(
                    value,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    strings,
                );
                strings.accumulate = None;
                register_let_tensor(name, None, value, &v, tensors);
                env.insert(name.clone(), v);
                strings.note_init(name, None, value);
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
                    strings,
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
                strings,
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
                    strings,
                );
                instructions.push(IrInst::Return { value: val });
            }
        }
        Stmt::While { cond, body, .. } => {
            let mut assigned = HashSet::new();
            collect_assigned_block(body, &mut assigned);
            let mut names: Vec<String> = assigned
                .into_iter()
                .filter(|n| env.contains_key(n))
                .collect();
            names.sort();

            let mut loop_env = env.clone();
            let mut specs = Vec::new();
            for name in names {
                let incoming = env[&name].clone();
                *counter += 1;
                let dest = format!("t{counter}");
                loop_env.insert(name.clone(), dest.clone());
                specs.push((name, incoming, dest));
            }

            let mut cond_insts = Vec::new();
            let mut cond_env = loop_env.clone();
            let c = lower_expr(
                cond,
                &mut cond_insts,
                &mut cond_env,
                counter,
                regions,
                tensors,
                dim_params,
                fn_names,
                strings,
            );
            let mut body_insts = Vec::new();
            let mut body_env = loop_env;
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
                    strings,
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
                    strings,
                );
            }
            let mut carried = Vec::new();
            for (name, incoming, dest) in specs {
                let updated = body_env.get(&name).cloned().unwrap_or_else(|| incoming.clone());
                let updated = if updated == dest {
                    incoming.clone()
                } else {
                    updated
                };
                if let Some(info) = tensors
                    .get(&updated)
                    .cloned()
                    .or_else(|| tensors.get(&incoming).cloned())
                {
                    tensors.insert(dest.clone(), info);
                }
                env.insert(name, dest.clone());
                carried.push(WhileCarry {
                    dest,
                    incoming,
                    updated,
                });
            }
            instructions.push(IrInst::While {
                cond_insts,
                cond: c,
                body: body_insts,
                carried,
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
            place: annotated
                .and_then(|t| match t {
                    Type::Tensor { place, .. } => Some(place.clone()),
                    _ => None,
                })
                .unwrap_or(Place::Cpu),
            concrete,
        })
    } else if let Some((rows, cols)) = lit {
        Some(TensorInfo {
            elem: TensorElem::F32,
            dims: vec![Dim::Static(rows), Dim::Static(cols)],
            place: Place::Cpu,
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

/// Cada `elsif` baja como un `if` anidado en la rama else, en el mismo orden.
fn else_block_for_elsifs(else_block: Option<Block>, elsifs: &[Elsif]) -> Option<Block> {
    if elsifs.is_empty() {
        return else_block;
    }
    let mut else_b = else_block;
    for arm in elsifs.iter().rev() {
        let nested = Expr::If {
            cond: Box::new(arm.cond.clone()),
            then_block: arm.body.clone(),
            elsifs: Vec::new(),
            else_block: else_b,
            span: arm.span,
        };
        else_b = Some(Block {
            stmts: Vec::new(),
            tail: Some(Box::new(nested)),
            span: arm.span,
        });
    }
    else_b
}

fn lower_expr_simple(
    e: &Expr,
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    tensors: &mut HashMap<String, TensorInfo>,
    fn_names: &HashMap<String, ()>,
    strings: &mut StringEnv,
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
        strings,
    )
}

/// Runtime container kind: Int 0, Float 1, Bool 2, String 3.
fn scalar_kind(ty: &crate::ast::Type) -> i64 {
    match ty {
        crate::ast::Type::Named { name, .. } => match name.as_str() {
            "Float" => 1,
            "Bool" => 2,
            "String" => 3,
            _ => 0,
        },
        _ => 0,
    }
}

fn container_kinds(fname: &str, type_args: &[crate::ast::TypeArg]) -> Vec<i64> {
    let kinds: Vec<i64> = type_args
        .iter()
        .filter_map(|a| match a {
            crate::ast::TypeArg::Type(t) => Some(scalar_kind(t)),
            crate::ast::TypeArg::Dim(_) => None,
        })
        .collect();
    if fname == "dict_new" {
        let k = kinds.first().copied().unwrap_or(0);
        let v = kinds.get(1).copied().unwrap_or(0);
        vec![k, v]
    } else {
        vec![kinds.first().copied().unwrap_or(0)]
    }
}

fn runtime_call_name(fname: &str, fn_names: &HashMap<String, ()>) -> String {
    // List ops always hit the runtime; prelude bodies are signatures only.
    if matches!(
        fname,
        "list_new" | "list_push" | "list_len" | "list_get" | "dict_new" | "dict_put" | "dict_get"
    ) {
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
            | "where"
            | "index"
            | "read_file"
            | "write_file"
            | "path_readable"
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
            | "str_bytes"
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
            | "str_from_int"
            | "str_as_int"
            | "str_to_f64_bits"
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
            | "dict_new"
            | "dict_put"
            | "dict_get"
            | "clang"
            | "clang_obj"
            | "link_objs"
            | "realpath"
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
    strings: &mut StringEnv,
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
        Expr::Float { value, .. } => {
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::ConstFloat {
                dest: dest.clone(),
                value: *value,
            });
            dest
        }
        Expr::Ident { name, .. } => env.get(name).cloned().unwrap_or_else(|| name.clone()),
        Expr::Binary {
            op,
            left,
            right,
            ..
        } => {
            // `x = x + ...`: only the left spine of the chain may see the accumulator.
            let acc = strings.accumulate.take();
            let is_concat = *op == BinOp::Add && strings.add_is_string_concat(e);
            if is_concat && matches!(left.as_ref(), Expr::Binary { op: BinOp::Add, .. }) {
                strings.accumulate = acc.clone();
            }
            let l = lower_expr_simple(left, instructions, env, counter, tensors, fn_names, strings);
            strings.accumulate = None;
            let r = lower_expr_simple(right, instructions, env, counter, tensors, fn_names, strings);
            *counter += 1;
            let dest = format!("t{counter}");
            if is_concat {
                let into_acc = matches!(
                    (left.as_ref(), &acc),
                    (Expr::Ident { name, .. }, Some(a)) if name == a
                );
                let func = if into_acc || strings.add_uses_append(e) {
                    "sal_str_append"
                } else {
                    "sal_str_concat"
                };
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: func.into(),
                    args: vec![l, r.clone()],
                });
                if add_right_is_fresh_temp(right, strings) {
                    instructions.push(IrInst::Call {
                        dest: None,
                        func: "sal_free".into(),
                        args: vec![r],
                    });
                }
                return dest;
            }
            if let Some(tb) = try_tensor_bin(
                op,
                &l,
                left,
                &r,
                right,
                dest.clone(),
                tensors,
                env,
                dim_params,
                instructions,
                counter,
            ) {
                return tb;
            }
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
        Expr::Unary { op, expr, .. } => {
            let inner = lower_expr_simple(expr, instructions, env, counter, tensors, fn_names, strings);
            *counter += 1;
            let dest = format!("t{counter}");
            if let Some(info) = tensor_info_for_ssa(&inner, env, tensors) {
                let len = tensor_len_operand(&info, &inner, dim_params, instructions, counter);
                let place = tensor_place_name(&info, env);
                let op_s = match op {
                    UnOp::Neg => "neg",
                    UnOp::Not => "not",
                };
                instructions.push(IrInst::TensorUnary {
                    dest: dest.clone(),
                    op: op_s.into(),
                    elem: info.elem,
                    place,
                    input: inner,
                    len,
                });
                let out_info = if op_s == "not" {
                    TensorInfo {
                        elem: TensorElem::I8,
                        dims: info.dims.clone(),
                        place: info.place.clone(),
                        concrete: info.concrete.clone(),
                    }
                } else {
                    info.clone()
                };
                tensors.insert(dest.clone(), out_info);
                return dest;
            }
            let op_s = match op {
                UnOp::Neg => "neg",
                UnOp::Not => "not",
            };
            instructions.push(IrInst::Binary {
                dest: dest.clone(),
                op: op_s.into(),
                left: inner,
                right: "0".into(),
            });
            dest
        }
        Expr::Call {
            func,
            type_args,
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
                    strings,
                ));
            }
            *counter += 1;
            let dest = format!("t{counter}");
            if fname == "load" {
                if let Some(Expr::String { value, .. }) = args.first() {
                    instructions.push(IrInst::Call {
                        dest: Some(dest.clone()),
                        func: "sal_load".into(),
                        args: vec![format!("\"{value}\""), "cpu".into()],
                    });
                    return dest;
                }
            }
            if fname == "list_new" || fname == "dict_new" {
                let kinds = container_kinds(&fname, type_args);
                let mut call_args = Vec::new();
                for k in kinds {
                    *counter += 1;
                    let kd = format!("t{counter}");
                    instructions.push(IrInst::ConstInt {
                        dest: kd.clone(),
                        value: k,
                    });
                    call_args.push(kd);
                }
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: format!("sal_{fname}"),
                    args: call_args,
                });
                return dest;
            }
            if let Some(sdef) = strings.structs.get(&fname) {
                if sdef.type_params.is_empty() && arg_names.len() == sdef.fields.len() {
                    if is_transparent_struct(sdef) {
                        return arg_names.into_iter().next().unwrap_or(dest);
                    }
                    instructions.push(IrInst::Aggregate {
                        dest: dest.clone(),
                        struct_name: fname,
                        fields: arg_names,
                    });
                    return dest;
                }
            }
            if let Some((edef, vdef)) = find_variant_in_enums(&fname, &strings.enums) {
                let vidx = enum_variant_index(edef, &vdef.name).unwrap_or(0) as u32;
                let payload = if arg_names.is_empty() {
                    None
                } else {
                    Some(arg_names[0].clone())
                };
                instructions.push(IrInst::EnumMake {
                    dest: dest.clone(),
                    enum_name: edef.name.clone(),
                    variant_index: vidx,
                    payload,
                });
                return dest;
            }
            if fname == "where" && arg_names.len() == 2 {
                if let Some(frame_ty) = frame_name_for_where(&args[0], strings) {
                    let frame_ssa = arg_names[0].clone();
                    let mask_ssa = arg_names[1].clone();
                    if let Some(info) = tensor_info_for_ssa(&mask_ssa, env, tensors) {
                        let len = tensor_len_operand(
                            &info,
                            &mask_ssa,
                            dim_params,
                            instructions,
                            counter,
                        );
                        instructions.push(IrInst::Call {
                            dest: Some(dest.clone()),
                            func: "sal_where".into(),
                            args: vec![
                                frame_ssa,
                                mask_ssa,
                                len,
                                format!("\"{frame_ty}\""),
                            ],
                        });
                        return dest;
                    }
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
        Expr::On {
            place,
            kernel_index,
            body,
            ..
        } => {
            if let Some(ki) = kernel_index {
                let place_s = place_str(place);
                let bounds = kernel_bounds(ki, tensors, env);
                *counter += 1;
                let dest = format!("t{counter}");
                let mut kenv = env.clone();
                for n in &ki.names {
                    kenv.insert(n.clone(), format!("__{n}"));
                }
                let mut body_insts = Vec::new();
                let mut kstrings = strings.clone();
                for st in &body.stmts {
                    lower_stmt(
                        st,
                        &mut body_insts,
                        &mut kenv,
                        counter,
                        regions,
                        tensors,
                        dim_params,
                        fn_names,
                        &mut kstrings,
                    );
                }
                let tail = if let Some(t) = &body.tail {
                    lower_expr(
                        t,
                        &mut body_insts,
                        &mut kenv,
                        counter,
                        regions,
                        tensors,
                        dim_params,
                        fn_names,
                        &mut kstrings,
                    )
                } else {
                    let z = format!("t{}", *counter + 1);
                    *counter += 1;
                    body_insts.push(IrInst::ConstInt {
                        dest: z.clone(),
                        value: 0,
                    });
                    z
                };
                instructions.push(IrInst::KernelGrid {
                    place: place_s,
                    index_names: ki.names.clone(),
                    bounds,
                    body: body_insts,
                    tail,
                    dest: dest.clone(),
                });
                return dest;
            }
            let place_s = place_str(place);
            let mut ops = Vec::new();
            if let Some(t) = &body.tail {
                collect_fused_ops(t, &mut ops, env, tensors);
            }
            let has_epilogue = ops
                .iter()
                .any(|o| matches!(o, FusedOp::MapEpilogue { .. }));
            let has_column = ops
                .iter()
                .any(|o| matches!(o, FusedOp::ColumnBin { .. }));
            let fused = has_epilogue
                || has_column
                || ops.len() > 1
                || ops.iter().any(|o| matches!(o, FusedOp::Matmul { .. }));
            let (peak_bytes, peak_symbolic) = estimate_peak_for_region(&ops, tensors, &place_s);
            regions.push(FusedRegion {
                place: place_s.clone(),
                ops: ops.clone(),
                peak_bytes,
                peak_symbolic,
                fused,
            });

            if has_column {
                *counter += 1;
                return format!("t{counter}");
            }
            if let Some(FusedOp::Matmul { lhs, rhs, .. }) = ops
                .iter()
                .find(|o| matches!(o, FusedOp::Matmul { .. }))
            {
                *counter += 1;
                let dest = format!("t{counter}");
                let (m, k, n) = emit_matmul_dim_args(
                    lhs,
                    rhs,
                    tensors,
                    dim_params,
                    instructions,
                    counter,
                );
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: "sal_matmul_f32".into(),
                    args: vec![lhs.clone(), rhs.clone(), m, k, n],
                });
                if has_epilogue {
                    *counter += 1;
                    let out = format!("t{counter}");
                    instructions.push(IrInst::Call {
                        dest: Some(out.clone()),
                        func: "sal_relu".into(),
                        args: vec![dest],
                    });
                    return out;
                }
                return dest;
            }

            let mut on_strings = strings.clone();
            for st in &body.stmts {
                lower_stmt(
                    st,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    &mut on_strings,
                );
            }
            if let Some(t) = &body.tail {
                lower_expr(
                    t,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    &mut on_strings,
                )
            } else {
                *counter += 1;
                let dest = format!("t{counter}");
                instructions.push(IrInst::ConstInt {
                    dest: dest.clone(),
                    value: 0,
                });
                dest
            }
        }
        Expr::To { place, expr, .. } => {
            let src = lower_expr_simple(expr, instructions, env, counter, tensors, fn_names, strings);
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
            elsifs,
            else_block,
            ..
        } => {
            let else_block = else_block_for_elsifs(else_block.clone(), elsifs);
            let c = lower_expr_simple(cond, instructions, env, counter, tensors, fn_names, strings);
            let mut then_body = Vec::new();
            let mut else_body = Vec::new();
            let mut then_env = env.clone();
            let mut else_env = env.clone();
            let mut then_strings = strings.clone();
            let mut else_strings = strings.clone();
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
                    &mut then_strings,
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
                    &mut then_strings,
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
                        &mut else_strings,
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
                        &mut else_strings,
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
            let parent_env = env.clone();
            let carried = merge_if_carries(&parent_env, &then_env, &else_env, counter, env, tensors);
            instructions.push(IrInst::If {
                cond: c,
                then_body,
                else_body,
                then_val,
                else_val,
                dest: dest.clone(),
                carried,
            });
            dest
        }
        Expr::Field { base, field, .. } => {
            let base_ssa = lower_expr_simple(
                base,
                instructions,
                env,
                counter,
                tensors,
                fn_names,
                strings,
            );
            if let Some(name) = struct_name_of_base(base, strings) {
                if let Some(sdef) = strings.structs.get(&name) {
                    if is_transparent_struct(sdef) {
                        return base_ssa;
                    }
                    if let Some(idx) = struct_field_index(sdef, field) {
                        *counter += 1;
                        let dest = format!("t{counter}");
                        instructions.push(IrInst::Extract {
                            dest: dest.clone(),
                            base: base_ssa,
                            struct_name: name,
                            field_index: idx as u32,
                        });
                        return dest;
                    }
                }
            }
            base_ssa
        }
        Expr::Match { scrutinee, arms, .. } => {
            let scrut = lower_expr_simple(
                scrutinee,
                instructions,
                env,
                counter,
                tensors,
                fn_names,
                strings,
            );
            lower_match_chain(
                scrut,
                arms,
                instructions,
                env,
                counter,
                regions,
                tensors,
                dim_params,
                fn_names,
                strings,
            )
        }
        _ => {
            *counter += 1;
            format!("t{counter}")
        }
    }
}

fn find_variant_in_enums<'a>(
    variant: &str,
    enums: &'a HashMap<String, crate::ast::EnumDef>,
) -> Option<(&'a crate::ast::EnumDef, &'a crate::ast::EnumVariant)> {
    for edef in enums.values() {
        if let Some(v) = edef.variants.iter().find(|v| v.name == variant) {
            return Some((edef, v));
        }
    }
    None
}

fn struct_name_of_base(base: &Expr, strings: &StringEnv) -> Option<String> {
    match base {
        Expr::Ident { name, .. } => strings.struct_types.get(name).cloned(),
        _ => None,
    }
}

fn frame_name_for_where(expr: &Expr, strings: &StringEnv) -> Option<String> {
    match expr {
        Expr::Ident { name, .. } => strings.struct_types.get(name).cloned(),
        Expr::Call { func, .. } => {
            let Expr::Ident { name, .. } = func.as_ref() else {
                return None;
            };
            if strings.structs.contains_key(name) {
                Some(name.clone())
            } else {
                None
            }
        }
        _ => strings.expr_type(expr).and_then(|ty| {
            if let crate::ast::Type::Named { name, args, .. } = ty {
                if args.is_empty() && strings.structs.contains_key(name) {
                    return Some(name.clone());
                }
            }
            None
        }),
    }
}

fn lower_match_chain(
    scrut: String,
    arms: &[MatchArm],
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    regions: &mut Vec<FusedRegion>,
    tensors: &mut HashMap<String, TensorInfo>,
    dim_params: &[String],
    fn_names: &HashMap<String, ()>,
    strings: &mut StringEnv,
) -> String {
    if arms.is_empty() {
        *counter += 1;
        return format!("t{counter}");
    }
    if arms.iter().all(|a| {
        matches!(
            a.pattern,
            Pattern::Variant { .. } | Pattern::Ident(_, _) | Pattern::Wild(_)
        )
    }) {
        return lower_match_switch(
            scrut,
            arms,
            instructions,
            env,
            counter,
            regions,
            tensors,
            dim_params,
            fn_names,
            strings,
        );
    }
    let arm = &arms[0];
    let rest = &arms[1..];
    let (cond_ssa, mut arm_env) = match_arm_cond(&scrut, &arm.pattern, counter, instructions);
    for (k, v) in env.iter() {
        arm_env.entry(k.clone()).or_insert_with(|| v.clone());
    }
    let mut then_body = Vec::new();
    let then_val = lower_expr(
        &arm.body,
        &mut then_body,
        &mut arm_env,
        counter,
        regions,
        tensors,
        dim_params,
        fn_names,
        strings,
    );
    if rest.is_empty() {
        *counter += 1;
        let dest = format!("t{counter}");
        instructions.push(IrInst::If {
            cond: cond_ssa,
            then_body,
            else_body: Vec::new(),
            then_val,
            else_val: "0".into(),
            dest: dest.clone(),
            carried: Vec::new(),
        });
        return dest;
    }
    let else_val = lower_match_chain(
        scrut.clone(),
        rest,
        instructions,
        env,
        counter,
        regions,
        tensors,
        dim_params,
        fn_names,
        strings,
    );
    *counter += 1;
    let dest = format!("t{counter}");
    instructions.push(IrInst::If {
        cond: cond_ssa,
        then_body,
        else_body: Vec::new(),
        then_val,
        else_val,
        dest: dest.clone(),
        carried: Vec::new(),
    });
    dest
}

fn lower_match_switch(
    scrut: String,
    arms: &[MatchArm],
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    regions: &mut Vec<FusedRegion>,
    tensors: &mut HashMap<String, TensorInfo>,
    dim_params: &[String],
    fn_names: &HashMap<String, ()>,
    strings: &mut StringEnv,
) -> String {
    let mut switch_arms = Vec::new();
    let mut default_body = Vec::new();
    let mut default_val = "0".to_string();
    for arm in arms {
        match &arm.pattern {
            Pattern::Wild(_) => {
                let mut arm_env = env.clone();
                default_val = lower_expr(
                    &arm.body,
                    &mut default_body,
                    &mut arm_env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    strings,
                );
            }
            Pattern::Ident(_, _) => {
                let mut arm_env = env.clone();
                let mut body = Vec::new();
                let value = lower_expr(
                    &arm.body,
                    &mut body,
                    &mut arm_env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    strings,
                );
                switch_arms.push(SwitchArm {
                    tag: 0,
                    body,
                    value,
                });
            }
            Pattern::Variant { name, args, .. } => {
                let tag = variant_tag_for_name(name) + 1;
                let mut body = Vec::new();
                let mut arm_env = env.clone();
                if let Some(Pattern::Ident(bind, _)) = args.first() {
                    *counter += 1;
                    let payload = format!("t{counter}");
                    body.push(IrInst::Binary {
                        dest: payload.clone(),
                        op: "div".into(),
                        left: scrut.clone(),
                        right: "256".into(),
                    });
                    arm_env.insert(bind.clone(), payload);
                }
                let value = lower_expr(
                    &arm.body,
                    &mut body,
                    &mut arm_env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    strings,
                );
                switch_arms.push(SwitchArm { tag, body, value });
            }
            _ => {}
        }
    }
    *counter += 1;
    let dest = format!("t{counter}");
    instructions.push(IrInst::Switch {
        scrut,
        arms: switch_arms,
        default_body,
        default_val,
        dest: dest.clone(),
    });
    dest
}

fn match_arm_cond(
    scrut: &str,
    pat: &Pattern,
    counter: &mut u32,
    instructions: &mut Vec<IrInst>,
) -> (String, HashMap<String, String>) {
    let mut env = HashMap::new();
    match pat {
        Pattern::Wild(_) => {
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::ConstInt {
                dest: dest.clone(),
                value: 1,
            });
            (dest, env)
        }
        Pattern::Int(v, _) => {
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::Binary {
                dest: dest.clone(),
                op: "eq".into(),
                left: scrut.to_string(),
                right: format!("{v}"),
            });
            (dest, env)
        }
        Pattern::Ident(name, _) => {
            // Unit variant (e.g. None): tag == 0
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::Binary {
                dest: dest.clone(),
                op: "eq".into(),
                left: scrut.to_string(),
                right: "0".into(),
            });
            let _ = name;
            (dest, env)
        }
        Pattern::Variant { name, args, .. } => {
            let vtag = variant_tag_for_name(name) + 1;
            *counter += 1;
            let low = format!("t{counter}");
            instructions.push(IrInst::BitAnd {
                dest: low.clone(),
                left: scrut.to_string(),
                right: "255".into(),
            });
            *counter += 1;
            let tag_tmp = format!("t{counter}");
            instructions.push(IrInst::Binary {
                dest: tag_tmp.clone(),
                op: "eq".into(),
                left: low,
                right: vtag.to_string(),
            });
            if let Some(Pattern::Ident(bind, _)) = args.first() {
                *counter += 1;
                let payload = format!("t{counter}");
                instructions.push(IrInst::Binary {
                    dest: payload.clone(),
                    op: "div".into(),
                    left: scrut.to_string(),
                    right: "256".into(),
                });
                env.insert(bind.clone(), payload);
            }
            (tag_tmp, env)
        }
    }
}

fn variant_tag_for_name(name: &str) -> i64 {
    match name {
        "None" => 0,
        "Some" => 1,
        "Ok" => 1,
        "Err" => 2,
        _ => 1,
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

fn tensor_info_for_ssa(
    ssa: &str,
    env: &HashMap<String, String>,
    tensors: &HashMap<String, TensorInfo>,
) -> Option<TensorInfo> {
    if let Some(i) = tensors.get(ssa) {
        return Some(i.clone());
    }
    env.get(ssa)
        .and_then(|n| tensors.get(n))
        .cloned()
}

fn tensor_place_name(info: &TensorInfo, _env: &HashMap<String, String>) -> String {
    place_str(&info.place)
}

fn tensor_len_operand(
    info: &TensorInfo,
    tensor_ssa: &str,
    dim_params: &[String],
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> String {
    if info.dims.len() == 1 {
        return axis_len_ssa(
            Some(info),
            tensor_ssa,
            false,
            dim_params,
            instructions,
            counter,
        );
    }
    match dims_elems(info) {
        Ok(n) => push_const_dim(n, instructions, counter),
        Err(sym) => {
            let _ = sym;
            push_const_dim(1, instructions, counter)
        }
    }
}

fn expr_has_tensor(e: &Expr, env: &HashMap<String, String>, tensors: &HashMap<String, TensorInfo>) -> bool {
    match e {
        Expr::Ident { name, .. } => {
            tensors.contains_key(name)
                || env
                    .get(name)
                    .and_then(|n| tensors.get(n))
                    .is_some()
        }
        Expr::TensorLit { .. } => true,
        Expr::Binary { left, right, .. } => {
            expr_has_tensor(left, env, tensors) || expr_has_tensor(right, env, tensors)
        }
        _ => false,
    }
}

fn try_tensor_bin(
    op: &BinOp,
    l_ssa: &str,
    left: &Expr,
    r_ssa: &str,
    right: &Expr,
    dest: String,
    tensors: &mut HashMap<String, TensorInfo>,
    env: &HashMap<String, String>,
    dim_params: &[String],
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> Option<String> {
    if !expr_has_tensor(left, env, tensors) && !expr_has_tensor(right, env, tensors) {
        return None;
    }
    let (tensor_side, other_ssa, tensor_expr) = if expr_has_tensor(left, env, tensors) {
        (l_ssa, r_ssa, left)
    } else {
        (r_ssa, l_ssa, right)
    };
    let info = tensor_info_for_ssa(tensor_side, env, tensors)?;
    let len = tensor_len_operand(&info, tensor_side, dim_params, instructions, counter);
    let place = tensor_place_name(&info, env);
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
    let right_scalar = !expr_has_tensor(
        if std::ptr::eq(tensor_expr, left) {
            right
        } else {
            left
        },
        env,
        tensors,
    );
    instructions.push(IrInst::TensorBin {
        dest: dest.clone(),
        op: op_s.into(),
        elem: info.elem,
        place,
        left: l_ssa.to_string(),
        right: other_ssa.to_string(),
        len,
        right_scalar,
    });
    let out_info = if matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    ) {
        TensorInfo {
            elem: TensorElem::I8,
            dims: info.dims.clone(),
            place: info.place.clone(),
            concrete: info.concrete.clone(),
        }
    } else {
        info.clone()
    };
    tensors.insert(dest.clone(), out_info);
    Some(dest)
}

fn collect_fused_ops(
    e: &Expr,
    ops: &mut Vec<FusedOp>,
    env: &HashMap<String, String>,
    tensors: &HashMap<String, TensorInfo>,
) {
    match e {
        Expr::Binary { op, left, right, .. } => {
            collect_fused_ops(left, ops, env, tensors);
            collect_fused_ops(right, ops, env, tensors);
            if expr_has_tensor(left, env, tensors) || expr_has_tensor(right, env, tensors) {
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
                let scalar = if expr_has_tensor(left, env, tensors) {
                    if expr_has_tensor(right, env, tensors) {
                        None
                    } else {
                        Some(expr_id(right, env))
                    }
                } else {
                    Some(expr_id(left, env))
                };
                ops.push(FusedOp::ColumnBin {
                    op: op_s.into(),
                    left: expr_id(left, env),
                    right: expr_id(right, env),
                    dest: format!("col_{}", ops.len()),
                    scalar,
                });
            }
        }
        Expr::Call { func, args, .. } => {
            for a in args {
                collect_fused_ops(a, ops, env, tensors);
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
        place: lhs.place.clone(),
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
            FusedOp::ColumnBin { left, .. } => {
                if let Some(info) = tensors.get(left) {
                    saw_shaped = true;
                    match tensor_nbytes(info) {
                        Ok(n) => static_total = static_total.saturating_add(n.saturating_mul(2)),
                        Err(s) => sym_parts.push(format!("col:{s}")),
                    }
                } else {
                    sym_parts.push(format!("column_bin({left}):symbolic"));
                }
            }
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

fn kernel_bounds(
    ki: &KernelIndex,
    tensors: &HashMap<String, TensorInfo>,
    env: &HashMap<String, String>,
) -> Vec<u64> {
    let dims = match &ki.shape {
        KernelShape::Type(Type::Tensor { dims, .. }) => dims.clone(),
        KernelShape::Binding(name) => env
            .get(name)
            .and_then(|v| tensors.get(v))
            .map(|t| t.dims.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    dims.iter()
        .map(|d| match d {
            Dim::Static(n) => *n,
            Dim::Dynamic => 1,
        })
        .collect()
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
                    FusedOp::ColumnBin {
                        op,
                        left,
                        right,
                        dest,
                        scalar,
                    } => {
                        s.push_str(&format!(
                            "    ColumnBin {{ op: \"{op}\", left: \"{left}\", right: \"{right}\", dest: \"{dest}\", scalar: {scalar:?} }}\n"
                        ));
                    }
                    other => s.push_str(&format!("    {other:?}\n")),
                }
            }
        }
    }
    s
}
