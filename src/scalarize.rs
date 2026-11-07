//! Struct values without the heap.
//!
//! The IR keeps a multi-field struct as one SSA name (`Aggregate` / `Extract`), which
//! the LLVM backend used to turn into a `malloc`ed block of `i64` slots. This pass runs
//! just before LLVM emission and rewrites every struct value it can type into one
//! scalar SSA name per leaf field, so a struct costs nothing but registers:
//!
//! - `Aggregate` becomes bookkeeping (the field operands are the slots) and `Extract`
//!   becomes a copy of one slot;
//! - parameters are flattened, and a function returning a struct gets a hidden first
//!   parameter `sret` pointing at a caller-side stack buffer (`SretStore` / `CallSret`);
//! - `if` / `while` / `match` carry one phi per slot.
//!
//! A struct is boxed back into the old heap form (`Aggregate`) only where it flows into
//! something that holds plain `i64` values: runtime builtins, containers, enum payloads.
//! The inverse (a handle read from a container, used as a struct argument) unboxes with
//! `Extract`. Both forms share the layout: one slot per field, nested structs boxed.

use std::collections::{HashMap, HashSet};

use crate::ast::{FnDef, Item, Program, StructDef, Type};
use crate::ir::{IfCarry, IrFunction, IrInst, IrModule, SwitchArm, WhileCarry};

/// Scalarised view of a function signature. `None` entries are ordinary `i64` values.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FnSig {
    pub params: Vec<Option<String>>,
    pub ret: Option<String>,
}

/// Name of the struct that `ty` is passed as, when it is scalarised.
///
/// Only non-generic structs with at least two fields and no tensor columns qualify
/// (frames keep their handle form because `where` reads columns through it). A
/// single-field struct is a transparent newtype whose value is its field.
pub fn agg_struct(ty: &Type, structs: &HashMap<String, StructDef>) -> Option<String> {
    agg_struct_rec(ty, structs, 0)
}

fn agg_struct_rec(ty: &Type, structs: &HashMap<String, StructDef>, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let Type::Named { name, args, .. } = ty else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let def = structs.get(name)?;
    if !def.type_params.is_empty() {
        return None;
    }
    match def.fields.len() {
        0 => None,
        1 => agg_struct_rec(&def.fields[0].ty, structs, depth + 1),
        _ => {
            if def.fields.iter().any(|f| matches!(f.ty, Type::Tensor { .. })) {
                None
            } else {
                Some(name.clone())
            }
        }
    }
}

pub fn fn_sig(f: &FnDef, structs: &HashMap<String, StructDef>) -> FnSig {
    FnSig {
        params: f.params.iter().map(|p| agg_struct(&p.ty, structs)).collect(),
        ret: agg_struct(&f.ret, structs),
    }
}

/// Signatures of every function in `progs`, keyed by name.
pub fn sigs_for_programs(
    progs: &[&Program],
    structs: &HashMap<String, StructDef>,
) -> HashMap<String, FnSig> {
    let mut out = HashMap::new();
    for prog in progs {
        for item in &prog.items {
            if let Item::Fn(f) = item {
                out.entry(f.name.clone()).or_insert_with(|| fn_sig(f, structs));
            }
        }
    }
    out
}

/// Number of leaf slots of a scalarised struct.
pub fn slot_count(name: &str, structs: &HashMap<String, StructDef>) -> usize {
    let Some(def) = structs.get(name) else {
        return 1;
    };
    def.fields
        .iter()
        .map(|f| match agg_struct(&f.ty, structs) {
            Some(inner) => slot_count(&inner, structs),
            None => 1,
        })
        .sum()
}

/// Parameter names and slot count of the flattened signature (`sret` included).
pub fn flat_arity(sig: &FnSig, structs: &HashMap<String, StructDef>) -> usize {
    let params: usize = sig
        .params
        .iter()
        .map(|p| match p {
            Some(sn) => slot_count(sn, structs),
            None => 1,
        })
        .sum();
    params + usize::from(sig.ret.is_some())
}

#[derive(Clone)]
struct Val {
    name: String,
    slots: Vec<String>,
}

#[derive(Clone, Default)]
struct Scope {
    vals: HashMap<String, Val>,
    boxed: HashMap<String, String>,
}

struct Pass<'a> {
    structs: &'a HashMap<String, StructDef>,
    sigs: &'a HashMap<String, FnSig>,
    ret: Option<String>,
    fresh: u32,
    /// SSA name of a string constant.
    strs: HashMap<String, String>,
    /// `sal_desc_copy` of a struct (`S...`): the box is freed after its fields move out.
    copies: HashSet<String>,
}

pub fn scalarize_module(
    m: &IrModule,
    structs: &HashMap<String, StructDef>,
    sigs: &HashMap<String, FnSig>,
) -> IrModule {
    IrModule {
        functions: m
            .functions
            .iter()
            .map(|f| scalarize_fn(f, structs, sigs))
            .collect(),
        any_vtables: m.any_vtables.clone(),
    }
}

fn scalarize_fn(
    f: &IrFunction,
    structs: &HashMap<String, StructDef>,
    sigs: &HashMap<String, FnSig>,
) -> IrFunction {
    let sig = match sigs.get(&f.name) {
        Some(s) if s.params.len() == f.params.len() => s.clone(),
        _ => FnSig {
            params: vec![None; f.params.len()],
            ret: None,
        },
    };
    let mut p = Pass {
        structs,
        sigs,
        ret: sig.ret.clone(),
        fresh: 0,
        strs: HashMap::new(),
        copies: HashSet::new(),
    };
    let mut scope = Scope::default();
    let mut params = Vec::new();
    if sig.ret.is_some() {
        params.push("sret".to_string());
    }
    for (name, sn) in f.params.iter().zip(sig.params.iter()) {
        match sn {
            Some(sn) => {
                let n = slot_count(sn, structs);
                let slots: Vec<String> = (0..n).map(|i| format!("{name}_s{i}")).collect();
                params.extend(slots.iter().cloned());
                scope.vals.insert(
                    name.clone(),
                    Val {
                        name: sn.clone(),
                        slots,
                    },
                );
            }
            None => params.push(name.clone()),
        }
    }
    let instructions = p.list(&f.instructions, &mut scope);
    IrFunction {
        name: f.name.clone(),
        params,
        instructions,
        regions: f.regions.clone(),
        dim_params: f.dim_params.clone(),
        host_literals: f.host_literals.clone(),
    }
}

impl<'a> Pass<'a> {
    fn fresh(&mut self) -> String {
        self.fresh += 1;
        format!("sv{}", self.fresh)
    }

    /// Slot layout of a struct field: `Some(inner)` when it is itself scalarised.
    fn field_kinds(&self, sn: &str) -> Vec<Option<String>> {
        self.structs
            .get(sn)
            .map(|d| d.fields.iter().map(|f| agg_struct(&f.ty, self.structs)).collect())
            .unwrap_or_default()
    }

    /// Heap form of a scalarised value; the instructions that build it go to `out`.
    fn box_val(&mut self, v: &Val, scope: &mut Scope, out: &mut Vec<IrInst>) -> String {
        if let Some(b) = scope.boxed.get(&v.name) {
            return b.clone();
        }
        let handle = self.box_slots(&v.name, &v.slots, out);
        handle
    }

    fn box_slots(&mut self, sn: &str, slots: &[String], out: &mut Vec<IrInst>) -> String {
        let kinds = self.field_kinds(sn);
        let mut fields = Vec::new();
        let mut at = 0usize;
        for k in &kinds {
            match k {
                Some(inner) => {
                    let n = slot_count(inner, self.structs);
                    let h = self.box_slots(inner, &slots[at..at + n], out);
                    fields.push(h);
                    at += n;
                }
                None => {
                    fields.push(slots[at].clone());
                    at += 1;
                }
            }
        }
        let dest = self.fresh();
        out.push(IrInst::Aggregate {
            dest: dest.clone(),
            struct_name: sn.to_string(),
            fields,
        });
        dest
    }

    /// Operand in plain `i64` form: a scalarised value is boxed first.
    fn plain(&mut self, name: &str, scope: &mut Scope, out: &mut Vec<IrInst>) -> String {
        match scope.vals.get(name).cloned() {
            Some(v) => {
                let h = self.box_val(&v, scope, out);
                scope.boxed.insert(name.to_string(), h.clone());
                h
            }
            None => name.to_string(),
        }
    }

    /// Slots of `name` as a value of struct `sn`, unboxing a handle if needed.
    fn slots_of(&mut self, name: &str, sn: &str, scope: &mut Scope, out: &mut Vec<IrInst>) -> Vec<String> {
        if let Some(v) = scope.vals.get(name) {
            if v.name == sn {
                return v.slots.clone();
            }
        }
        let name = self.plain(name, scope, out);
        self.unbox(&name, sn, out)
    }

    fn unbox(&mut self, handle: &str, sn: &str, out: &mut Vec<IrInst>) -> Vec<String> {
        let kinds = self.field_kinds(sn);
        let mut slots = Vec::new();
        for (i, k) in kinds.iter().enumerate() {
            let d = self.fresh();
            out.push(IrInst::Extract {
                dest: d.clone(),
                base: handle.to_string(),
                struct_name: sn.to_string(),
                field_index: i as u32,
            });
            match k {
                Some(inner) => slots.extend(self.unbox(&d, inner, out)),
                None => slots.push(d),
            }
        }
        slots
    }

    fn list(&mut self, insts: &[IrInst], scope: &mut Scope) -> Vec<IrInst> {
        let mut out = Vec::new();
        for inst in insts {
            self.inst(inst, scope, &mut out);
        }
        out
    }

    fn plain_all(&mut self, names: &[String], scope: &mut Scope, out: &mut Vec<IrInst>) -> Vec<String> {
        names.iter().map(|n| self.plain(n, scope, out)).collect()
    }

    fn inst(&mut self, inst: &IrInst, scope: &mut Scope, out: &mut Vec<IrInst>) {
        match inst {
            IrInst::ConstInt { .. } | IrInst::ConstFloat { .. } | IrInst::Drop { .. } => out.push(inst.clone()),
            IrInst::ConstString { dest, value } => {
                self.strs.insert(dest.clone(), value.clone());
                out.push(inst.clone());
            }
            IrInst::Binary { dest, op, left, right } => {
                let l = self.plain(left, scope, out);
                let r = self.plain(right, scope, out);
                out.push(IrInst::Binary {
                    dest: dest.clone(),
                    op: op.clone(),
                    left: l,
                    right: r,
                });
            }
            IrInst::EnumTag { dest, value } => {
                let v = self.plain(value, scope, out);
                out.push(IrInst::EnumTag {
                    dest: dest.clone(),
                    value: v,
                });
            }
            IrInst::EnumField { dest, value, index } => {
                let v = self.plain(value, scope, out);
                out.push(IrInst::EnumField {
                    dest: dest.clone(),
                    value: v,
                    index: *index,
                });
            }
            IrInst::TensorBin { dest, op, elem, place, left, right, len, right_scalar } => {
                let l = self.plain(left, scope, out);
                let r = self.plain(right, scope, out);
                out.push(IrInst::TensorBin {
                    dest: dest.clone(),
                    op: op.clone(),
                    elem: *elem,
                    place: place.clone(),
                    left: l,
                    right: r,
                    len: len.clone(),
                    right_scalar: *right_scalar,
                });
            }
            IrInst::TensorUnary { dest, op, elem, place, input, len } => {
                let i = self.plain(input, scope, out);
                out.push(IrInst::TensorUnary {
                    dest: dest.clone(),
                    op: op.clone(),
                    elem: *elem,
                    place: place.clone(),
                    input: i,
                    len: len.clone(),
                });
            }
            IrInst::PlaceCopy { dest, from, to_place } => {
                let f = self.plain(from, scope, out);
                out.push(IrInst::PlaceCopy {
                    dest: dest.clone(),
                    from: f,
                    to_place: to_place.clone(),
                });
            }
            IrInst::TensorRebind { dest, src } => {
                let s = self.plain(src, scope, out);
                out.push(IrInst::TensorRebind {
                    dest: dest.clone(),
                    src: s,
                });
            }
            IrInst::EnumMake { dest, enum_name, variant_index, payload, owned } => {
                let payload = payload.iter().map(|p| self.plain(p, scope, out)).collect();
                out.push(IrInst::EnumMake {
                    dest: dest.clone(),
                    enum_name: enum_name.clone(),
                    variant_index: *variant_index,
                    payload,
                    owned: *owned,
                });
            }
            IrInst::Aggregate { dest, struct_name, fields } => {
                if agg_struct(
                    &Type::Named {
                        name: struct_name.clone(),
                        args: vec![],
                        span: Default::default(),
                    },
                    self.structs,
                )
                .as_deref()
                    == Some(struct_name.as_str())
                {
                    let kinds = self.field_kinds(struct_name);
                    if kinds.len() == fields.len() {
                        let mut slots = Vec::new();
                        for (f, k) in fields.iter().zip(kinds.iter()) {
                            match k {
                                Some(inner) => slots.extend(self.slots_of(f, inner, scope, out)),
                                None => {
                                    let p = self.plain(f, scope, out);
                                    slots.push(p);
                                }
                            }
                        }
                        scope.vals.insert(
                            dest.clone(),
                            Val {
                                name: struct_name.clone(),
                                slots,
                            },
                        );
                        return;
                    }
                }
                let fields = self.plain_all(fields, scope, out);
                out.push(IrInst::Aggregate {
                    dest: dest.clone(),
                    struct_name: struct_name.clone(),
                    fields,
                });
            }
            IrInst::Extract { dest, base, struct_name, field_index } => {
                if let Some(v) = scope.vals.get(base).cloned() {
                    let kinds = self.field_kinds(&v.name);
                    let idx = *field_index as usize;
                    if idx < kinds.len() {
                        let mut at = 0usize;
                        for k in &kinds[..idx] {
                            at += match k {
                                Some(inner) => slot_count(inner, self.structs),
                                None => 1,
                            };
                        }
                        match &kinds[idx] {
                            Some(inner) => {
                                let n = slot_count(inner, self.structs);
                                scope.vals.insert(
                                    dest.clone(),
                                    Val {
                                        name: inner.clone(),
                                        slots: v.slots[at..at + n].to_vec(),
                                    },
                                );
                            }
                            None => out.push(IrInst::Binary {
                                dest: dest.clone(),
                                op: "add".into(),
                                left: v.slots[at].clone(),
                                right: "0".into(),
                            }),
                        }
                        return;
                    }
                }
                out.push(IrInst::Extract {
                    dest: dest.clone(),
                    base: base.clone(),
                    struct_name: struct_name.clone(),
                    field_index: *field_index,
                });
            }
            IrInst::Call { dest, func, args } => self.call(dest, func, args, scope, out),
            IrInst::AnyBox { .. } | IrInst::AnyCall { .. } => out.push(inst.clone()),
            IrInst::CallSret { .. } | IrInst::SretStore { .. } => out.push(inst.clone()),
            IrInst::Return { value } => {
                let copy = self.copies.contains(value);
                if let Some(rs) = self.ret.clone() {
                    let slots = self.slots_of(value, &rs, scope, out);
                    if copy {
                        out.push(IrInst::Call {
                            dest: None,
                            func: "sal_struct_free_box".into(),
                            args: vec![value.clone()],
                        });
                        self.copies.remove(value);
                    }
                    out.push(IrInst::SretStore { values: slots });
                    out.push(IrInst::Return { value: "0".into() });
                } else {
                    let v = self.plain(value, scope, out);
                    out.push(IrInst::Return { value: v });
                }
            }
            IrInst::If { cond, then_body, else_body, then_val, else_val, dest, carried } => {
                self.if_inst(cond, then_body, else_body, then_val, else_val, dest, carried, scope, out)
            }
            IrInst::While { cond_insts, cond, body, carried } => {
                self.while_inst(cond_insts, cond, body, carried, scope, out)
            }
            IrInst::Switch { scrut, arms, default_body, default_val, dest } => {
                let scrut = self.plain(scrut, scope, out);
                let mut new_arms = Vec::new();
                for arm in arms {
                    let mut sc = scope.clone();
                    let mut body = self.list(&arm.body, &mut sc);
                    let value = self.plain_in_body(&arm.value, &mut sc, &mut body);
                    new_arms.push(SwitchArm {
                        tag: arm.tag,
                        body,
                        value,
                    });
                }
                let mut sc = scope.clone();
                let mut dbody = self.list(default_body, &mut sc);
                let dval = self.plain_in_body(default_val, &mut sc, &mut dbody);
                out.push(IrInst::Switch {
                    scrut,
                    arms: new_arms,
                    default_body: dbody,
                    default_val: dval,
                    dest: dest.clone(),
                });
            }
            IrInst::KernelGrid { place, index_names, bounds, body, tail, dest } => {
                let mut sc = scope.clone();
                let mut b = self.list(body, &mut sc);
                let t = self.plain_in_body(tail, &mut sc, &mut b);
                out.push(IrInst::KernelGrid {
                    place: place.clone(),
                    index_names: index_names.clone(),
                    bounds: bounds.clone(),
                    body: b,
                    tail: t,
                    dest: dest.clone(),
                });
            }
        }
    }

    /// `plain` for a value that leaves a nested body: the boxing goes at its end,
    /// unless the body already ended in a `Return`.
    fn plain_in_body(&mut self, name: &str, scope: &mut Scope, body: &mut Vec<IrInst>) -> String {
        if !scope.vals.contains_key(name) {
            return name.to_string();
        }
        if matches!(body.last(), Some(IrInst::Return { .. })) {
            return "0".into();
        }
        self.plain(name, scope, body)
    }

    fn call(
        &mut self,
        dest: &Option<String>,
        func: &str,
        args: &[String],
        scope: &mut Scope,
        out: &mut Vec<IrInst>,
    ) {
        let Some(sig) = self.sigs.get(func).cloned() else {
            let args = self.plain_all(args, scope, out);
            if func == "sal_desc_copy" {
                if let (Some(d), Some(desc)) = (dest, args.first()) {
                    if self.strs.get(desc).is_some_and(|s| s.starts_with('S')) {
                        self.copies.insert(d.clone());
                    }
                }
            }
            out.push(IrInst::Call {
                dest: dest.clone(),
                func: func.to_string(),
                args,
            });
            return;
        };
        let mut flat = Vec::new();
        for (i, a) in args.iter().enumerate() {
            match sig.params.get(i).cloned().flatten() {
                Some(sn) => flat.extend(self.slots_of(a, &sn, scope, out)),
                None => {
                    let p = self.plain(a, scope, out);
                    flat.push(p);
                }
            }
        }
        match sig.ret {
            Some(rs) => {
                let n = slot_count(&rs, self.structs);
                let dests: Vec<String> = (0..n).map(|_| self.fresh()).collect();
                out.push(IrInst::CallSret {
                    dests: dests.clone(),
                    func: func.to_string(),
                    args: flat,
                });
                if let Some(d) = dest {
                    scope.vals.insert(
                        d.clone(),
                        Val {
                            name: rs,
                            slots: dests,
                        },
                    );
                }
            }
            None => out.push(IrInst::Call {
                dest: dest.clone(),
                func: func.to_string(),
                args: flat,
            }),
        }
    }

    /// Struct both branches agree on, if any.
    fn merge_ty(&self, a: &str, sa: &Scope, b: &str, sb: &Scope) -> Option<String> {
        match (sa.vals.get(a), sb.vals.get(b)) {
            (Some(x), Some(y)) if x.name == y.name => Some(x.name.clone()),
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn if_inst(
        &mut self,
        cond: &str,
        then_body: &[IrInst],
        else_body: &[IrInst],
        then_val: &str,
        else_val: &str,
        dest: &str,
        carried: &[IfCarry],
        scope: &mut Scope,
        out: &mut Vec<IrInst>,
    ) {
        let cond = self.plain(cond, scope, out);
        let mut ts = scope.clone();
        let mut es = scope.clone();
        let mut tb = self.list(then_body, &mut ts);
        let mut eb = self.list(else_body, &mut es);
        let mut phis: Vec<(String, String, String)> = vec![(dest.to_string(), then_val.to_string(), else_val.to_string())];
        for c in carried {
            phis.push((c.dest.clone(), c.then_val.clone(), c.else_val.clone()));
        }
        let mut new_phis: Vec<IfCarry> = Vec::new();
        let mut produced: Vec<(String, Val)> = Vec::new();
        for (d, tv, ev) in &phis {
            if let Some(sn) = self.merge_ty(tv, &ts, ev, &es) {
                let ta = ts.vals[tv].slots.clone();
                let ea = es.vals[ev].slots.clone();
                let slots: Vec<String> = ta.iter().map(|_| self.fresh()).collect();
                for ((s, t), e) in slots.iter().zip(ta.iter()).zip(ea.iter()) {
                    new_phis.push(IfCarry {
                        dest: s.clone(),
                        then_val: t.clone(),
                        else_val: e.clone(),
                    });
                }
                produced.push((d.clone(), Val { name: sn, slots }));
            } else {
                let t = self.plain_in_body(tv, &mut ts, &mut tb);
                let e = self.plain_in_body(ev, &mut es, &mut eb);
                new_phis.push(IfCarry {
                    dest: d.clone(),
                    then_val: t,
                    else_val: e,
                });
            }
        }
        // The primary phi must stay first and plain; a struct phi has no primary slot,
        // so give the primary position a dummy plain phi.
        let primary_is_struct = produced.iter().any(|(d, _)| d == dest);
        let (pdest, pthen, pelse, rest) = if primary_is_struct {
            let rest = new_phis;
            ("0".to_string(), "0".to_string(), "0".to_string(), rest)
        } else {
            let mut it = new_phis.into_iter();
            let p = it.next().unwrap();
            (p.dest, p.then_val, p.else_val, it.collect::<Vec<_>>())
        };
        let pdest_real = if primary_is_struct { self.fresh() } else { pdest };
        out.push(IrInst::If {
            cond,
            then_body: tb,
            else_body: eb,
            then_val: pthen,
            else_val: pelse,
            dest: pdest_real,
            carried: rest,
        });
        for (d, v) in produced {
            scope.vals.insert(d, v);
        }
    }

    fn while_inst(
        &mut self,
        cond_insts: &[IrInst],
        cond: &str,
        body: &[IrInst],
        carried: &[WhileCarry],
        scope: &mut Scope,
        out: &mut Vec<IrInst>,
    ) {
        let mut inner = scope.clone();
        // Decide, per carry, whether the loop variable is scalarised (by its incoming value).
        struct Plan {
            dest: String,
            incoming: Vec<String>,
            struct_name: Option<String>,
            slots: Vec<String>,
            plain_incoming: String,
        }
        let mut plans: Vec<Plan> = Vec::new();
        for c in carried {
            match scope.vals.get(&c.incoming).cloned() {
                Some(v) => {
                    let slots: Vec<String> = v.slots.iter().map(|_| self.fresh()).collect();
                    inner.vals.insert(
                        c.dest.clone(),
                        Val {
                            name: v.name.clone(),
                            slots: slots.clone(),
                        },
                    );
                    plans.push(Plan {
                        dest: c.dest.clone(),
                        incoming: v.slots.clone(),
                        struct_name: Some(v.name),
                        slots,
                        plain_incoming: String::new(),
                    });
                }
                None => {
                    let pi = c.incoming.clone();
                    // The loop variable is a plain value inside the loop.
                    inner.vals.remove(&c.dest);
                    plans.push(Plan {
                        dest: c.dest.clone(),
                        incoming: Vec::new(),
                        struct_name: None,
                        slots: Vec::new(),
                        plain_incoming: pi,
                    });
                }
            }
        }
        let new_cond_insts = self.list(cond_insts, &mut inner);
        let cond = self.plain(cond, &mut inner, &mut Vec::new());
        let mut new_body = self.list(body, &mut inner);
        let mut new_carried = Vec::new();
        for (c, plan) in carried.iter().zip(plans.iter()) {
            match &plan.struct_name {
                Some(sn) => {
                    let upd = self.slots_of(&c.updated, sn, &mut inner, &mut new_body);
                    for ((d, i), u) in plan.slots.iter().zip(plan.incoming.iter()).zip(upd.iter()) {
                        new_carried.push(WhileCarry {
                            dest: d.clone(),
                            incoming: i.clone(),
                            updated: u.clone(),
                        });
                    }
                }
                None => {
                    let upd = self.plain_in_body(&c.updated, &mut inner, &mut new_body);
                    new_carried.push(WhileCarry {
                        dest: plan.dest.clone(),
                        incoming: plan.plain_incoming.clone(),
                        updated: upd,
                    });
                }
            }
        }
        out.push(IrInst::While {
            cond_insts: new_cond_insts,
            cond,
            body: new_body,
            carried: new_carried,
        });
        for plan in plans {
            if let Some(sn) = plan.struct_name {
                scope.vals.insert(
                    plan.dest,
                    Val {
                        name: sn,
                        slots: plan.slots,
                    },
                );
            }
        }
    }
}
