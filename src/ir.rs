use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ast::*;
use crate::layout::{enum_variant_index, is_transparent_struct, struct_field_index};
use crate::string_expr::{
    add_chain_leftmost, add_left_is_fresh_call, add_right_is_fresh_temp, StringEnv,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IrModule {
    pub functions: Vec<IrFunction>,
    #[serde(default)]
    pub any_vtables: Vec<AnyVtableDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnyVtableDef {
    pub type_name: String,
    pub protocol: String,
    pub global: String,
    pub entries: Vec<String>,
}

/// Host `tensor[[…]]` literal materialised in LLVM with this SSA name (`tN`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IrHostLiteral {
    pub ir_name: String,
    pub rows: u64,
    pub cols: u64,
    pub data: Vec<f32>,
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
    /// Tensor literals lowered in this function (`tN` matches `lower_expr` / `env`).
    #[serde(default)]
    pub host_literals: Vec<IrHostLiteral>,
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
    AnyBox {
        dest: String,
        vtable: String,
        payload: String,
        drop_slot: u32,
    },
    AnyCall {
        dest: Option<String>,
        receiver: String,
        slot: u32,
        args: Vec<String>,
    },
    PlaceCopy {
        dest: String,
        from: String,
        to_place: String,
    },
    /// Same storage as `src`, new shape metadata only (reshape / transpose view).
    TensorRebind { dest: String, src: String },
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
    /// Enum as one i64: a unit variant is its tag alone; a variant with fields is
    /// a heap box `[tag, field0, field1, …]` and the i64 is its address.
    EnumMake {
        dest: String,
        enum_name: String,
        variant_index: u32,
        payload: Vec<String>,
        /// Two bits per field: 1 owned `String`, 2 owned enum box; `enum_free` follows them.
        owned: u64,
    },
    /// Tag of an enum value, whether unit (the i64 itself) or boxed.
    EnumTag {
        dest: String,
        value: String,
    },
    /// Field `index` of a boxed enum value.
    EnumField {
        dest: String,
        value: String,
        index: u32,
    },
    /// `match` on an enum: dispatch on the low tag byte.
    Switch {
        scrut: String,
        arms: Vec<SwitchArm>,
        default_body: Vec<IrInst>,
        default_val: String,
        dest: String,
    },
    /// Call of a function that returns a scalarised struct (see `scalarize`): the callee
    /// writes its slots through a hidden buffer and `dests` receive them. Only produced
    /// just before LLVM emission, never by lowering.
    CallSret {
        dests: Vec<String>,
        func: String,
        args: Vec<String>,
    },
    /// Store the slots of the struct a function returns into its hidden `sret` buffer.
    SretStore { values: Vec<String> },
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
    mut strings: StringEnv,
) -> IrModule {
    let fn_names = callable_fns;
    let moded = crate::ownership::infer_param_modes(prog);
    let dropped = crate::drops::insert_drops(&moded, &strings.types, &strings.structs, &strings.enums);
    // The field-read temporaries the drops pass made have struct types the lowering needs.
    strings.struct_types.extend(crate::drops::tmp_struct_types());
    let prog = &dropped;
    let mut param_modes: HashMap<String, Vec<ParamMode>> = HashMap::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            // A function that releases the fields of a struct parameter owns them from
            // then on: for the callers' struct glue it takes the argument, even though
            // the parameter is only borrowed (the callee's own glue stays as it is).
            let releasing = releases_param_fields(f);
            let handed = params_string_field_hand_out(f, &strings.structs);
            param_modes.insert(
                f.name.clone(),
                f.params
                    .iter()
                    .map(|p| {
                        if releasing.contains(&p.name) || handed.contains(&p.name) {
                            ParamMode::Take
                        } else {
                            p.mode
                        }
                    })
                    .collect(),
            );
        }
    }
    let mut functions = Vec::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            functions.push(lower_fn(f, &fn_names, &strings, &param_modes));
        }
    }
    IrModule {
        functions,
        any_vtables: collect_any_vtables(prog),
    }
}

fn collect_any_vtables(prog: &Program) -> Vec<AnyVtableDef> {
    let mut protocols = HashMap::new();
    for item in &prog.items {
        if let Item::Protocol(p) = item {
            protocols.insert(p.name.clone(), p.clone());
        }
    }
    let mut out = Vec::new();
    for item in &prog.items {
        let (type_name, claims) = match item {
            Item::Struct(s) if s.type_params.is_empty() => (s.name.as_str(), &s.protocols),
            Item::Enum(e) if e.type_params.is_empty() => (e.name.as_str(), &e.protocols),
            _ => continue,
        };
        for claim in claims {
            let Some(pdef) = protocols.get(&claim.name) else {
                continue;
            };
            out.push(AnyVtableDef {
                type_name: type_name.to_string(),
                protocol: claim.name.clone(),
                global: crate::protocols::vtable_global(type_name, &claim.name),
                entries: crate::protocols::vtable_entries(type_name, pdef),
            });
        }
    }
    out
}

/// The parameters of `f` whose fields it passes to a releasing builtin
/// (`list_free(p.xs)`, `free(p.name)`, ...).
fn releases_param_fields(f: &FnDef) -> HashSet<String> {
    fn root(e: &Expr) -> Option<&str> {
        let Expr::Field { base, .. } = e else { return None };
        match base.as_ref() {
            Expr::Ident { name, .. } => Some(name),
            other => root(other),
        }
    }
    fn blk(b: &Block, out: &mut HashSet<String>) {
        for st in &b.stmts {
            match st {
                Stmt::Break { .. } => {}
                Stmt::Let { init, .. } => ex(init, out),
                Stmt::Assign { target, value, .. } => {
                    ex(target, out);
                    ex(value, out);
                }
                Stmt::Return { value, .. } => {
                    if let Some(v) = value {
                        ex(v, out);
                    }
                }
                Stmt::Expr(e) => ex(e, out),
                Stmt::While { cond, body, .. } => {
                    ex(cond, out);
                    blk(body, out);
                }
            }
        }
        if let Some(t) = &b.tail {
            ex(t, out);
        }
    }
    fn ex(e: &Expr, out: &mut HashSet<String>) {
        match e {
            Expr::Call { func, args, .. } => {
                if let Expr::Ident { name, .. } = func.as_ref() {
                    if matches!(
                        name.as_str(),
                        "free" | "list_free" | "dict_free" | "bytes_free" | "enum_free" | "struct_free" | "struct_free_box" | "tensor_free" | "any_free"
                    ) {
                        for a in args {
                            if let Some(r) = root(a) {
                                out.insert(r.to_string());
                            }
                        }
                    }
                }
                ex(func, out);
                for a in args {
                    ex(a, out);
                }
            }
            Expr::Binary { left, right, .. } => {
                ex(left, out);
                ex(right, out);
            }
            Expr::Unary { expr, .. }
            | Expr::To { expr, .. }
            | Expr::Try { expr, .. }
            | Expr::Field { base: expr, .. } => ex(expr, out),
            Expr::Block(b) | Expr::On { body: b, .. } => blk(b, out),
            Expr::If { cond, then_block, elsifs, else_block, .. } => {
                ex(cond, out);
                blk(then_block, out);
                for a in elsifs {
                    ex(&a.cond, out);
                    blk(&a.body, out);
                }
                if let Some(b) = else_block {
                    blk(b, out);
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                ex(scrutinee, out);
                for a in arms {
                    ex(&a.body, out);
                }
            }
            Expr::Lambda { body, .. } => ex(body, out),
            _ => {}
        }
    }
    let params: HashSet<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let mut all = HashSet::new();
    blk(&f.body, &mut all);
    all.retain(|n| params.contains(n.as_str()));
    all
}

/// Parameters whose `String` field is returned by value (`return p.name`).
fn params_string_field_hand_out(
    f: &FnDef,
    structs: &HashMap<String, crate::ast::StructDef>,
) -> HashSet<String> {
    let param_names: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    let mut out = HashSet::new();
    let mut note = |e: &Expr| {
        let mut fields = Vec::new();
        let Some(root) = field_chain_root(e, &mut fields) else {
            return;
        };
        if !param_names.contains(root) {
            return;
        }
        let Some(mut ty) = f.params.iter().find(|p| p.name == root).map(|p| p.ty.clone()) else {
            return;
        };
        for fname in &fields {
            ty = match struct_field_type(&ty, fname, structs) {
                Some(t) => t,
                None => return,
            };
        }
        if matches!(
            ty,
            crate::ast::Type::Named { name, args, .. }
                if name == "String" && args.is_empty()
        ) {
            out.insert(root.to_string());
        }
    };
    for st in &f.body.stmts {
        if let Stmt::Return { value: Some(v), .. } = st {
            note(v);
        }
    }
    if let Some(t) = &f.body.tail {
        note(t);
    }
    out
}

fn struct_field_type(
    ty: &crate::ast::Type,
    field: &str,
    structs: &HashMap<String, crate::ast::StructDef>,
) -> Option<crate::ast::Type> {
    let crate::ast::Type::Named { name, args, .. } = ty else {
        return None;
    };
    let sdef = structs.get(name)?;
    if !args.is_empty() {
        return None;
    }
    sdef.fields.iter().find(|f| f.name == field).map(|f| f.ty.clone())
}

fn field_chain_root<'a>(e: &'a Expr, fields: &mut Vec<&'a str>) -> Option<&'a str> {
    match e {
        Expr::Field { base, field, .. } => {
            let root = field_chain_root(base, fields)?;
            fields.push(field);
            Some(root)
        }
        Expr::Ident { name, .. } => Some(name),
        _ => None,
    }
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
    let mut host_literals: Vec<IrHostLiteral> = Vec::new();
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
                        dim_params.push(name.clone());
                    }
                    if !env.contains_key(&name) {
                        // The axis is a dim_param, not a constant. Selfhost names it as-is.
                        env.insert(name.clone(), name);
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

    let mut fn_diverged = false;
    for st in &f.body.stmts {
        if lower_stmt(
            st,
            &mut instructions,
            &mut env,
            &mut counter,
            &mut regions,
            &mut tensors,
            &mut dim_params,
            fn_names,
            &mut host_literals,
            &mut strings,
        ) {
            fn_diverged = true;
            break;
        }
    }
    if !fn_diverged {
        if let Some(t) = &f.body.tail {
            let v = lower_expr(
                t,
                &mut instructions,
                &mut env,
                &mut counter,
                &mut regions,
                &mut tensors,
                &mut dim_params,
                fn_names,
                &mut host_literals,
                &mut strings,
            );
            instructions.push(IrInst::Return { value: v });
        }
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
        host_literals,
    }
}

/// Struct locals bound to `list_get(xs, i)` whose element struct is not describable. Such a
/// list hands out its boxes, not copies, so the glue must not release those locals' fields.
fn list_alias_locals(f: &FnDef, strings: &StringEnv) -> HashSet<String> {
    fn note(name: &str, init: &Expr, strings: &StringEnv, out: &mut HashSet<String>) {
        let Expr::Call { func, .. } = init else {
            return;
        };
        if !matches!(func.as_ref(), Expr::Ident { name, .. } if name == "list_get") {
            return;
        }
        let Some(sn) = strings.struct_types.get(name) else {
            return;
        };
        if struct_desc(sn, &strings.structs, &strings.enums).is_none() {
            out.insert(name.to_string());
        }
    }
    fn expr(e: &Expr, strings: &StringEnv, out: &mut HashSet<String>) {
        match e {
            Expr::If { then_block, elsifs, else_block, .. } => {
                block(then_block, strings, out);
                for a in elsifs {
                    block(&a.body, strings, out);
                }
                if let Some(b) = else_block {
                    block(b, strings, out);
                }
            }
            Expr::Match { arms, .. } => {
                for a in arms {
                    expr(&a.body, strings, out);
                }
            }
            Expr::Block(b) => block(b, strings, out),
            _ => {}
        }
    }
    fn block(b: &Block, strings: &StringEnv, out: &mut HashSet<String>) {
        for st in &b.stmts {
            match st {
                Stmt::Let { name, init, .. } => note(name, init, strings, out),
                Stmt::Assign { target: Expr::Ident { name, .. }, value, .. } => note(name, value, strings, out),
                Stmt::While { body, .. } => block(body, strings, out),
                Stmt::Expr(e) => expr(e, strings, out),
                _ => {}
            }
        }
        if let Some(t) = &b.tail {
            expr(t, strings, out);
        }
    }
    let mut out = HashSet::new();
    block(&f.body, strings, &mut out);
    out
}

fn callee_transfers_struct_box_runtime(func: &str) -> bool {
    matches!(func, "sal_list_push" | "sal_list_set" | "sal_dict_put")
}

fn scan_struct_box_shell(
    insts: &[IrInst],
    copies: &mut HashMap<String, u32>,
    shell: &mut HashSet<String>,
    loop_depth: u32,
) {
    for inst in insts {
        match inst {
            IrInst::While { cond_insts, body, .. } => {
                scan_struct_box_shell(cond_insts, copies, shell, loop_depth + 1);
                scan_struct_box_shell(body, copies, shell, loop_depth + 1);
            }
            IrInst::If { then_body, else_body, .. } => {
                // A copy inside a branch does not dominate the function return.
                scan_struct_box_shell(then_body, copies, shell, loop_depth + 1);
                scan_struct_box_shell(else_body, copies, shell, loop_depth + 1);
            }
            IrInst::Call { func, args, dest, .. } => {
                if func == "sal_list_get" || func == "sal_list_pop" {
                    if let Some(d) = dest {
                        copies.insert(d.clone(), loop_depth);
                    }
                } else if callee_transfers_struct_box_runtime(func) {
                    if let Some(v) = args.get(1) {
                        copies.remove(v);
                        shell.remove(v);
                    }
                } else if !func.starts_with("sal_") {
                    if let Some(v) = args.first() {
                        if copies.get(v).is_some_and(|d| *d == 0) {
                            shell.insert(v.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// After fusion, free struct-copy boxes passed into scalarizing calls (see `emit_struct_box_shell_drops`).
pub fn finish_module_box_shell_drops(m: &mut IrModule) {
    for f in &mut m.functions {
        emit_struct_box_shell_drops(&mut f.instructions);
    }
}

/// After a scalarizing user call, free the `list_get` struct box once the local is done with.
fn emit_struct_box_shell_drops(instructions: &mut Vec<IrInst>) {
    let mut copies: HashMap<String, u32> = HashMap::new();
    let mut shell: HashSet<String> = HashSet::new();
    scan_struct_box_shell(instructions, &mut copies, &mut shell, 0);
    if shell.is_empty() {
        return;
    }
    let drops: Vec<IrInst> = shell
        .into_iter()
        .map(|v| IrInst::Call {
            dest: None,
            func: "sal_struct_free_box".into(),
            args: vec![v],
        })
        .collect();
    if let Some(ret) = instructions.iter().rposition(|i| matches!(i, IrInst::Return { .. })) {
        instructions.splice(ret..ret, drops);
    } else {
        instructions.extend(drops);
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
    let alias = list_alias_locals(f, strings);
    let reads = field_reads(f, strings, param_modes);
    let effective_modes = param_modes
        .get(&f.name)
        .cloned()
        .unwrap_or_else(|| f.params.iter().map(|p| p.mode).collect());
    let own_mode: HashMap<&str, ParamMode> = f
        .params
        .iter()
        .zip(effective_modes.iter())
        .map(|(p, m)| (p.name.as_str(), *m))
        .collect();
    let mut names: Vec<String> = strings.struct_types.keys().cloned().collect();
    names.sort();
    for name in names {
        if moved.contains(&name) || alias.contains(&name) {
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
        // A struct-valued field read by value (`q = w.p`) hands those strings to
        // someone else: leave that local's nested structs alone (a leak, never a
        // double free).
        let glue = Glue {
            nested: !reads.struct_valued.contains(&name),
            containers: !reads.heap_moved.contains(&name),
        };
        let skip = reads.strings_moved.get(&name);
        free_struct_strings(
            &base,
            sdef,
            strings,
            glue,
            0,
            counter,
            instructions,
            skip,
            "",
            Some(&reads),
            &name,
        );
    }
}

/// Which fields of a struct local the glue may release.
#[derive(Clone, Copy)]
struct Glue {
    /// Descend into struct-typed fields.
    nested: bool,
    /// Release `List` / `Dict` / `Bytes` fields.
    containers: bool,
}

/// How a field of a struct is released when the struct goes out of scope.
enum FieldDrop<'a> {
    /// `sal_free`, `sal_list_free`, `sal_dict_free` or `sal_bytes_free` on the field.
    Call(&'static str),
    /// A nested struct: its own fields, recursively.
    Struct(&'a crate::ast::StructDef),
}

/// The glue of one field type. A struct owns what was moved into it, so its
/// `String`, `Bytes` and flat `List` / `Dict` fields are released with it.
/// Containers of `String` are released when every stored value was fresh (see
/// `FieldReads::container_tainted`); otherwise they are left alone.
fn field_drop<'a>(
    ty: &crate::ast::Type,
    strings: &'a StringEnv,
    glue: Glue,
    depth: u32,
    reads: Option<&FieldReads>,
    root: &str,
    path: &str,
) -> Option<FieldDrop<'a>> {
    let tainted = reads.is_some_and(|r| {
        r.container_tainted
            .get(root)
            .is_some_and(|s| s.contains(path))
    });
    let flat = |t: &crate::ast::Type| {
        matches!(t, crate::ast::Type::Named { name, args, .. }
            if args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool"))
    };
    let crate::ast::Type::Named { name, args, .. } = ty else {
        return None;
    };
    match (name.as_str(), args.as_slice()) {
        ("String", []) => Some(FieldDrop::Call("sal_free")),
        ("Bytes", []) if glue.containers => Some(FieldDrop::Call("sal_bytes_free")),
        (_, []) if strings.enums.contains_key(name) => Some(FieldDrop::Call("sal_enum_free")),
        ("List", [e]) if glue.containers && flat(e) => Some(FieldDrop::Call("sal_list_free")),
        ("List", [crate::ast::Type::Named { name, args, .. }])
            if glue.containers && args.is_empty() && name == "String" && !tainted =>
        {
            Some(FieldDrop::Call("sal_list_free"))
        }
        ("List", [crate::ast::Type::Named { name: sn, args, .. }])
            if glue.containers
                && args.is_empty()
                && !tainted
                && crate::ir::struct_desc(sn, &strings.structs, &strings.enums).is_some() =>
        {
            Some(FieldDrop::Call("sal_list_free"))
        }
        ("Dict", [k, v])
            if glue.containers
                && flat(v)
                && matches!(k, crate::ast::Type::Named { name, args, .. }
                    if args.is_empty() && matches!(name.as_str(), "Int" | "String")) =>
        {
            Some(FieldDrop::Call("sal_dict_free"))
        }
        ("Dict", [k, crate::ast::Type::Named { name: vn, args: va, .. }])
            if glue.containers
                && va.is_empty()
                && vn == "String"
                && !tainted
                && matches!(k, crate::ast::Type::Named { name, args, .. }
                    if args.is_empty() && matches!(name.as_str(), "Int" | "String")) =>
        {
            Some(FieldDrop::Call("sal_dict_free"))
        }
        (_, []) if glue.nested && depth < 8 => strings
            .structs
            .get(name)
            .filter(|d| d.type_params.is_empty() && struct_has_heap(d, strings, glue, depth + 1))
            .map(FieldDrop::Struct),
        _ => None,
    }
}

/// Frees the heap fields of the struct value `base` (of type `sdef`) and, when
/// `glue.nested`, those of the structs held in its fields, recursively. A field of a
/// transparent (single-field) struct is the value itself; otherwise it is read
/// with `Extract`.
fn free_struct_strings(
    base: &str,
    sdef: &crate::ast::StructDef,
    strings: &StringEnv,
    glue: Glue,
    depth: u32,
    counter: &mut u32,
    instructions: &mut Vec<IrInst>,
    strings_moved: Option<&HashSet<String>>,
    path_prefix: &str,
    reads: Option<&FieldReads>,
    root: &str,
) {
    let transparent = is_transparent_struct(sdef);
    for (i, field) in sdef.fields.iter().enumerate() {
        let path = if path_prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{path_prefix}.{}", field.name)
        };
        let Some(action) = field_drop(&field.ty, strings, glue, depth, reads, root, &path) else {
            continue;
        };
        if matches!(action, FieldDrop::Call("sal_free") | FieldDrop::Call("sal_enum_free"))
            && strings_moved.is_some_and(|s| s.contains(&path))
        {
            continue;
        }
        let ssa = if transparent {
            base.to_string()
        } else {
            *counter += 1;
            let d = format!("t{counter}");
            instructions.push(IrInst::Extract {
                dest: d.clone(),
                base: base.to_string(),
                struct_name: sdef.name.clone(),
                field_index: i as u32,
            });
            d
        };
        match action {
            FieldDrop::Call(func) => instructions.push(IrInst::Call {
                dest: None,
                func: func.into(),
                args: vec![ssa],
            }),
            FieldDrop::Struct(inner) => {
                free_struct_strings(
                    &ssa,
                    inner,
                    strings,
                    glue,
                    depth + 1,
                    counter,
                    instructions,
                    strings_moved,
                    &path,
                    reads,
                    root,
                )
            }
        }
    }
}

/// Does a struct hold anything its glue releases, directly or in a nested struct?
fn struct_has_heap(def: &crate::ast::StructDef, strings: &StringEnv, glue: Glue, depth: u32) -> bool {
    depth < 8
        && def
            .fields
            .iter()
            .any(|f| field_drop(&f.ty, strings, glue, depth, None, "", "").is_some())
}

/// What a function does with the fields of its struct locals, for the glue.
#[derive(Default)]
struct FieldReads {
    /// Locals read as a whole nested struct: `q = w.p`, `f(w.p)`, `return w.p`…
    /// (a field chain that ends in a struct-typed field and is not itself the
    /// base of another field access).
    struct_valued: HashSet<String>,
    /// Locals whose `List` / `Dict` / `Bytes` field is handed on: let value,
    /// assignment value, `return`, block tail, or an argument of a call that may
    /// keep it. The new owner releases it, so the local must not.
    heap_moved: HashSet<String>,
    /// Root local -> dot-paths of `String` fields handed out by value (`return n.name`).
    strings_moved: HashMap<String, HashSet<String>>,
    /// Root local -> dot-paths of `List[String]` / `Dict` of `String` tainted by a non-fresh store.
    container_tainted: HashMap<String, HashSet<String>>,
}

fn field_reads(
    f: &FnDef,
    strings: &StringEnv,
    param_modes: &HashMap<String, Vec<ParamMode>>,
) -> FieldReads {
    struct Walk<'a> {
        strings: &'a StringEnv,
        modes: &'a HashMap<String, Vec<ParamMode>>,
        out: FieldReads,
    }
    /// `a.f1.f2…`: the root local and the type of the last field.
    fn chain<'a>(e: &'a Expr, fields: &mut Vec<&'a str>) -> Option<&'a str> {
        match e {
            Expr::Field { base, field, .. } => {
                let root = chain(base, fields)?;
                fields.push(field);
                Some(root)
            }
            Expr::Ident { name, .. } => Some(name),
            _ => None,
        }
    }
    impl Walk<'_> {
        fn note(&mut self, e: &Expr, moving: bool) {
            let mut fields = Vec::new();
            let Some(root) = chain(e, &mut fields) else {
                return;
            };
            let strings = self.strings;
            let Some(mut cur) = strings.struct_types.get(root).and_then(|n| strings.structs.get(n)) else {
                return;
            };
            for (k, fname) in fields.iter().enumerate() {
                let Some(fd) = cur.fields.iter().find(|x| x.name == *fname) else {
                    return;
                };
                let last = k + 1 == fields.len();
                if let crate::ast::Type::Named { name, args, .. } = &fd.ty {
                    if last && moving {
                        if matches!(name.as_str(), "List" | "Dict" | "Bytes") {
                            self.out.heap_moved.insert(root.to_string());
                        } else if name == "String" && args.is_empty() {
                            let path = fields.join(".");
                            self.out
                                .strings_moved
                                .entry(root.to_string())
                                .or_default()
                                .insert(path);
                        } else if args.is_empty() && self.strings.enums.contains_key(name) {
                            let path = fields.join(".");
                            self.out
                                .strings_moved
                                .entry(root.to_string())
                                .or_default()
                                .insert(path);
                        }
                    }
                    if let Some(next) = strings.structs.get(name).filter(|_| args.is_empty()) {
                        if last {
                            self.out.struct_valued.insert(root.to_string());
                        }
                        cur = next;
                        continue;
                    }
                }
                return;
            }
        }
        fn ex(&mut self, e: &Expr, moving: bool) {
            match e {
                Expr::Field { base, .. } => {
                    self.note(e, moving);
                    // The root of the chain has nothing more to visit unless it is a call.
                    let mut b: &Expr = base;
                    while let Expr::Field { base, .. } = b {
                        b = base;
                    }
                    if !matches!(b, Expr::Ident { .. }) {
                        self.ex(b, false);
                    }
                }
                Expr::Call { func, args, .. } => {
                    self.ex(func, false);
                    let callee = match func.as_ref() {
                        Expr::Ident { name, .. } => Some(name.as_str()),
                        _ => None,
                    };
                    if matches!(callee, Some("list_push" | "list_set" | "dict_put"))
                        && args.len() >= 2
                    {
                        if let (Some((root, path)), Some(v)) =
                            (container_field_path(&args[0]), args.last())
                        {
                            if !crate::drops::is_fresh_value(v) {
                                self.out
                                    .container_tainted
                                    .entry(root)
                                    .or_default()
                                    .insert(path);
                            }
                        }
                    }
                    for (i, a) in args.iter().enumerate() {
                        let keeps = match callee {
                            Some(n) => match self.modes.get(n) {
                                Some(m) => m.get(i) == Some(&ParamMode::Take),
                                None => !crate::drops::borrows_args(n),
                            },
                            None => true,
                        };
                        self.ex(a, keeps);
                    }
                }
                Expr::Binary { left, right, .. } => {
                    self.ex(left, false);
                    self.ex(right, false);
                }
                Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } => {
                    self.ex(expr, moving)
                }
                Expr::Block(b) | Expr::On { body: b, .. } => self.blk(b),
                Expr::If { cond, then_block, elsifs, else_block, .. } => {
                    self.ex(cond, false);
                    self.blk(then_block);
                    for a in elsifs {
                        self.ex(&a.cond, false);
                        self.blk(&a.body);
                    }
                    if let Some(b) = else_block {
                        self.blk(b);
                    }
                }
                Expr::Match { scrutinee, arms, .. } => {
                    self.ex(scrutinee, false);
                    for a in arms {
                        self.ex(&a.body, true);
                    }
                }
                Expr::Lambda { body, .. } => self.ex(body, true),
                Expr::TensorLit { rows, .. } => {
                    for r in rows {
                        for c in r {
                            self.ex(c, false);
                        }
                    }
                }
                _ => {}
            }
        }
        fn blk(&mut self, b: &Block) {
            for st in &b.stmts {
                match st {
                    Stmt::Let { init, .. } => self.ex(init, true),
                    Stmt::Expr(e) => self.ex(e, false),
                    Stmt::Assign { target, value, .. } => {
                        self.ex(target, false);
                        self.ex(value, true);
                    }
                    Stmt::Return { value, .. } => {
                        if let Some(v) = value {
                            self.ex(v, true);
                        }
                    }
                    Stmt::While { cond, body, .. } => {
                        self.ex(cond, false);
                        self.blk(body);
                    }
                    Stmt::Break { .. } => {}
                }
            }
            if let Some(t) = &b.tail {
                self.ex(t, true);
            }
        }
    }
    let mut w = Walk { strings, modes: param_modes, out: FieldReads::default() };
    w.blk(&f.body);
    w.out
}

fn field_root_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident { name, .. } => Some(name.clone()),
        Expr::Field { base, .. } => field_root_name(base),
        _ => None,
    }
}

fn field_dot_path(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident { .. } => Some(String::new()),
        Expr::Field { base, field, .. } => {
            let mut p = field_dot_path(base)?;
            if !p.is_empty() {
                p.push('.');
            }
            p.push_str(field);
            Some(p)
        }
        _ => None,
    }
}

fn container_field_path(e: &Expr) -> Option<(String, String)> {
    Some((field_root_name(e)?, field_dot_path(e)?))
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
        Stmt::Break { .. } => {}
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
                } else {
                    // A builtin (`list_push`, `dict_put`, ...) or a struct/variant constructor
                    // keeps what it is given.
                    for a in args {
                        if let Expr::Ident { name: arg, .. } = a {
                            moved.insert(arg.clone());
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
        Stmt::Break { .. } => {}
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
    dim_params: &mut Vec<String>,
    fn_names: &HashMap<String, ()>,
    host_literals: &mut Vec<IrHostLiteral>,
    strings: &mut StringEnv,
) -> bool {
    // `f = x => body` / `let f = x => body`: calls of `f` inline the body.
    match st {
        Stmt::Let { name, init, .. }
        | Stmt::Assign { target: Expr::Ident { name, .. }, value: init, .. } => {
            if let Expr::Lambda { params, body, .. } = init {
                if let Some(p) = params.first() {
                    strings.lambdas.insert(name.clone(), (p.clone(), (**body).clone()));
                }
            }
        }
        _ => {}
    }
    match st {
        Stmt::Break { .. } => return false,
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
                host_literals,
                strings,
            );
            register_let_tensor(name, ty.as_ref(), init, &v, strings, tensors);
            env.insert(name.clone(), v);
            if let Some(Type::Tensor { dims, .. }) = ty.as_ref() {
                for (i, d) in dims.iter().enumerate() {
                    if matches!(d, Dim::Dynamic) {
                        let dname = format!("{name}_d{i}");
                        if !dim_params.contains(&dname) {
                            dim_params.push(dname.clone());
                        }
                        if !env.contains_key(&dname) {
                            let n = tensors
                                .get(name)
                                .and_then(|t| t.concrete.get(i).copied().flatten())
                                .or_else(|| {
                                    let (rows, cols) = tensor_lit_shape(init)?;
                                    if dims.len() >= 2 {
                                        let r_i = dims.len() - 2;
                                        let c_i = dims.len() - 1;
                                        match i {
                                            x if x == r_i => Some(rows),
                                            x if x == c_i => Some(cols),
                                            _ => None,
                                        }
                                    } else {
                                        None
                                    }
                                });
                            let dssa = if let Some(n) = n {
                                push_const_dim(n, instructions, counter)
                            } else {
                                *counter += 1;
                                let dssa = format!("t{counter}");
                                instructions.push(IrInst::ConstInt {
                                    dest: dssa.clone(),
                                    value: 0,
                                });
                                dssa
                            };
                            env.insert(dname, dssa);
                        }
                    }
                }
            }
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
                                    host_literals,
                                    strings,
                                );
                                env.insert(name.clone(), v);
                                return false;
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
                                    host_literals,
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
                                return false;
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
                    host_literals,
                    strings,
                );
                strings.accumulate = None;
                register_let_tensor(name, None, value, &v, strings, tensors);
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
                    host_literals,
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
                host_literals,
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
                    host_literals,
                    strings,
                );
                instructions.push(IrInst::Return { value: val });
            } else {
                *counter += 1;
                let z = format!("t{counter}");
                instructions.push(IrInst::ConstInt {
                    dest: z.clone(),
                    value: 0,
                });
                instructions.push(IrInst::Return { value: z });
            }
            return true;
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
                host_literals,
                strings,
            );
            let mut body_insts = Vec::new();
            let mut body_env = loop_env;
            let mut body_diverged = false;
            for st in &body.stmts {
                if lower_stmt(
                    st,
                    &mut body_insts,
                    &mut body_env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    host_literals,
                    strings,
                ) {
                    body_diverged = true;
                    break;
                }
            }
            if !body_diverged {
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
                        host_literals,
                        strings,
                    );
                }
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
    false
}

fn call_fn_tensor_ret<'a>(
    init: &Expr,
    strings: &'a StringEnv,
) -> Option<&'a Type> {
    let Expr::Call { func, .. } = init else {
        return None;
    };
    let Expr::Ident { name, .. } = func.as_ref() else {
        return None;
    };
    strings.fn_tensor_rets.get(name)
}

fn tensor_info_from_type(ty: &Type) -> Option<TensorInfo> {
    let Type::Tensor {
        elem, dims, place, ..
    } = ty
    else {
        return None;
    };
    let concrete = dims
        .iter()
        .map(|d| match d {
            Dim::Static(n) => Some(*n),
            Dim::Dynamic => None,
        })
        .collect();
    Some(TensorInfo {
        elem: *elem,
        dims: dims.clone(),
        place: place.clone(),
        concrete,
    })
}

fn register_ssa_tensor(ty: &Type, ir_ssa: &str, tensors: &mut HashMap<String, TensorInfo>) {
    if let Some(info) = tensor_info_from_type(ty) {
        tensors.insert(ir_ssa.to_string(), info);
    }
}

fn register_let_tensor(
    name: &str,
    ty: Option<&Type>,
    init: &Expr,
    ir_name: &str,
    strings: &StringEnv,
    tensors: &mut HashMap<String, TensorInfo>,
) {
    let lit = tensor_lit_shape(init);
    let load_ty = load_type_arg(init);
    let call_ret = call_fn_tensor_ret(init, strings);
    let annotated = ty.or(load_ty.as_ref()).or(call_ret);
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
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => expr_as_f32(expr).map(|v| -v),
        _ => None,
    }
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

fn maybe_box_for_any(
    e: &Expr,
    payload: String,
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
    strings: &StringEnv,
) -> String {
    let Some((tname, protocol)) = strings.any_coercion(e.span()) else {
        return payload;
    };
    let vtable = crate::protocols::vtable_global(&tname, &protocol);
    let drop_slot = strings
        .protocols
        .get(&protocol)
        .map(|p| crate::protocols::drop_slot(p) as u32)
        .unwrap_or(0);
    *counter += 1;
    let dest = format!("t{counter}");
    instructions.push(IrInst::AnyBox {
        dest: dest.clone(),
        vtable,
        payload,
        drop_slot,
    });
    dest
}

fn lower_expr_simple(
    e: &Expr,
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    tensors: &mut HashMap<String, TensorInfo>,
    fn_names: &HashMap<String, ()>,
    host_literals: &mut Vec<IrHostLiteral>,
    strings: &mut StringEnv,
) -> String {
    let mut empty_dims = Vec::new();
    lower_expr(
        e,
        instructions,
        env,
        counter,
        &mut Vec::new(),
        tensors,
        &mut empty_dims,
        fn_names,
        host_literals,
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

/// Element descriptor of `list_new[T]()` (read by `sal_list_new_desc`): `i` a Copy
/// scalar, `s` a String, `E` an enum, `S(d..)` a flat struct whose field k is described by d_k.
/// Any other element type is a Copy scalar here, as before the descriptors.
pub(crate) fn list_desc(
    type_args: &[crate::ast::TypeArg],
    structs: &HashMap<String, crate::ast::StructDef>,
    enums: &HashMap<String, crate::ast::EnumDef>,
    owned: bool,
) -> String {
    let base = match type_args.first() {
        Some(crate::ast::TypeArg::Type(t)) => type_desc(t, structs, enums, owned),
        _ => "i".into(),
    };
    // `+` marks an owned list: its `list_free` releases the elements (see sal_list_new_desc).
    if owned {
        format!("+{base}")
    } else {
        base
    }
}

/// `owned` is set only for the lists the drops pass releases (`list_new_owned`): an enum
/// element is `E` there and `i` (borrowed) everywhere else.
/// Descriptor of a struct value (`S(d..)`, one slot per field): a non-generic struct
/// whose fields are all describable, `None` otherwise (a List, Dict, Bytes, generic…).
/// A nested struct is boxed in its parent's slot, an enum field is an enum box.
pub(crate) fn struct_desc(
    name: &str,
    structs: &HashMap<String, crate::ast::StructDef>,
    enums: &HashMap<String, crate::ast::EnumDef>,
) -> Option<String> {
    struct_desc_at(name, structs, enums, 0)
}

fn struct_desc_at(
    name: &str,
    structs: &HashMap<String, crate::ast::StructDef>,
    enums: &HashMap<String, crate::ast::EnumDef>,
    depth: u32,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let sd = structs.get(name)?;
    // One-field structs are transparent (the value is the field), so they have no box.
    if !sd.type_params.is_empty() || sd.fields.len() < 2 {
        return None;
    }
    let mut out = String::from("S(");
    for f in &sd.fields {
        out.push_str(&field_desc(&f.ty, structs, enums, depth + 1)?);
    }
    out.push(')');
    Some(out)
}

fn field_desc(
    t: &crate::ast::Type,
    structs: &HashMap<String, crate::ast::StructDef>,
    enums: &HashMap<String, crate::ast::EnumDef>,
    depth: u32,
) -> Option<String> {
    let crate::ast::Type::Named { name, args, .. } = t else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    Some(match name.as_str() {
        "Int" | "Float" | "Bool" => "i".into(),
        "String" => "s".into(),
        _ if enums.get(name).is_some_and(|e| e.type_params.is_empty()) => "E".into(),
        _ => return struct_desc_at(name, structs, enums, depth),
    })
}

/// Element descriptor of `T` in `list_new[T]()`. A struct is always described, so that
/// `list_get` hands out a copy the caller may release (the glue of a struct local frees
/// its fields). An enum is described only when `owned` (`list_new_owned`, set by the drops
/// pass for the lists it releases): otherwise its boxes are borrowed and `list_get` aliases.
fn type_desc(
    t: &crate::ast::Type,
    structs: &HashMap<String, crate::ast::StructDef>,
    enums: &HashMap<String, crate::ast::EnumDef>,
    owned: bool,
) -> String {
    if let crate::ast::Type::Named { name, args, .. } = t {
        if args.is_empty() {
            if name == "String" {
                return "s".into();
            }
            if owned && enums.get(name).is_some_and(|e| e.type_params.is_empty()) {
                return "E".into();
            }
            if let Some(d) = struct_desc(name, structs, enums) {
                return d;
            }
        }
    }
    "i".into()
}

fn struct_val_kind(
    name: &str,
    structs: &HashMap<String, crate::ast::StructDef>,
    _enums: &HashMap<String, crate::ast::EnumDef>,
) -> Option<i64> {
    let sd = structs.get(name)?;
    if !sd.type_params.is_empty() || sd.fields.len() < 2 {
        return None;
    }
    let mut kind = 4i64 | ((sd.fields.len() as i64) << 4);
    for (i, f) in sd.fields.iter().enumerate() {
        if matches!(&f.ty, crate::ast::Type::Named { name, args, .. } if args.is_empty() && name == "String") {
            kind |= 1 << (12 + i);
        }
    }
    Some(kind)
}

fn dict_val_kind(
    ty: &crate::ast::Type,
    structs: &HashMap<String, crate::ast::StructDef>,
    enums: &HashMap<String, crate::ast::EnumDef>,
) -> i64 {
    let crate::ast::Type::Named { name, args, .. } = ty else {
        return 0;
    };
    if !args.is_empty() {
        return 0;
    }
    match name.as_str() {
        "String" => 3,
        n if enums.get(n).is_some_and(|e| e.type_params.is_empty()) => 5,
        n => struct_val_kind(n, structs, enums).unwrap_or(0),
    }
}

/// Runtime kinds of `dict_new[K, V]()`: the key and the value.
fn dict_kinds(
    type_args: &[crate::ast::TypeArg],
    structs: &HashMap<String, crate::ast::StructDef>,
    enums: &HashMap<String, crate::ast::EnumDef>,
) -> Vec<i64> {
    let mut out = vec![0i64, 0];
    for (i, a) in type_args
        .iter()
        .filter_map(|a| match a {
            crate::ast::TypeArg::Type(t) => Some(t),
            crate::ast::TypeArg::Dim(_) => None,
        })
        .take(2)
        .enumerate()
    {
        out[i] = if i == 0 {
            scalar_kind(a)
        } else {
            dict_val_kind(a, structs, enums)
        };
    }
    out
}

fn matmul_runtime_name(tensors: &HashMap<String, TensorInfo>, lhs: &str) -> String {
    match tensors.get(lhs).map(|t| t.elem) {
        Some(TensorElem::F16) => "sal_matmul_f16".into(),
        Some(TensorElem::BF16) => "sal_matmul_bf16".into(),
        _ => "sal_matmul_f32".into(),
    }
}

fn runtime_call_name(fname: &str, fn_names: &HashMap<String, ()>) -> String {
    // List ops always hit the runtime; prelude bodies are signatures only.
    if matches!(
        fname,
        "list_new" | "list_push" | "list_len" | "list_get" | "list_set" | "list_pop" | "dict_new"
            | "dict_put" | "dict_get" | "dict_has" | "dict_len" | "dict_remove" | "dict_keys" | "dict_values" | "list_free" | "dict_free" | "enum_free" | "struct_free" | "struct_free_box" | "tensor_free" | "any_free"
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
            | "qmatmul"
            | "attention"
            | "softmax"
            | "relu"
            | "silu"
            | "rms_norm"
            | "rope"
            | "map"
            | "reduce"
            | "tensor_append"
            | "argmax"
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
            | "str_index_of"
            | "str_index_from"
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
            | "f64_bits_to_str"
            | "now_ns"
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
            | "list_free"
            | "dict_free"
            | "enum_free"
            | "any_free"
            | "struct_free"
            | "struct_free_box"
            | "enum_owns"
            | "enum_box_id"
            | "expr_addr"
            | "bytes_free"
            | "bytes_new"
            | "bytes_len"
            | "bytes_get_u8"
            | "bytes_get_u16"
            | "bytes_get_u32"
            | "bytes_get_i64"
            | "bytes_set_u8"
            | "bytes_set_u16"
            | "bytes_set_u32"
            | "bytes_set_i64"
            | "bytes_fill"
            | "bytes_fill_u32"
            | "bytes_copy"
            | "str_bytes_copy"
            | "bytes_to_str"
            | "bytes_view"
            | "bytes_view_mem"
            | "bytes_ptr"
            | "syscall"
            | "list_new"
            | "list_new_owned"
            | "list_push"
            | "list_len"
            | "list_box_id"
            | "list_get"
            | "dict_new"
            | "dict_put"
            | "dict_get"
            | "dict_has"
            | "dict_len"
            | "dict_remove"
            | "dict_keys"
            | "dict_values"
            | "list_set"
            | "list_pop"
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
    dim_params: &mut Vec<String>,
    fn_names: &HashMap<String, ()>,
    host_literals: &mut Vec<IrHostLiteral>,
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
        Expr::TensorLit { .. } => {
            *counter += 1;
            let dest = format!("t{counter}");
            if let Some((rows, cols, data)) = tensor_lit_data(e) {
                host_literals.push(IrHostLiteral {
                    ir_name: dest.clone(),
                    rows,
                    cols,
                    data,
                });
                tensors.insert(
                    dest.clone(),
                    TensorInfo {
                        elem: TensorElem::F32,
                        dims: vec![Dim::Static(rows), Dim::Static(cols)],
                        place: Place::Cpu,
                        concrete: vec![Some(rows), Some(cols)],
                    },
                );
            }
            dest
        }
        Expr::Ident { name, .. } => {
            if let Some(v) = env.get(name) {
                return v.clone();
            }
            if let Some((edef, vdef)) = find_variant_in_enums(name, &strings.enums) {
                if vdef.fields.is_empty() {
                    *counter += 1;
                    let dest = format!("t{counter}");
                    let vidx = enum_variant_index(edef, &vdef.name).unwrap_or(0) as u32;
                    instructions.push(IrInst::EnumMake {
                        dest: dest.clone(),
                        enum_name: edef.name.clone(),
                        variant_index: vidx,
                        payload: Vec::new(),
                        owned: 0,
                    });
                    return dest;
                }
            }
            name.clone()
        }
        // `a && b` is `if a then b else false`; `a || b` is `if a then true else b`. The
        // right operand runs only on the branch that needs it, so the short circuit is the
        // same control flow the `if` already lowers.
        Expr::Binary {
            op: op @ (BinOp::And | BinOp::Or),
            left,
            right,
            span,
        } => {
            let lit = |value: bool| Expr::Bool { value, span: *span };
            let tail = |e: Expr| Block {
                stmts: Vec::new(),
                tail: Some(Box::new(e)),
                span: *span,
            };
            // The result is a truth value: an `Int` right operand becomes `right != 0`
            // (a `Bool` is already 0 or 1, so the comparison leaves it unchanged).
            let truth = Expr::Binary {
                op: BinOp::Ne,
                left: right.clone(),
                right: Box::new(Expr::Int { value: 0, span: *span }),
                span: *span,
            };
            let (then_e, else_e) = if *op == BinOp::And {
                (truth, lit(false))
            } else {
                (lit(true), truth)
            };
            let short = Expr::If {
                cond: left.clone(),
                then_block: tail(then_e),
                elsifs: Vec::new(),
                else_block: Some(tail(else_e)),
                span: *span,
            };
            lower_expr(
                &short,
                instructions,
                env,
                counter,
                regions,
                tensors,
                dim_params,
                fn_names,
                host_literals,
                strings,
            )
        }
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
            let l = lower_expr_simple(left, instructions, env, counter, tensors, fn_names, host_literals, strings);
            strings.accumulate = None;
            let r = lower_expr_simple(right, instructions, env, counter, tensors, fn_names, host_literals, strings);
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
                    args: vec![l.clone(), r.clone()],
                });
                if add_right_is_fresh_temp(right, strings) {
                    instructions.push(IrInst::Call {
                        dest: None,
                        func: "sal_free".into(),
                        args: vec![r],
                    });
                }
                // `str_concat` copies both operands into a new buffer; `str_append` keeps
                // the left one as the result.
                if func == "sal_str_concat" && add_left_is_fresh_call(left) {
                    instructions.push(IrInst::Call {
                        dest: None,
                        func: "sal_free".into(),
                        args: vec![l],
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
                BinOp::Mod => "mod",
                BinOp::Band => "band",
                BinOp::Bxor => "bxor",
                BinOp::Bor => "bor",
                BinOp::Shl => "shl",
                BinOp::Shr => "shr",
                BinOp::Eq => "eq",
                BinOp::Ne => "ne",
                BinOp::Lt => "lt",
                BinOp::Le => "le",
                BinOp::Gt => "gt",
                BinOp::Ge => "ge",
                BinOp::And | BinOp::Or => unreachable!("`&&` and `||` are lowered as `if`"),
            };
            let is_float = strings.expr_is_float(left) || strings.expr_is_float(right);
            let op_s = if is_float { format!("f{op_s}") } else { op_s.to_string() };
            instructions.push(IrInst::Binary {
                dest: dest.clone(),
                op: op_s,
                left: l,
                right: r,
            });
            dest
        }
        Expr::Unary { op, expr, .. } => {
            let inner = lower_expr_simple(expr, instructions, env, counter, tensors, fn_names, host_literals, strings);
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
            let op_s = if matches!(op, UnOp::Neg) && strings.expr_is_float(expr) {
                "fneg"
            } else {
                op_s
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
            span,
            ..
        } => {
            if let Some((_protocol, _method, slot)) = strings.protocol_call(*span) {
                if let Expr::Field { base, .. } = func.as_ref() {
                    *counter += 1;
                    let dest = format!("t{counter}");
                    let receiver = lower_expr_simple(
                        base,
                        instructions,
                        env,
                        counter,
                        tensors,
                        fn_names,
                        host_literals,
                        strings,
                    );
                    let mut arg_names = Vec::new();
                    for a in args {
                        let mut n = lower_expr_simple(
                            a,
                            instructions,
                            env,
                            counter,
                            tensors,
                            fn_names,
                            host_literals,
                            strings,
                        );
                        n = maybe_box_for_any(a, n, instructions, counter, strings);
                        arg_names.push(n);
                    }
                    instructions.push(IrInst::AnyCall {
                        dest: Some(dest.clone()),
                        receiver,
                        slot,
                        args: arg_names,
                    });
                    return dest;
                }
            }
            let fname = match func.as_ref() {
                Expr::Ident { name, .. } => name.clone(),
                _ => "unknown".into(),
            };
            let mut arg_names = Vec::new();
            for a in args {
                let mut n = lower_expr_simple(
                    a,
                    instructions,
                    env,
                    counter,
                    tensors,
                    fn_names,
                    host_literals,
                    strings,
                );
                n = maybe_box_for_any(a, n, instructions, counter, strings);
                arg_names.push(n);
            }
            if let Some((param, lbody)) = strings.lambdas.get(&fname).cloned() {
                if let Some(a) = arg_names.first() {
                    let mut lenv = env.clone();
                    lenv.insert(param, a.clone());
                    return lower_expr_simple(
                        &lbody,
                        instructions,
                        &mut lenv,
                        counter,
                        tensors,
                        fn_names,
                        host_literals,
                        strings,
                    );
                }
            }
            if fname == "map" && arg_names.len() == 2 {
                // Elementwise lambda: bind the parameter to the tensor and lower the
                // body with the tensor operators.
                if let Expr::Lambda { params, body, .. } = &args[1] {
                    if let (Some(p), true) = (
                        params.first(),
                        tensor_info_for_ssa(&arg_names[0], env, tensors).is_some(),
                    ) {
                        let mut lenv = env.clone();
                        lenv.insert(p.clone(), arg_names[0].clone());
                        return lower_expr_simple(
                            body,
                            instructions,
                            &mut lenv,
                            counter,
                            tensors,
                            fn_names,
                            host_literals,
                            strings,
                        );
                    }
                }
            }
            if fname == "matmul" && arg_names.len() >= 2 {
                let lhs = arg_names[0].clone();
                let rhs = arg_names[1].clone();
                let (m, k, n) = emit_matmul_dim_args(
                    &lhs,
                    &rhs,
                    env,
                    tensors,
                    dim_params,
                    instructions,
                    counter,
                );
                *counter += 1;
                let dest = format!("t{counter}");
                let mm_func = matmul_runtime_name(tensors, &lhs);
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: mm_func,
                    args: vec![lhs.clone(), rhs.clone(), m, k, n],
                });
                if let (Some(li), Some(ri)) = (
                    tensor_info_for_ssa(&lhs, env, tensors),
                    tensor_info_for_ssa(&rhs, env, tensors),
                ) {
                    if let Some(out) = matmul_out_info(&li, &ri) {
                        tensors.insert(dest.clone(), out);
                    }
                }
                return dest;
            }
            if fname == "qmatmul" && arg_names.len() == 4 {
                let a = arg_names[0].clone();
                let w = arg_names[1].clone();
                let scale = arg_names[2].clone();
                let group = arg_names[3].clone();
                let (m, k, n) = emit_matmul_dim_args(
                    &a,
                    &w,
                    env,
                    tensors,
                    dim_params,
                    instructions,
                    counter,
                );
                *counter += 1;
                let dest = format!("t{counter}");
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: "sal_qmatmul_f32".into(),
                    args: vec![a, w, scale, group, m, k, n],
                });
                return dest;
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
            if fname == "list_new" || fname == "list_new_owned" {
                // The element descriptor travels as a string constant (see `list_desc`).
                *counter += 1;
                let desc = format!("t{counter}");
                instructions.push(IrInst::ConstString {
                    dest: desc.clone(),
                    value: list_desc(type_args, &strings.structs, &strings.enums, fname == "list_new_owned"),
                });
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: "sal_list_new_desc".into(),
                    args: vec![desc],
                });
                return dest;
            }
            if fname == "dict_new" {
                let kinds = dict_kinds(type_args, &strings.structs, &strings.enums);
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
                instructions.push(IrInst::EnumMake {
                    dest: dest.clone(),
                    enum_name: edef.name.clone(),
                    variant_index: vidx,
                    payload: arg_names.clone(),
                    owned: enum_owned_mask(args, &strings.enums),
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
            if fname == "reshape" && arg_names.len() >= 2 {
                let src = arg_names[0].clone();
                if let Some(mut info) = tensor_info_for_ssa(&src, env, tensors) {
                    let mut new_dims = Vec::new();
                    let mut concrete = Vec::new();
                    for a in &args[1..] {
                        if let Expr::Int { value: v, .. } = a {
                            new_dims.push(Dim::Static(*v as u64));
                            concrete.push(Some(*v as u64));
                        }
                    }
                    if !new_dims.is_empty() {
                        info.dims = new_dims;
                        info.concrete = concrete;
                    }
                    tensors.insert(dest.clone(), info);
                }
                instructions.push(IrInst::TensorRebind {
                    dest: dest.clone(),
                    src: src.clone(),
                });
                return dest;
            }
            if fname == "transpose" && !arg_names.is_empty() {
                let src = arg_names[0].clone();
                if let Some(mut info) = tensor_info_for_ssa(&src, env, tensors) {
                    let n = info.dims.len();
                    // A static rank-2 F32 tensor is really transposed (new buffer).
                    if let (2, Some(Dim::Static(r)), Some(Dim::Static(c))) =
                        (n, info.dims.first().cloned(), info.dims.get(1).cloned())
                    {
                        if info.elem == TensorElem::F32 {
                            let mut dim_ssa = |v: u64, instructions: &mut Vec<IrInst>| {
                                *counter += 1;
                                let d = format!("t{counter}");
                                instructions.push(IrInst::ConstInt { dest: d.clone(), value: v as i64 });
                                d
                            };
                            let rows = dim_ssa(r, instructions);
                            let cols = dim_ssa(c, instructions);
                            info.dims.swap(0, 1);
                            tensors.insert(dest.clone(), info);
                            instructions.push(IrInst::Call {
                                dest: Some(dest.clone()),
                                func: "sal_transpose_f32".into(),
                                args: vec![src, rows, cols],
                            });
                            return dest;
                        }
                    }
                    if n >= 2 {
                        info.dims.swap(n - 1, n - 2);
                    }
                    tensors.insert(dest.clone(), info);
                }
                instructions.push(IrInst::TensorRebind {
                    dest: dest.clone(),
                    src: src.clone(),
                });
                return dest;
            }
            if fname == "softmax" && arg_names.len() == 2 {
                let input = arg_names[0].clone();
                if let Some(info) = tensors.get(&input).cloned() {
                    tensors.insert(dest.clone(), info);
                }
                if let Some(axis) = args.get(1).and_then(|e| match e {
                    Expr::Int { value: v, .. } => Some(*v as i64),
                    _ => None,
                }) {
                    instructions.push(IrInst::Call {
                        dest: Some(dest.clone()),
                        func: "sal_softmax_axis_f32".into(),
                        args: vec![input, axis.to_string()],
                    });
                    return dest;
                }
            }
            if fname == "attention" && arg_names.len() == 4 {
                let q = arg_names[0].clone();
                let k = arg_names[1].clone();
                let v = arg_names[2].clone();
                let causal = arg_names[3].clone();
                let q_info = tensor_info_for_ssa(&q, env, tensors);
                let k_info = tensor_info_for_ssa(&k, env, tensors);
                let (seq, heads, head_dim) = attention_shape_q(q_info.as_ref(), &q);
                let kv_heads = attention_shape_kv(k_info.as_ref(), &k);
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: "sal_attention_f32".into(),
                    args: vec![
                        q,
                        k,
                        v,
                        seq,
                        heads,
                        kv_heads,
                        head_dim,
                        causal,
                    ],
                });
                if let Some(info) = tensor_info_for_ssa(&arg_names[0], env, tensors) {
                    tensors.insert(dest.clone(), info);
                }
                return dest;
            }
            if fname == "tensor_append" && arg_names.len() == 2 {
                let cache = arg_names[0].clone();
                let row = arg_names[1].clone();
                let row_elems_total = tensors
                    .get(&row)
                    .and_then(|r| dims_elems(r).ok())
                    .unwrap_or(1);
                let rows_add = tensors
                    .get(&row)
                    .and_then(|r| match r.dims.first() {
                        Some(Dim::Static(n)) => Some(*n),
                        _ => Some(1),
                    })
                    .unwrap_or(1);
                let row_stride = if rows_add > 0 {
                    row_elems_total / rows_add
                } else {
                    row_elems_total
                };
                let dim_key = format!("{cache}_d0");
                let seq_ssa = if let Some(s) = env.get(&dim_key) {
                    s.clone()
                } else {
                    let s = push_const_dim(0, instructions, counter);
                    env.insert(dim_key.clone(), s.clone());
                    s
                };
                if rows_add > 1 {
                    instructions.push(IrInst::Call {
                        dest: None,
                        func: "sal_tensor_write_rows_f32".into(),
                        args: vec![
                            cache.clone(),
                            seq_ssa.clone(),
                            row,
                            rows_add.to_string(),
                            row_stride.to_string(),
                        ],
                    });
                } else {
                    instructions.push(IrInst::Call {
                        dest: None,
                        func: "sal_tensor_write_row_f32".into(),
                        args: vec![
                            cache.clone(),
                            seq_ssa.clone(),
                            row,
                            row_stride.to_string(),
                        ],
                    });
                }
                *counter += 1;
                let new_seq = format!("t{counter}");
                let rows_add_ssa = push_const_dim(rows_add, instructions, counter);
                instructions.push(IrInst::Binary {
                    dest: new_seq.clone(),
                    op: "add".into(),
                    left: seq_ssa.clone(),
                    right: rows_add_ssa,
                });
                env.insert(format!("{dest}_d0"), new_seq.clone());
                if let Some(c) = tensors.get(&cache).cloned() {
                    tensors.insert(dest.clone(), c);
                }
                env.insert(format!("{cache}_d0"), new_seq);
                env.insert(dest.clone(), cache.clone());
                return dest;
            }
            if fname == "rope" && arg_names.len() == 2 {
                let input = arg_names[0].clone();
                let pos = arg_names[1].clone();
                let info = tensor_info_for_ssa(&input, env, tensors);
                let (seq, dim) = rope_seq_dim_ssa(
                    info.as_ref(),
                    &input,
                    dim_params,
                    instructions,
                    counter,
                );
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: "sal_rope".into(),
                    args: vec![input.clone(), seq, dim, pos],
                });
                if let Some(i) = info {
                    tensors.insert(dest.clone(), i);
                }
                return dest;
            }
            if fname == "silu" && !arg_names.is_empty() {
                let input = arg_names[0].clone();
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: "sal_silu".into(),
                    args: vec![input.clone()],
                });
                if let Some(i) = tensor_info_for_ssa(&input, env, tensors) {
                    tensors.insert(dest.clone(), i);
                }
                return dest;
            }
            let func_name = runtime_call_name(&fname, fn_names);
            instructions.push(IrInst::Call {
                dest: Some(dest.clone()),
                func: func_name,
                args: arg_names,
            });
            if fn_names.contains_key(&fname) {
                if let Some(ret_ty) = strings.fn_tensor_rets.get(&fname) {
                    register_ssa_tensor(ret_ty, &dest, tensors);
                }
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
                        host_literals,
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
                        host_literals,
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

            if let Some(FusedOp::Matmul { lhs, rhs, .. }) = ops
                .iter()
                .find(|o| matches!(o, FusedOp::Matmul { .. }))
            {
                let (m, k, n) = emit_matmul_dim_args(
                    lhs,
                    rhs,
                    env,
                    tensors,
                    dim_params,
                    instructions,
                    counter,
                );
                *counter += 1;
                let dest = format!("t{counter}");
                let mm_func = matmul_runtime_name(tensors, &lhs);
                instructions.push(IrInst::Call {
                    dest: Some(dest.clone()),
                    func: mm_func,
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
                    host_literals,
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
                    host_literals,
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
            let src = lower_expr_simple(expr, instructions, env, counter, tensors, fn_names, host_literals, strings);
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
            let c = lower_expr_simple(cond, instructions, env, counter, tensors, fn_names, host_literals, strings);
            let mut then_body = Vec::new();
            let mut else_body = Vec::new();
            let mut then_env = env.clone();
            let mut else_env = env.clone();
            let mut then_strings = strings.clone();
            let mut else_strings = strings.clone();
            let mut then_diverged = false;
            for st in &then_block.stmts {
                if lower_stmt(
                    st,
                    &mut then_body,
                    &mut then_env,
                    counter,
                    &mut Vec::new(),
                    tensors,
                    dim_params,
                    fn_names,
                    host_literals,
                    &mut then_strings,
                ) {
                    then_diverged = true;
                    break;
                }
            }
            let then_val = if then_diverged {
                *counter += 1;
                format!("t{counter}")
            } else if let Some(t) = &then_block.tail {
                lower_expr(
                    t,
                    &mut then_body,
                    &mut then_env,
                    counter,
                    &mut Vec::new(),
                    tensors,
                    dim_params,
                    fn_names,
                    host_literals,
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
                let mut else_diverged = false;
                for st in &else_block.stmts {
                    if lower_stmt(
                        st,
                        &mut else_body,
                        &mut else_env,
                        counter,
                        &mut Vec::new(),
                        tensors,
                        dim_params,
                        fn_names,
                        host_literals,
                        &mut else_strings,
                    ) {
                        else_diverged = true;
                        break;
                    }
                }
                if else_diverged {
                    *counter += 1;
                    format!("t{counter}")
                } else if let Some(t) = &else_block.tail {
                    lower_expr(
                        t,
                        &mut else_body,
                        &mut else_env,
                        counter,
                        &mut Vec::new(),
                        tensors,
                        dim_params,
                        fn_names,
                        host_literals,
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
        Expr::Try { expr: inner, .. } => {
            let v = lower_expr_simple(inner, instructions, env, counter, tensors, fn_names, host_literals, strings);
            // Failing variant (`Err` / `None`) of the operand's enum.
            let fail_tag = match strings.expr_type(inner) {
                Some(crate::ast::Type::Named { name, .. }) => strings.enums.get(name).and_then(|ed| {
                    ed.variants
                        .iter()
                        .find(|x| x.name == "Err" || x.name == "None")
                        .map(|x| enum_variant_tag(ed, x))
                }),
                _ => None,
            };
            let Some(tag) = fail_tag else { return v };
            *counter += 1;
            let low = format!("t{counter}");
            instructions.push(IrInst::EnumTag { dest: low.clone(), value: v.clone() });
            *counter += 1;
            let tagc = format!("t{counter}");
            instructions.push(IrInst::ConstInt { dest: tagc.clone(), value: tag });
            *counter += 1;
            let cond = format!("t{counter}");
            instructions.push(IrInst::Binary { dest: cond.clone(), op: "eq".into(), left: low, right: tagc });
            *counter += 1;
            let join = format!("t{counter}");
            instructions.push(IrInst::If {
                cond,
                then_body: vec![IrInst::Return { value: v.clone() }],
                else_body: Vec::new(),
                then_val: v.clone(),
                else_val: v.clone(),
                dest: join,
                carried: Vec::new(),
            });
            *counter += 1;
            let payload = format!("t{counter}");
            instructions.push(IrInst::EnumField { dest: payload.clone(), value: v, index: 0 });
            payload
        }
        Expr::Field { base, field, .. } => {
            let base_ssa = lower_expr_simple(
                base,
                instructions,
                env,
                counter,
                tensors,
                fn_names,
                host_literals,
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
                host_literals,
                strings,
            );
            let scrut_ty = strings.expr_type(scrutinee).cloned();
            lower_match_chain(
                scrut,
                scrut_ty,
                arms,
                instructions,
                env,
                counter,
                regions,
                tensors,
                dim_params,
                fn_names,
                host_literals,
                strings,
            )
        }
        Expr::Block(b) => {
            let mut diverged = false;
            for st in &b.stmts {
                if lower_stmt(
                    st,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    host_literals,
                    strings,
                ) {
                    diverged = true;
                    break;
                }
            }
            if diverged {
                *counter += 1;
                format!("t{counter}")
            } else if let Some(t) = &b.tail {
                lower_expr(
                    t,
                    instructions,
                    env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    host_literals,
                    strings,
                )
            } else {
                *counter += 1;
                let d = format!("t{counter}");
                instructions.push(IrInst::ConstInt {
                    dest: d.clone(),
                    value: 0,
                });
                d
            }
        }
    }
}

/// Fields of a variant constructor that the new box owns: fresh strings and fresh
/// boxes of enums with fields. Anything else (names, user calls) may be someone
/// else's value, so the box leaves it alone.
fn enum_owned_mask(args: &[Expr], enums: &HashMap<String, crate::ast::EnumDef>) -> u64 {
    let mut mask = 0u64;
    for (i, a) in args.iter().enumerate().take(28) {
        let kind = if crate::drops::is_fresh_value(a) {
            1
        } else if let Expr::Call { func, args: inner, .. } = a {
            match func.as_ref() {
                Expr::Ident { name, .. }
                    if !inner.is_empty() && find_variant_in_enums(name, enums).is_some() =>
                {
                    2
                }
                _ => 0,
            }
        } else {
            0
        };
        mask |= kind << (2 * i);
    }
    mask
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
        // `o.i.a`: the struct type of `o.i` is the declared type of field `i`.
        Expr::Field { base: inner, field, .. } => {
            let outer = struct_name_of_base(inner, strings)?;
            let sdef = strings.structs.get(&outer)?;
            let f = sdef.fields.iter().find(|f| &f.name == field)?;
            match &f.ty {
                crate::ast::Type::Named { name, args, .. }
                    if args.is_empty() && strings.structs.contains_key(name) =>
                {
                    Some(name.clone())
                }
                _ => None,
            }
        }
        Expr::Call { func, args, .. } => match func.as_ref() {
            Expr::Ident { name, .. } if strings.structs.contains_key(name) => Some(name.clone()),
            // `list_get(xs, i).f`: the element type of `xs`. (A field shares its base's span,
            // so the call's own recorded type is shadowed by the field's.)
            Expr::Ident { name, .. } if name == "list_get" || name == "list_pop" => {
                match args.first().and_then(|a| strings.expr_type(a)) {
                    Some(crate::ast::Type::Named { name: l, args: targs, .. }) if l == "List" => {
                        match targs.first() {
                            Some(crate::ast::Type::Named { name, args, .. })
                                if args.is_empty() && strings.structs.contains_key(name) =>
                            {
                                Some(name.clone())
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            Expr::Ident { name, .. } => strings.fn_struct_rets.get(name).cloned(),
            _ => None,
        },
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

fn type_named_is(name: &str, ty: Option<&Type>) -> bool {
    matches!(ty, Some(Type::Named { name: n, .. }) if n == name)
}

fn match_scrut_uses_enum_switch(scrut_ty: Option<&Type>, strings: &StringEnv) -> bool {
    matches!(
        scrut_ty,
        Some(Type::Named { name, .. }) if strings.enums.contains_key(name)
    )
}

fn match_should_use_switch(
    arms: &[MatchArm],
    scrut_ty: Option<&Type>,
    strings: &StringEnv,
) -> bool {
    if !arms.iter().all(|a| {
        matches!(
            a.pattern,
            Pattern::Variant { .. } | Pattern::Ident(_, _) | Pattern::Wild(_)
        )
    }) {
        return false;
    }
    if type_named_is("String", scrut_ty) {
        return false;
    }
    if arms
        .iter()
        .any(|a| matches!(a.pattern, Pattern::Variant { .. }))
    {
        return true;
    }
    if type_named_is("Int", scrut_ty) {
        return true;
    }
    match_scrut_uses_enum_switch(scrut_ty, strings)
}

fn lower_match_chain(
    scrut: String,
    scrut_ty: Option<Type>,
    arms: &[MatchArm],
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    regions: &mut Vec<FusedRegion>,
    tensors: &mut HashMap<String, TensorInfo>,
    dim_params: &mut Vec<String>,
    fn_names: &HashMap<String, ()>,
    host_literals: &mut Vec<IrHostLiteral>,
    strings: &mut StringEnv,
) -> String {
    if arms.is_empty() {
        *counter += 1;
        return format!("t{counter}");
    }
    if match_should_use_switch(arms, scrut_ty.as_ref(), strings) {
        return lower_match_switch(
            scrut,
            scrut_ty,
            arms,
            instructions,
            env,
            counter,
            regions,
            tensors,
            dim_params,
            fn_names,
            host_literals,
            strings,
        );
    }
    let arm = &arms[0];
    let rest = &arms[1..];
    let (cond_ssa, mut arm_env) = match_arm_cond(
        &scrut,
        &arm.pattern,
        scrut_ty.as_ref(),
        counter,
        instructions,
        strings,
    );
    note_variant_binding(&arm.pattern, scrut_ty.as_ref(), strings);
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
        host_literals,
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
        scrut_ty.clone(),
        rest,
        instructions,
        env,
        counter,
        regions,
        tensors,
        dim_params,
        fn_names,
        host_literals,
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
    scrut_ty: Option<Type>,
    arms: &[MatchArm],
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
    regions: &mut Vec<FusedRegion>,
    tensors: &mut HashMap<String, TensorInfo>,
    dim_params: &mut Vec<String>,
    fn_names: &HashMap<String, ()>,
    host_literals: &mut Vec<IrHostLiteral>,
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
                    host_literals,
                    strings,
                );
            }
            Pattern::Ident(vname, _) if find_variant_in_enums(vname, &strings.enums).is_some() => {
                let (edef, vdef) = find_variant_in_enums(vname, &strings.enums).unwrap();
                let tag = enum_variant_tag(edef, vdef);
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
                    host_literals,
                    strings,
                );
                switch_arms.push(SwitchArm { tag, body, value });
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
                    host_literals,
                    strings,
                );
                switch_arms.push(SwitchArm {
                    tag: 0,
                    body,
                    value,
                });
            }
            Pattern::Variant { name, args, .. } => {
                let tag = match find_variant_in_enums(name, &strings.enums) {
                    Some((edef, vdef)) => enum_variant_tag(edef, vdef),
                    None => variant_tag_for_name(name) + 1,
                };
                let mut body = Vec::new();
                let mut arm_env = env.clone();
                note_variant_binding(&arm.pattern, scrut_ty.as_ref(), strings);
                bind_variant_pattern_args(&scrut, args, &mut body, &mut arm_env, counter);
                let value = lower_expr(
                    &arm.body,
                    &mut body,
                    &mut arm_env,
                    counter,
                    regions,
                    tensors,
                    dim_params,
                    fn_names,
                    host_literals,
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

/// `Some(p)` over a struct payload: `p.field` needs to know `p` is that struct. A
/// generic field (`Option[T]`) takes its type from the scrutinee's type arguments.
fn note_variant_binding(pat: &Pattern, scrut_ty: Option<&Type>, strings: &mut StringEnv) {
    let Pattern::Variant { name, args, .. } = pat else {
        return;
    };
    let Some((edef, vdef)) = find_variant_in_enums(name, &strings.enums) else {
        return;
    };
    let scrut_args: &[Type] = match scrut_ty {
        Some(Type::Named { args, .. }) => args,
        _ => &[],
    };
    let mut named = Vec::new();
    for (arg, fty) in args.iter().zip(vdef.fields.iter()) {
        let Pattern::Ident(bind, _) = arg else {
            continue;
        };
        let resolved = match fty {
            Type::Named { name: tn, args: targs, .. } if targs.is_empty() => {
                match edef.type_params.iter().position(|p| p == tn) {
                    Some(i) => scrut_args.get(i).cloned(),
                    None => Some(fty.clone()),
                }
            }
            _ => None,
        };
        if let Some(Type::Named { name: sn, args: targs, .. }) = resolved {
            if targs.is_empty() && strings.structs.contains_key(&sn) {
                named.push((bind.clone(), sn));
            }
        }
    }
    for (bind, sn) in named {
        strings.struct_types.insert(bind, sn);
    }
}

/// Bind names from a variant's payload fields (`EnumField` per index). Nested
/// `Variant` patterns bind from the extracted sub-value.
fn bind_variant_pattern_args(
    scrut: &str,
    args: &[Pattern],
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
) {
    for (i, a) in args.iter().enumerate() {
        bind_one_match_payload(scrut, i as u32, a, instructions, env, counter);
    }
}

fn bind_one_match_payload(
    scrut: &str,
    field_index: u32,
    pat: &Pattern,
    instructions: &mut Vec<IrInst>,
    env: &mut HashMap<String, String>,
    counter: &mut u32,
) {
    *counter += 1;
    let payload = format!("t{counter}");
    instructions.push(IrInst::EnumField {
        dest: payload.clone(),
        value: scrut.to_string(),
        index: field_index,
    });
    match pat {
        Pattern::Ident(bind, _) => {
            env.insert(bind.clone(), payload);
        }
        Pattern::Variant { args, .. } => {
            bind_variant_pattern_args(&payload, args, instructions, env, counter);
        }
        Pattern::Wild(_)
        | Pattern::Int(_, _)
        | Pattern::Str(_, _)
        | Pattern::Regex(_, _) => {}
    }
}

fn match_arm_cond(
    scrut: &str,
    pat: &Pattern,
    scrut_ty: Option<&Type>,
    counter: &mut u32,
    instructions: &mut Vec<IrInst>,
    strings: &StringEnv,
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
        Pattern::Str(lit, _) => {
            *counter += 1;
            let rhs = format!("t{counter}");
            instructions.push(IrInst::ConstString {
                dest: rhs.clone(),
                value: lit.clone(),
            });
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::Call {
                dest: Some(dest.clone()),
                func: "sal_str_eq".into(),
                args: vec![scrut.to_string(), rhs],
            });
            (dest, env)
        }
        Pattern::Regex(re, _) => {
            *counter += 1;
            let rhs = format!("t{counter}");
            instructions.push(IrInst::ConstString {
                dest: rhs.clone(),
                value: re.clone(),
            });
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::Call {
                dest: Some(dest.clone()),
                func: "sal_regex_match".into(),
                args: vec![scrut.to_string(), rhs],
            });
            (dest, env)
        }
        Pattern::Ident(name, _) => {
            let enum_unit = match scrut_ty {
                Some(Type::Named { name: en, .. }) => strings.enums.get(en).and_then(|edef| {
                    edef.variants
                        .iter()
                        .find(|v| v.name == *name && v.fields.is_empty())
                        .map(|_| ())
                }),
                _ => None,
            };
            if enum_unit.is_some() {
                *counter += 1;
                let dest = format!("t{counter}");
                instructions.push(IrInst::Binary {
                    dest: dest.clone(),
                    op: "eq".into(),
                    left: scrut.to_string(),
                    right: "0".into(),
                });
                return (dest, env);
            }
            env.insert(name.clone(), scrut.to_string());
            *counter += 1;
            let dest = format!("t{counter}");
            instructions.push(IrInst::ConstInt {
                dest: dest.clone(),
                value: 1,
            });
            (dest, env)
        }
        Pattern::Variant { name, args, .. } => {
            let vtag = variant_tag_for_name(name) + 1;
            *counter += 1;
            let low = format!("t{counter}");
            instructions.push(IrInst::EnumTag {
                dest: low.clone(),
                value: scrut.to_string(),
            });
            *counter += 1;
            let tag_tmp = format!("t{counter}");
            instructions.push(IrInst::Binary {
                dest: tag_tmp.clone(),
                op: "eq".into(),
                left: low,
                right: vtag.to_string(),
            });
            bind_variant_pattern_args(scrut, args, instructions, &mut env, counter);
            (tag_tmp, env)
        }
    }
}

/// Low-byte tag `EnumMake` gives a variant: `index + 1`, except a payload-free
/// first variant, which is 0.
fn enum_variant_tag(edef: &crate::ast::EnumDef, vdef: &crate::ast::EnumVariant) -> i64 {
    let idx = enum_variant_index(edef, &vdef.name).unwrap_or(0) as i64;
    if vdef.fields.is_empty() && idx == 0 {
        0
    } else {
        idx + 1
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
    env: &HashMap<String, String>,
    tensors: &HashMap<String, TensorInfo>,
    dim_params: &mut Vec<String>,
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> (String, String, String) {
    let lhs_info = tensor_info_for_ssa(lhs, env, tensors);
    let rhs_info = tensor_info_for_ssa(rhs, env, tensors);
    let li = lhs_info.as_ref();
    let ri = rhs_info.as_ref();
    let m = matmul_batch_m_ssa(li, lhs, dim_params, instructions, counter);
    let k = axis_len_ssa(li, lhs, /*row*/ false, dim_params, instructions, counter);
    let n = axis_len_ssa(ri, rhs, /*row*/ false, dim_params, instructions, counter);
    (m, k, n)
}

/// Product of leading axes of `lhs` (all but the last two) flattened into batch `m`.
fn matmul_batch_m_ssa(
    info: Option<&TensorInfo>,
    tensor: &str,
    dim_params: &mut Vec<String>,
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> String {
    let Some(t) = info else {
        *counter += 1;
        let dest = format!("t{counter}");
        instructions.push(IrInst::ConstInt { dest: dest.clone(), value: 1 });
        return dest;
    };
    if t.dims.len() <= 2 {
        return axis_len_ssa(Some(t), tensor, true, dim_params, instructions, counter);
    }
    let lead = &t.dims[0..t.dims.len() - 2];
    let mut acc = axis_len_ssa_for_dim(t, tensor, 0, &lead[0], dim_params, instructions, counter);
    for (i, d) in lead.iter().enumerate().skip(1) {
        let next = axis_len_ssa_for_dim(t, tensor, i, d, dim_params, instructions, counter);
        *counter += 1;
        let dest = format!("t{counter}");
        instructions.push(IrInst::Binary {
            dest: dest.clone(),
            op: "mul".into(),
            left: acc,
            right: next,
        });
        acc = dest;
    }
    acc
}

fn axis_len_ssa_for_dim(
    t: &TensorInfo,
    tensor: &str,
    idx: usize,
    d: &Dim,
    dim_params: &mut Vec<String>,
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> String {
    match (d, t.concrete.get(idx).copied().flatten()) {
        (Dim::Static(n), _) => push_const_dim(*n, instructions, counter),
        (Dim::Dynamic, Some(n)) => push_const_dim(n, instructions, counter),
        (Dim::Dynamic, None) => {
            let name = format!("{tensor}_d{idx}");
            let _ = dim_params;
            name
        }
    }
}

fn axis_len_ssa(
    info: Option<&TensorInfo>,
    tensor: &str,
    take_row: bool,
    dim_params: &mut Vec<String>,
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> String {
    let Some(t) = info else {
        // Unknown shape: still name a runtime length (never hardcode 0 here).
        let name = format!("{tensor}_d{}", if take_row { 0 } else { 1 });
        return name;
    };
    if t.dims.len() < 2 {
        if let Some(d) = t.dims.first() {
            return axis_len_ssa_for_dim(t, tensor, 0, d, dim_params, instructions, counter);
        }
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
    if let Some(i) = env.get(ssa).and_then(|n| tensors.get(n)) {
        return Some(i.clone());
    }
    for (name, bound) in env {
        if bound == ssa {
            if let Some(i) = tensors.get(name) {
                return Some(i.clone());
            }
        }
    }
    None
}

fn tensor_place_name(info: &TensorInfo, _env: &HashMap<String, String>) -> String {
    place_str(&info.place)
}

fn tensor_len_operand(
    info: &TensorInfo,
    tensor_ssa: &str,
    dim_params: &mut Vec<String>,
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
    dim_params: &mut Vec<String>,
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
    // `scalar op tensor`: the kernel takes the scalar on the right, so swap the
    // operands (flipping order comparisons) or rewrite `s - t` as `-t + s`.
    let scalar_left = !expr_has_tensor(left, env, tensors);
    let mut op = op.clone();
    let mut sub_neg: Option<String> = None;
    if scalar_left {
        op = match op {
            BinOp::Lt => BinOp::Gt,
            BinOp::Le => BinOp::Ge,
            BinOp::Gt => BinOp::Lt,
            BinOp::Ge => BinOp::Le,
            BinOp::Sub => {
                *counter += 1;
                let neg = format!("t{counter}");
                instructions.push(IrInst::TensorUnary {
                    dest: neg.clone(),
                    op: "neg".into(),
                    elem: info.elem,
                    place: place.clone(),
                    input: tensor_side.to_string(),
                    len: len.clone(),
                });
                tensors.insert(neg.clone(), info.clone());
                sub_neg = Some(neg);
                BinOp::Add
            }
            other => other,
        };
    }
    let op = &op;
    let (l_ssa, other_ssa) = match (scalar_left, sub_neg.as_deref()) {
        (false, _) => (l_ssa, other_ssa),
        (true, Some(neg)) => (neg, l_ssa),
        (true, None) => (r_ssa, l_ssa),
    };
    let op_s = match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
        BinOp::Mod => "mod",
        BinOp::Band => "band",
        BinOp::Bxor => "bxor",
        BinOp::Bor => "bor",
        BinOp::Shl => "shl",
        BinOp::Shr => "shr",
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        BinOp::Lt => "lt",
        BinOp::Le => "le",
        BinOp::Gt => "gt",
        BinOp::Ge => "ge",
        BinOp::And | BinOp::Or => unreachable!("`&&` and `||` are lowered as `if`"),
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
                    BinOp::Mod => "mod",
                    BinOp::Band => "band",
                    BinOp::Bxor => "bxor",
                    BinOp::Bor => "bor",
                    BinOp::Shl => "shl",
                    BinOp::Shr => "shr",
                    BinOp::Eq => "eq",
                    BinOp::Ne => "ne",
                    BinOp::Lt => "lt",
                    BinOp::Le => "le",
                    BinOp::Gt => "gt",
                    BinOp::Ge => "ge",
                    BinOp::And | BinOp::Or => unreachable!("`&&` and `||` are lowered as `if`"),
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
                "relu" | "map" | "silu" => {
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

fn rope_seq_dim_ssa(
    info: Option<&TensorInfo>,
    tensor: &str,
    dim_params: &mut Vec<String>,
    instructions: &mut Vec<IrInst>,
    counter: &mut u32,
) -> (String, String) {
    let Some(t) = info else {
        let one = push_const_dim(1, instructions, counter);
        return (one.clone(), one);
    };
    if t.dims.len() >= 2 {
        let seq = axis_len_ssa_for_dim(t, tensor, 0, &t.dims[0], dim_params, instructions, counter);
        let dim = axis_len_ssa_for_dim(
            t,
            tensor,
            t.dims.len() - 1,
            t.dims.last().unwrap(),
            dim_params,
            instructions,
            counter,
        );
        return (seq, dim);
    }
    (push_const_dim(1, instructions, counter), push_const_dim(1, instructions, counter))
}

fn attention_shape_q(info: Option<&TensorInfo>, tensor: &str) -> (String, String, String) {
    match info {
        Some(t) if t.dims.len() >= 3 => (
            dim_lit_or_name(t, tensor, 0, "1"),
            dim_lit_or_name(t, tensor, 1, "1"),
            dim_lit_or_name(t, tensor, 2, "1"),
        ),
        _ => ("1".into(), "1".into(), "1".into()),
    }
}

fn attention_shape_kv(info: Option<&TensorInfo>, tensor: &str) -> String {
    match info {
        Some(t) if t.dims.len() >= 2 => dim_lit_or_name(t, tensor, 1, "1"),
        _ => "1".into(),
    }
}

fn dim_lit_or_name(t: &TensorInfo, tensor: &str, idx: usize, fallback: &str) -> String {
    match (t.dims.get(idx), t.concrete.get(idx).copied().flatten()) {
        (Some(Dim::Static(n)), _) => n.to_string(),
        (Some(Dim::Dynamic), Some(n)) => n.to_string(),
        (Some(Dim::Dynamic), None) => format!("{tensor}_d{idx}"),
        _ => fallback.into(),
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
            // Selfhost's emitter omits an empty `carried` list. Debug always prints it.
            let line = format!("{i:?}").replace(", carried: []", "");
            s.push_str(&line);
            s.push('\n');
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
