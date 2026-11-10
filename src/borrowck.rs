//! Borrow checker (`check_borrows`). Moves stay in `ownership.rs` (`E_MOVED`).
//! This pass adds `E_BORROW`: a loan that overlaps a mutation or another
//! exclusive loan, a reference that outlives its referent, and moving a
//! non-`Copy` value out of a reference (`*r`).

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diagnostic, DiagResult, ErrorCode};
use crate::infer::InferOutput;
use crate::span::Span;
use crate::typed::ExprTypeEntry;

#[derive(Clone)]
struct Loan {
    place: String,
    mutable: bool,
    start: u32,
    end: u32,
    span: Span,
    /// Extent of the expression that created the loan (`&p`, or the autoreferenced argument):
    /// a use inside it is the loan itself, not a conflict.
    site: (u32, u32),
    /// End of the scope of the local the loan refers to: the reference must not be used past it.
    referent_end: u32,
    /// Where the reference that holds the loan is used (end offsets). The loan is live at a
    /// point when one of these is still reachable from it.
    uses: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EventKind {
    Move,
    /// An assignment to the place, or a call that takes it by `&mut` through autoref.
    Mutation,
    Read,
}

struct Event {
    place: String,
    span: Span,
    kind: EventKind,
}

#[derive(Clone)]
struct Binding {
    ty: Type,
    /// Parameter whose type is already `&T` or `&mut T`: its referent outlives the fn.
    param_ref: bool,
    /// The loans this name holds (a reference, or a value that stores references).
    loans: Vec<usize>,
    scope_end: u32,
    /// Bound by a pattern over a reference (`match &o`): the name borrows the payload, so a
    /// non-`Copy` value does not move out of it.
    from_ref: bool,
}

struct Scope {
    vars: HashMap<String, Binding>,
    end: u32,
}

#[derive(Default)]
struct Src {
    /// `(place, mutable, extent of the borrowing expression)`.
    new: Vec<(String, bool, Span)>,
    shared: Vec<usize>,
}

impl Src {
    fn merge(&mut self, o: Src) {
        self.new.extend(o.new);
        self.shared.extend(o.shared);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamKind {
    Value,
    Shared,
    Mut,
    Unknown,
}

struct Cx {
    publish_moves: bool,
    types: HashMap<(u32, u32), Type>,
    structs: HashMap<String, StructDef>,
    sigs: HashMap<String, Vec<Type>>,
    ret_refs: bool,
    scopes: Vec<Scope>,
    loans: Vec<Loan>,
    events: Vec<Event>,
    diags: Vec<Diagnostic>,
    whiles: Vec<Span>,
    /// The branches of each `if` / `match`: control in one never reaches a sibling.
    groups: Vec<Vec<Span>>,
    /// Blocks that end in a `return` (the block, the offset of the `return`).
    diverge: Vec<(Span, u32)>,
}

thread_local! {
    /// Parameter types of the fns of the other modules of the graph, by final name.
    static EXTERN_SIGS: std::cell::RefCell<HashMap<String, Vec<Type>>> = std::cell::RefCell::new(HashMap::new());
}

/// Signatures of the fns this module can call from other modules. Without them a call across
/// modules is only checked by what its arguments say (`&x`, `&mut x`).
/// Whether an arm of a `match` over an owned scrutinee of type `ty` binds a non-`Copy` payload:
/// the `match` then moves the scrutinee (SPEC «Memoria»).
pub(crate) fn match_moves(ty: &Type, arms: &[MatchArm], structs: &HashMap<String, StructDef>) -> bool {
    arms.iter().any(|a| match &a.pattern {
        Pattern::Variant { name, args, .. } => args
            .iter()
            .enumerate()
            .any(|(i, p)| matches!(p, Pattern::Ident(..)) && payload_moves(name, i, ty, structs)),
        _ => false,
    })
}

thread_local! {
    /// Enums of the other modules of the graph, and of the program being checked.
    static EXTERN_ENUMS: std::cell::RefCell<Vec<EnumDef>> = const { std::cell::RefCell::new(Vec::new()) };
    static LOCAL_ENUMS: std::cell::RefCell<Vec<EnumDef>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The enums of every module of the graph: a `match` needs the payload types of a variant.
pub fn set_extern_enums(enums: Vec<EnumDef>) {
    EXTERN_ENUMS.with(|s| *s.borrow_mut() = enums);
}

/// A pattern name that is a variant without payload (`None`), not a binding.
fn is_variant_name(n: &str) -> bool {
    let last = n.rsplit("__").next().unwrap_or(n);
    let has = |enums: &Vec<EnumDef>| {
        enums.iter().any(|e| e.variants.iter().any(|v| v.name.rsplit("__").next().unwrap_or(&v.name) == last))
    };
    LOCAL_ENUMS.with(|s| has(&s.borrow())) || EXTERN_ENUMS.with(|s| has(&s.borrow()))
}

/// Whether binding field `index` of variant `name` moves a non-`Copy` value out of a scrutinee
/// of type `scrut`. An unknown variant or an unresolved type parameter does not move.
fn payload_moves(name: &str, index: usize, scrut: &Type, structs: &HashMap<String, StructDef>) -> bool {
    let last = name.rsplit("__").next().unwrap_or(name);
    let (sname, sargs) = match scrut {
        Type::Named { name, args, .. } | Type::Qualified { name, args, .. } => (name.as_str(), args.as_slice()),
        _ => return false,
    };
    let slast = sname.rsplit("__").next().unwrap_or(sname);
    let check = |enums: &Vec<EnumDef>| -> Option<bool> {
        for e in enums {
            let elast = e.name.rsplit("__").next().unwrap_or(&e.name);
            if elast != slast {
                continue;
            }
            let Some(v) = e.variants.iter().find(|v| v.name.rsplit("__").next().unwrap_or(&v.name) == last) else { continue };
            let Some(ft) = v.fields.get(index) else { return Some(false) };
            if let Type::Named { name: tn, args, .. } = ft {
                if args.is_empty() {
                    if let Some(i) = e.type_params.iter().position(|p| p == tn) {
                        return Some(sargs.get(i).is_some_and(|a| !is_unknown(a) && !is_copy(a, structs)));
                    }
                }
            }
            return Some(!is_copy(ft, structs));
        }
        None
    };
    LOCAL_ENUMS
        .with(|s| check(&s.borrow()))
        .or_else(|| EXTERN_ENUMS.with(|s| check(&s.borrow())))
        .unwrap_or(false)
}

pub fn set_extern_sigs(sigs: HashMap<String, Vec<Type>>) {
    EXTERN_SIGS.with(|s| *s.borrow_mut() = sigs);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Read,
    Move,
}

fn collect_sigs(prog: &Program) -> HashMap<String, Vec<Type>> {
    let mut sigs = EXTERN_SIGS.with(|s| s.borrow().clone());
    for item in &prog.items {
        if let Item::Fn(f) = item {
            sigs.insert(f.name.clone(), f.params.iter().map(|p| p.ty.clone()).collect());
        }
    }
    sigs
}

pub fn check_borrows(prog: &Program, infer: &InferOutput) -> DiagResult<()> {
    let structs = collect_structs(prog);
    let sigs = collect_sigs(prog);
    let mut diags = Vec::new();
    for item in &prog.items {
        let Item::Fn(f) = item else { continue };
        let entries = infer
            .expr_types_by_fn
            .iter()
            .find(|(fd, _)| fd.name == f.name && fd.span == f.span)
            .or_else(|| infer.expr_types_by_fn.iter().find(|(fd, _)| fd.name == f.name))
            .map(|(_, e)| e.as_slice())
            .unwrap_or(&[]);
        diags.extend(check_fn(f, entries, &structs, &sigs));
    }
    if diags.is_empty() {
        Ok(())
    } else {
        diags.sort_by_key(|d| (d.span.start, d.span.col));
        Err(diags)
    }
}

fn collect_structs(prog: &Program) -> HashMap<String, StructDef> {
    let enums = prog.items.iter().filter_map(|i| if let Item::Enum(e) = i { Some(e.clone()) } else { None }).collect();
    LOCAL_ENUMS.with(|s| *s.borrow_mut() = enums);
    let mut structs = HashMap::new();
    for item in &prog.items {
        if let Item::Struct(s) = item {
            structs.insert(s.name.clone(), s.clone());
        } else if let Item::Frame(f) = item {
            structs.insert(f.name.clone(), crate::infer::frame_as_struct(f));
        }
    }
    structs
}

thread_local! {
    /// The moved locals of each fn of the last checked program (`publish_moved_locals`).
    static PUBLISHED: std::cell::RefCell<Option<HashMap<String, HashSet<String>>>> = const { std::cell::RefCell::new(None) };
}

/// Publishes the moved locals of `prog` for the drops pass that lowers it next.
pub fn publish_moved_locals(prog: &Program, infer: &InferOutput) {
    let moved = moved_locals(prog, infer);
    PUBLISHED.with(|p| *p.borrow_mut() = Some(moved));
}

/// The locals `fn_name` moves, as published; `None` when no program was checked.
pub fn published_moved_locals(fn_name: &str) -> Option<HashSet<String>> {
    PUBLISHED.with(|p| p.borrow().as_ref().and_then(|m| m.get(fn_name).cloned()))
}

/// The locals each function moves (SPEC «Memoria»): the root of every moved place, per fn name.
/// `elaborate_drops` releases an owned local only when it is not in this set. Not every move is
/// flow-sensitive yet: a local moved on one path counts as moved on all of them.
pub fn moved_locals(prog: &Program, infer: &InferOutput) -> HashMap<String, HashSet<String>> {
    let structs = collect_structs(prog);
    let sigs = collect_sigs(prog);
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    for item in &prog.items {
        let Item::Fn(f) = item else { continue };
        let entries = infer
            .expr_types_by_fn
            .iter()
            .find(|(fd, _)| fd.name == f.name && fd.span == f.span)
            .or_else(|| infer.expr_types_by_fn.iter().find(|(fd, _)| fd.name == f.name))
            .map(|(_, e)| e.as_slice())
            .unwrap_or(&[]);
        let (_, roots) = check_fn_with_moves_policy(f, entries, &structs, &sigs, true);
        out.entry(f.name.clone()).or_default().extend(roots);
    }
    out
}

fn check_fn(
    f: &FnDef,
    entries: &[ExprTypeEntry],
    structs: &HashMap<String, StructDef>,
    sigs: &HashMap<String, Vec<Type>>,
) -> Vec<Diagnostic> {
    check_fn_with_moves_policy(f, entries, structs, sigs, false).0
}

/// `publish_moves`: for the drops pass a local of unknown type counts as owned, so a move of it
/// is recorded (a missed move would be a double release; a recorded one is only a leak).
fn check_fn_with_moves_policy(
    f: &FnDef,
    entries: &[ExprTypeEntry],
    structs: &HashMap<String, StructDef>,
    sigs: &HashMap<String, Vec<Type>>,
    publish_moves: bool,
) -> (Vec<Diagnostic>, HashSet<String>) {
    let cx = run_fn(f, entries, structs, sigs, publish_moves);
    let roots = cx
        .events
        .iter()
        .filter(|m| m.kind == EventKind::Move)
        .map(|m| m.place.split('.').next().unwrap_or("").to_string())
        .collect();
    (cx.diags, roots)
}

fn run_fn(
    f: &FnDef,
    entries: &[ExprTypeEntry],
    structs: &HashMap<String, StructDef>,
    sigs: &HashMap<String, Vec<Type>>,
    publish_moves: bool,
) -> Cx {
    let mut types = HashMap::new();
    for e in entries {
        types.insert((e.span_start, e.span_end), e.ty.clone());
    }
    let mut cx = Cx {
        publish_moves,
        types,
        structs: structs.clone(),
        sigs: sigs.clone(),
        ret_refs: false,
        scopes: vec![Scope { vars: HashMap::new(), end: u32::MAX }],
        loans: Vec::new(),
        events: Vec::new(),
        diags: Vec::new(),
        whiles: Vec::new(),
        groups: Vec::new(),
        diverge: Vec::new(),
    };
    cx.ret_refs = cx.ref_carrying(&f.ret);
    for p in &f.params {
        cx.bind(&p.name, p.ty.clone(), is_ref(&p.ty), Vec::new());
    }
    cx.walk_block(&f.body, true);
    cx.finish();
    cx
}

/// A struct or variant constructor (`P(a, b)`, `Some(x)`): its arguments move into the value.
/// Names of another module are qualified (`mod__P`): the last segment decides.
fn is_ctor_name(n: &str, structs: &HashMap<String, StructDef>) -> bool {
    if structs.contains_key(n) {
        return true;
    }
    let last = n.rsplit("__").next().unwrap_or(n);
    last.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

fn is_ref(ty: &Type) -> bool {
    matches!(ty, Type::Ref { .. })
}

impl Cx {
    fn ref_of(&self, e: &Expr) -> Option<(String, bool)> {
        ref_of(e)
    }

    fn bind(&mut self, name: &str, ty: Type, param_ref: bool, loans: Vec<usize>) {
        if let Some(scope) = self.scopes.last_mut() {
            let scope_end = scope.end;
            scope.vars.insert(
                name.to_string(),
                Binding { ty, param_ref, loans, scope_end, from_ref: false },
            );
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.vars.get(name))
    }

    fn lookup_depth(&self, name: &str) -> Option<usize> {
        self.scopes.iter().rposition(|s| s.vars.contains_key(name))
    }

    fn root_binding(&self, place: &str) -> Option<&Binding> {
        self.lookup(place.split('.').next().unwrap_or(place))
    }

    fn push(&mut self, end: u32) {
        self.scopes.push(Scope { vars: HashMap::new(), end });
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn ty_of(&self, e: &Expr) -> Option<Type> {
        if let Expr::Ident { name, .. } = e {
            if let Some(b) = self.lookup(name) {
                if !is_unknown(&b.ty) {
                    return Some(b.ty.clone());
                }
            }
        }
        let sp = e.span();
        self.types.get(&(sp.start, sp.end)).cloned()
    }

    fn copy_ty(&self, ty: &Type) -> bool {
        if self.publish_moves && is_unknown(ty) {
            return false;
        }
        is_copy(ty, &self.structs)
    }

    fn bound_ty(&self, anno: &Option<Type>, init: &Expr, span: Span) -> Type {
        let sp = init.span();
        anno.clone()
            .or_else(|| self.types.get(&(sp.start, sp.end)).cloned())
            .or_else(|| self.ty_of(init))
            .unwrap_or_else(|| named("Unknown", span))
    }

    /// A type that is a reference or stores one: a value of it keeps its loans alive.
    fn ref_carrying(&self, ty: &Type) -> bool {
        fn rec(ty: &Type, structs: &HashMap<String, StructDef>, seen: &mut Vec<String>) -> bool {
            match ty {
                Type::Ref { .. } => true,
                Type::Named { name, args, .. } | Type::Qualified { name, args, .. } => {
                    if args.iter().any(|a| rec(a, structs, seen)) {
                        return true;
                    }
                    if seen.iter().any(|s| s == name) {
                        return false;
                    }
                    let Some(def) = structs.get(name) else { return false };
                    if !def.lifetimes.is_empty() {
                        return true;
                    }
                    seen.push(name.clone());
                    let r = def.fields.iter().any(|f| rec(&f.ty, structs, seen));
                    seen.pop();
                    r
                }
                Type::Tensor { .. } | Type::Fn { .. } => false,
            }
        }
        rec(ty, &self.structs, &mut Vec::new())
    }

    /// A use of `name` keeps every loan it holds alive up to `end`.
    fn touch(&mut self, name: &str, end: u32) {
        let ids = self.lookup(name).map(|b| b.loans.clone()).unwrap_or_default();
        for id in ids {
            if let Some(l) = self.loans.get_mut(id) {
                l.end = l.end.max(end);
                l.uses.push(end);
            }
        }
    }

    fn param_kind(&self, fname: Option<&str>, i: usize) -> ParamKind {
        let Some(n) = fname else { return ParamKind::Unknown };
        if let Some(sig) = self.sigs.get(n) {
            return match sig.get(i) {
                Some(Type::Ref { mutable: true, .. }) => ParamKind::Mut,
                Some(Type::Ref { .. }) => ParamKind::Shared,
                Some(_) => ParamKind::Value,
                None => ParamKind::Unknown,
            };
        }
        if i == 0 && crate::ownership::mutates_container(n) {
            return ParamKind::Mut;
        }
        if i == 0 && matches!(n, "list_get" | "dict_get") {
            return ParamKind::Shared;
        }
        ParamKind::Unknown
    }

    /// The places `e` borrows from when its value is (or stores) a reference.
    fn sources(&self, e: &Expr) -> Src {
        match e {
            Expr::Unary { op: op @ (UnOp::Ref | UnOp::RefMut), expr, .. } => match place_of(expr) {
                // A place behind a reference (`&c.xs` with `c: &mut C`) is a reborrow: it lends
                // that place of the referent and keeps the loans the reference holds.
                Some(p) => match self.root_binding(&p) {
                    Some(b) if b.param_ref || is_ref(&b.ty) => Src {
                        new: vec![(p, matches!(op, UnOp::RefMut), e.span())],
                        shared: b.loans.clone(),
                    },
                    Some(_) => Src {
                        new: vec![(p, matches!(op, UnOp::RefMut), e.span())],
                        shared: Vec::new(),
                    },
                    None => Src::default(),
                },
                None => self.sources(expr),
            },
            Expr::Ident { name, .. } => Src {
                new: Vec::new(),
                shared: self.lookup(name).map(|b| b.loans.clone()).unwrap_or_default(),
            },
            Expr::Field { base, .. } => {
                if self.ty_of(e).is_some_and(|t| !self.ref_carrying(&t)) {
                    Src::default()
                } else {
                    self.sources(base)
                }
            }
            Expr::Call { func, args, .. } => {
                if !self.ty_of(e).is_some_and(|t| self.ref_carrying(&t)) {
                    return Src::default();
                }
                let fname = match func.as_ref() {
                    Expr::Ident { name, .. } => Some(name.as_str()),
                    _ => None,
                };
                let mut out = Src::default();
                for (i, a) in args.iter().enumerate() {
                    let pty = fname.and_then(|n| self.sigs.get(n)).and_then(|s| s.get(i));
                    if pty.is_some_and(|pt| !self.ref_carrying(pt)) {
                        continue;
                    }
                    out.merge(self.sources(a));
                    let k = self.param_kind(fname, i);
                    if matches!(k, ParamKind::Shared | ParamKind::Mut) {
                        if let Some(p) = place_of(a) {
                            if self.root_binding(&p).is_some_and(|b| !b.param_ref && !is_ref(&b.ty)) {
                                out.new.push((p, k == ParamKind::Mut, a.span()));
                            }
                        }
                    }
                }
                out
            }
            Expr::If { then_block, elsifs, else_block, .. } => {
                let mut out = Src::default();
                if let Some(t) = &then_block.tail {
                    out.merge(self.sources(t));
                }
                for arm in elsifs {
                    if let Some(t) = &arm.body.tail {
                        out.merge(self.sources(t));
                    }
                }
                if let Some(b) = else_block {
                    if let Some(t) = &b.tail {
                        out.merge(self.sources(t));
                    }
                }
                out
            }
            Expr::Match { arms, .. } => {
                let mut out = Src::default();
                for arm in arms {
                    out.merge(self.sources(&arm.body));
                }
                out
            }
            Expr::Block(b) | Expr::On { body: b, .. } => {
                b.tail.as_ref().map(|t| self.sources(t)).unwrap_or_default()
            }
            Expr::To { expr, .. } | Expr::Try { expr, .. } => self.sources(expr),
            _ => Src::default(),
        }
    }

    /// Creates the loans of `src`, live at least up to `end`, and returns every loan id it names.
    fn mint(&mut self, src: Src, end: u32) -> Vec<usize> {
        let mut ids = src.shared;
        for (place, mutable, span) in src.new {
            let referent_end = self
                .root_binding(&place)
                .filter(|b| !b.param_ref && !is_ref(&b.ty))
                .map(|b| b.scope_end)
                .unwrap_or(u32::MAX);
            self.loans.push(Loan {
                place,
                mutable,
                start: span.start,
                end: end.max(span.end),
                span,
                site: (span.start, span.end),
                referent_end,
                uses: vec![end.max(span.end)],
            });
            ids.push(self.loans.len() - 1);
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// `name = value`: a new local, or an assignment to one in scope. An assignment in the scope
    /// that declared the name replaces its loans; one in an inner scope (a branch, a loop body)
    /// adds to them, since the outer value may still be the old one.
    fn bind_value(&mut self, name: &str, anno: &Option<Type>, value: &Expr, span: Span, limit: u32) {
        self.walk_expr(value, Use::Move, limit);
        let src = self.sources(value);
        let ids = self.mint(src, value.span().end);
        let ty = self.bound_ty(anno, value, span);
        let cur = self.scopes.len() - 1;
        match self.lookup_depth(name) {
            Some(d) => {
                if let Some(b) = self.scopes[d].vars.get_mut(name) {
                    if d == cur {
                        if !ids.is_empty() {
                            b.param_ref = false;
                        }
                        b.loans = ids;
                        if !is_unknown(&ty) {
                            b.ty = ty;
                        }
                    } else {
                        b.loans.extend(ids);
                    }
                }
            }
            None => {
                self.bind(name, ty, false, ids);
            }
        }
    }

    /// Adds `ids` to the loans the root of `place` holds (a reference stored into it).
    fn attach(&mut self, place: &str, ids: Vec<usize>) {
        if ids.is_empty() {
            return;
        }
        let root = place.split('.').next().unwrap_or(place).to_string();
        if let Some(d) = self.lookup_depth(&root) {
            if let Some(b) = self.scopes[d].vars.get_mut(&root) {
                b.loans.extend(ids);
            }
        }
    }

    /// Whether writing through `e` goes through a shared reference.
    fn behind_shared(&self, e: &Expr) -> bool {
        let shared = |t: Option<Type>| matches!(t, Some(Type::Ref { mutable: false, .. }));
        match e {
            Expr::Ident { .. } => shared(self.ty_of(e)),
            Expr::Field { base, .. } => shared(self.ty_of(base)) || self.behind_shared(base),
            Expr::Unary { op: UnOp::Deref, expr, .. } => shared(self.ty_of(expr)) || self.behind_shared(expr),
            _ => false,
        }
    }

    /// Whether `e` is reached through a reference (`r.f` with `r: &T`, or deeper).
    fn behind_ref(&self, e: &Expr) -> bool {
        let is = |t: Option<Type>| matches!(t, Some(Type::Ref { .. }));
        match e {
            Expr::Ident { name, .. } => is(self.ty_of(e)) || self.lookup(name).is_some_and(|b| b.from_ref),
            Expr::Field { base, .. } => is(self.ty_of(e)) || self.behind_ref(base),
            Expr::Unary { op: UnOp::Ref | UnOp::RefMut, .. } => true,
            _ => false,
        }
    }

    fn event(&mut self, place: String, span: Span, kind: EventKind) {
        self.events.push(Event { place, span, kind });
    }

    fn walk_block(&mut self, b: &Block, fn_tail: bool) {
        self.push(b.span.end);
        let limit = b.span.end;
        for st in &b.stmts {
            if let Stmt::Return { span, .. } = st {
                self.diverge.push((b.span, span.start));
            }
            self.walk_stmt(st);
        }
        if let Some(t) = &b.tail {
            self.walk_expr(t, Use::Move, limit);
            if fn_tail {
                self.returned(t);
            }
        }
        self.pop();
    }

    fn walk_stmt(&mut self, st: &Stmt) {
        let limit = stmt_end(st);
        match st {
            Stmt::Let { name, init, ty, span, .. } => {
                self.bind_value(name, ty, init, *span, limit);
            }
            Stmt::Assign { target, value, span } => {
                if let Expr::Ident { name, .. } = target {
                    self.bind_value(name, &None, value, value.span(), limit);
                    self.event(name.clone(), *span, EventKind::Mutation);
                    return;
                }
                self.walk_expr(value, Use::Move, limit);
                let src = self.sources(value);
                let ids = self.mint(src, value.span().end);
                if self.behind_shared(target) {
                    self.diags.push(Diagnostic::new(
                        ErrorCode::EBorrow,
                        "cannot assign through a `&` reference: it must be `&mut`",
                        *span,
                    ));
                }
                if let Some(place) = place_of(target) {
                    self.touch_root(&place, span.end);
                    self.attach(&place, ids);
                    self.event(place, *span, EventKind::Mutation);
                } else {
                    if let Expr::Unary { op: UnOp::Deref, expr, .. } = target {
                        if let Some(place) = place_of(expr) {
                            self.attach(&place, ids);
                        }
                    }
                    self.walk_expr(target, Use::Read, limit);
                }
            }
            Stmt::Expr(e) => self.walk_expr(e, Use::Read, limit),
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v, Use::Move, limit);
                    self.returned(v);
                }
            }
            Stmt::Break { .. } => {}
            Stmt::While { cond, body, span } => {
                self.whiles.push(*span);
                self.walk_expr(cond, Use::Read, span.end);
                self.walk_block(body, false);
            }
        }
    }

    fn touch_root(&mut self, place: &str, end: u32) {
        let root = place.split('.').next().unwrap_or(place).to_string();
        self.touch(&root, end);
    }

    fn temp_loan(&mut self, e: &Expr, end: u32) {
        let Some((place, mutable)) = self.ref_of(e) else { return };
        let sp = e.span();
        // A reborrow through a reference (`&r.f`, `&mut self.xs`) borrows the reference's own
        // referent, which this fn does not own: the loan the reference holds already covers it.
        let owned_root = self.root_binding(&place).is_some_and(|b| !b.param_ref && !is_ref(&b.ty));
        if self.root_binding(&place).is_some() {
            // Behind a reference the referent is not a local of this fn: no scope to outlive.
            let referent_end = if owned_root {
                self.root_binding(&place).map(|b| b.scope_end).unwrap_or(u32::MAX)
            } else {
                u32::MAX
            };
            self.loans.push(Loan {
                place: place.clone(),
                mutable,
                start: sp.start,
                end: end.max(sp.end),
                span: sp,
                site: (sp.start, sp.end),
                referent_end,
                uses: vec![end.max(sp.end)],
            });
        }
        if !owned_root && mutable {
            if let Expr::Unary { expr, .. } = e {
                if self.behind_shared(expr) || matches!(self.ty_of_root(&place), Some(Type::Ref { mutable: false, .. })) {
                    self.diags.push(Diagnostic::new(
                        ErrorCode::EBorrow,
                        format!("cannot borrow `{place}` as `&mut` through a `&` reference"),
                        sp,
                    ));
                }
            }
        }
        self.touch_root(&place, sp.end);
    }

    fn ty_of_root(&self, place: &str) -> Option<Type> {
        self.root_binding(place).map(|b| b.ty.clone())
    }

    fn walk_expr(&mut self, e: &Expr, use_kind: Use, limit: u32) {
        match e {
            Expr::Ident { name, span } => {
                self.touch(name, span.end);
                if self.lookup(name).is_none() {
                    return;
                }
                let moves = use_kind == Use::Move
                    && self.lookup(name).is_some_and(|b| !self.copy_ty(&b.ty));
                if use_kind == Use::Move && !self.publish_moves && self.lookup(name).is_some_and(|b| b.from_ref) {
                    let owned = self
                        .types
                        .get(&(span.start, span.end))
                        .is_some_and(|t| !is_unknown(t) && !is_ref(t) && !self.copy_ty(t));
                    if owned {
                        self.diags.push(Diagnostic::new(
                            ErrorCode::EBorrow,
                            format!("cannot move `{name}` out of a reference: use `.clone()`"),
                            *span,
                        ));
                    }
                }
                let kind = if moves { EventKind::Move } else { EventKind::Read };
                self.event(name.clone(), *span, kind);
            }
            Expr::Field { base, span, .. } => {
                if let Some(place) = place_of(e) {
                    self.touch_root(&place, span.end);
                    if self.root_binding(&place).is_none() {
                        return;
                    }
                    let moves = use_kind == Use::Move && self.ty_of(e).is_none_or(|t| !self.copy_ty(&t));
                    // SPEC «Memoria»: a non-`Copy` value does not move out of a reference.
                    let known_owned = self.ty_of(e).is_some_and(|t| !is_unknown(&t) && !is_ref(&t) && !self.copy_ty(&t));
                    if moves && known_owned && !self.publish_moves && self.behind_ref(base) {
                        self.diags.push(Diagnostic::new(
                            ErrorCode::EBorrow,
                            format!("cannot move `{place}` out of a reference: use `.clone()`"),
                            *span,
                        ));
                    }
                    let kind = if moves { EventKind::Move } else { EventKind::Read };
                    self.event(place, *span, kind);
                } else {
                    self.walk_expr(base, Use::Read, limit);
                }
            }
            Expr::Unary { op, expr, span } => match op {
                UnOp::Ref | UnOp::RefMut => {
                    if self.ref_of(e).is_some() {
                        self.temp_loan(e, span.end);
                    } else {
                        self.walk_expr(expr, Use::Read, limit);
                    }
                }
                UnOp::Deref => {
                    if self.deref_moves_out(expr, e) {
                        self.diags.push(Diagnostic::new(
                            ErrorCode::EBorrow,
                            "cannot move a non-Copy value out of a reference",
                            *span,
                        ));
                    }
                    self.walk_expr(expr, Use::Read, limit);
                }
                UnOp::Neg | UnOp::Not => self.walk_expr(expr, Use::Read, limit),
            },
            Expr::Call { func, args, span, .. } => {
                let fname = match func.as_ref() {
                    Expr::Ident { name, .. } => Some(name.clone()),
                    _ => None,
                };
                // A temporary `&` passed as an argument lives until the call returns.
                let call_end = span.end;
                if fname.is_none() {
                    self.walk_expr(func, Use::Read, call_end);
                }
                let mut receiver: Option<(String, Span, ParamKind)> = None;
                for (i, a) in args.iter().enumerate() {
                    let kind = self.param_kind(fname.as_deref(), i);
                    if let Some((_, mutable)) = self.ref_of(a) {
                        if kind == ParamKind::Mut && !mutable {
                            self.diags.push(Diagnostic::new(
                                ErrorCode::EBorrow,
                                "this argument is mutated: it must be `&mut`",
                                a.span(),
                            ));
                        }
                        self.temp_loan(a, call_end);
                    } else if fname
                        .as_deref()
                        .is_some_and(|n| crate::ownership::operand_is_moved(n, i, args.len()) || is_ctor_name(n, &self.structs))
                    {
                        self.walk_expr(a, Use::Move, call_end);
                    } else if kind == ParamKind::Mut && place_of(a).is_some() {
                        // Autoref (`xs.push(v)`, `c.add(1)`): the exclusive loan starts when the
                        // call does, after the other arguments are evaluated.
                        let place = place_of(a).unwrap_or_default();
                        self.touch_root(&place, a.span().end);
                        if self.behind_shared(a) || matches!(self.ty_of(a), Some(Type::Ref { mutable: false, .. })) {
                            self.diags.push(Diagnostic::new(
                                ErrorCode::EBorrow,
                                format!("cannot mutate `{place}` through a `&` reference: it must be `&mut`"),
                                a.span(),
                            ));
                        }
                        receiver = Some((place, a.span(), kind));
                    } else {
                        self.walk_expr(a, Use::Read, call_end);
                    }
                }
                if let Some((place, sp, _)) = receiver {
                    let at = Span { start: call_end, end: call_end, ..sp };
                    self.event(place, at, EventKind::Mutation);
                }
                // A reference stored into a container makes the container hold its loans.
                if let Some(n) = fname.as_deref() {
                    let stored = match n {
                        "list_push" => args.get(1),
                        "list_set" | "dict_put" => args.get(2),
                        _ => None,
                    };
                    if let (Some(v), Some(dst)) = (stored, args.first()) {
                        let src = self.sources(v);
                        let ids = self.mint(src, call_end);
                        let target = match dst {
                            Expr::Unary { op: UnOp::Ref | UnOp::RefMut, expr, .. } => place_of(expr),
                            _ => place_of(dst),
                        };
                        if let Some(place) = target {
                            self.attach(&place, ids);
                        }
                    }
                }
            }
            Expr::Binary { left, right, .. } => {
                self.walk_expr(left, Use::Read, limit);
                self.walk_expr(right, Use::Read, limit);
            }
            Expr::If { cond, then_block, elsifs, else_block, .. } => {
                self.walk_expr(cond, Use::Read, limit);
                let mut group = vec![then_block.span];
                group.extend(elsifs.iter().map(|a| a.body.span));
                group.extend(else_block.iter().map(|b| b.span));
                self.groups.push(group);
                self.walk_block(then_block, false);
                for arm in elsifs {
                    self.walk_expr(&arm.cond, Use::Read, limit);
                    self.walk_block(&arm.body, false);
                }
                if let Some(b) = else_block {
                    self.walk_block(b, false);
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                // SPEC «Memoria»: a pattern that binds a non-`Copy` payload of an owned scrutinee
                // moves it out (a partial move of the scrutinee). Through `&`, it borrows.
                let owned = matches!(scrutinee.as_ref(), Expr::Ident { .. } | Expr::Field { .. })
                    && !self.behind_ref(scrutinee);
                let sty = self.ty_of(scrutinee);
                let by_ref = self.behind_ref(scrutinee) || matches!(sty, Some(Type::Ref { .. }));
                let moves = owned
                    && !self.publish_moves
                    && sty.as_ref().is_some_and(|t| {
                        !is_unknown(t) && !self.copy_ty(t) && match_moves(t, arms, &self.structs)
                    });
                self.walk_expr(scrutinee, if moves { Use::Move } else { Use::Read }, limit);
                let held = self.sources(scrutinee).shared;
                self.groups.push(arms.iter().map(|a| a.body.span()).collect());
                for arm in arms {
                    self.push(arm.body.span().end.max(limit));
                    let mut names = Vec::new();
                    pattern_names(&arm.pattern, &mut names);
                    for n in names {
                        self.bind(&n, named("Unknown", arm.span), false, held.clone());
                        if by_ref && !is_variant_name(&n) {
                            if let Some(b) = self.scopes.last_mut().and_then(|s| s.vars.get_mut(&n)) {
                                b.from_ref = true;
                            }
                        }
                    }
                    self.walk_expr(&arm.body, use_kind, limit);
                    self.pop();
                }
            }
            Expr::Block(b) | Expr::On { body: b, .. } => self.walk_block(b, false),
            Expr::To { expr, .. } | Expr::Try { expr, .. } => self.walk_expr(expr, Use::Move, limit),
            Expr::Lambda { body, .. } => self.walk_expr(body, Use::Read, limit),
            Expr::TensorLit { rows, .. } => {
                for row in rows {
                    for c in row {
                        self.walk_expr(c, Use::Read, limit);
                    }
                }
            }
            Expr::String { parts, span, .. } => {
                for part in parts {
                    if let StringPart::Interp(name) = part {
                        self.touch(name, span.end);
                        if self.lookup(name).is_some() {
                            self.event(name.clone(), *span, EventKind::Read);
                        }
                    }
                }
            }
            Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } => {}
        }
    }

    fn deref_moves_out(&self, operand: &Expr, deref_expr: &Expr) -> bool {
        let sp = deref_expr.span();
        if let Some(ty) = self.types.get(&(sp.start, sp.end)) {
            return !self.copy_ty(ty);
        }
        match self.ty_of(operand) {
            Some(Type::Ref { inner, .. }) => !self.copy_ty(&inner),
            _ => false,
        }
    }

    /// A returned reference (or a value that stores one) must come from a reference parameter:
    /// every loan this fn created refers to one of its own locals.
    fn returned(&mut self, e: &Expr) {
        if !self.ret_refs {
            return;
        }
        let src = self.sources(e);
        let own_new = src.new.iter().any(|(p, _, _)| {
            self.root_binding(p).is_some_and(|b| !b.param_ref && !is_ref(&b.ty))
        });
        let own_shared = src.shared.iter().any(|id| {
            self.loans.get(*id).is_some_and(|l| l.referent_end != u32::MAX)
        });
        if own_new || own_shared {
            self.diags.push(Diagnostic::new(
                ErrorCode::EBorrow,
                "returning a reference to a local",
                e.span(),
            ));
        }
    }

    /// Whether control at `at` can get to `u` (`u > at`), for a loan that starts at `start`.
    /// Sibling branches do not reach each other, and nothing follows a block's `return`; a loop
    /// that began after the loan brings the next iteration back, so there it is reachable.
    fn reachable(&self, at: u32, u: u32, start: u32) -> bool {
        let within = |sp: &Span, x: u32| x >= sp.start && x <= sp.end;
        for (block, ret) in &self.diverge {
            if within(block, at) && at <= *ret && u > block.end {
                return false;
            }
        }
        for g in &self.groups {
            let bi = g.iter().position(|sp| within(sp, at));
            let bj = g.iter().position(|sp| within(sp, u));
            if let (Some(i), Some(j)) = (bi, bj) {
                if i != j {
                    let looped = self
                        .whiles
                        .iter()
                        .any(|w| w.start > start && w.start <= g[i].start && g[i].end <= w.end);
                    if !looped {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn live_after(&self, loan: &Loan, at: u32) -> bool {
        at >= loan.start && loan.uses.iter().any(|u| *u > at && self.reachable(at, *u, loan.start))
    }

    fn finish(&mut self) {
        for loan in &mut self.loans {
            for w in &self.whiles {
                // A reference from before the loop, used inside it, is used again next iteration.
                if loan.start < w.start && loan.uses.iter().any(|u| *u >= w.start && *u <= w.end) {
                    loan.uses.push(w.end);
                    loan.end = loan.end.max(w.end);
                }
            }
        }
        let loans = self.loans.clone();
        let mut diags = Vec::new();
        for loan in &loans {
            if loan.uses.iter().any(|u| *u > loan.referent_end) {
                diags.push(Diagnostic::new(
                    ErrorCode::EBorrow,
                    format!("`{}` does not live long enough: a reference to it is used after its scope", loan.place),
                    loan.span,
                ));
            }
        }
        for (i, x) in loans.iter().enumerate() {
            for y in loans.iter().skip(i + 1) {
                if x.site == y.site || !overlaps(&x.place, &y.place) || !(x.mutable || y.mutable) {
                    continue;
                }
                let (a, b) = if x.start <= y.start { (x, y) } else { (y, x) };
                if self.live_after(a, b.start) {
                    diags.push(Diagnostic::new(
                        ErrorCode::EBorrow,
                        format!("`{}` is already borrowed (by the `&` of line {})", shortest(&a.place, &b.place), a.span.line),
                        b.span,
                    ));
                }
            }
        }
        for m in &self.events {
            let at = m.span.start;
            for loan in &loans {
                if !overlaps(&m.place, &loan.place) {
                    continue;
                }
                // The expression that creates the loan reads its own place.
                if at >= loan.site.0 && at <= loan.site.1 {
                    continue;
                }
                if m.kind == EventKind::Read && !loan.mutable {
                    continue;
                }
                if self.live_after(loan, at) {
                    diags.push(Diagnostic::new(
                        ErrorCode::EBorrow,
                        format!("`{}` is already borrowed (by the `&` of line {})", m.place, loan.span.line),
                        m.span,
                    ));
                    break;
                }
            }
        }
        self.diags.extend(diags);
        self.diags.sort_by_key(|d| (d.span.start, d.span.end));
        self.diags.dedup_by_key(|d| (d.span.start, d.span.end));
    }
}

fn is_unknown(ty: &Type) -> bool {
    matches!(ty, Type::Named { name, .. } if name == "Unknown")
}

fn pattern_names(p: &Pattern, out: &mut Vec<String>) {
    match p {
        Pattern::Ident(n, _) => out.push(n.clone()),
        Pattern::Variant { args, .. } => {
            for a in args {
                pattern_names(a, out);
            }
        }
        Pattern::Wild(_) | Pattern::Int(..) | Pattern::Str(..) | Pattern::Regex(..) => {}
    }
}

fn stmt_end(st: &Stmt) -> u32 {
    match st {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Break { span }
        | Stmt::While { span, .. } => span.end,
        Stmt::Expr(e) => e.span().end,
    }
}

fn ref_of(e: &Expr) -> Option<(String, bool)> {
    match e {
        Expr::Unary { op: UnOp::Ref, expr, .. } => place_of(expr).map(|p| (p, false)),
        Expr::Unary { op: UnOp::RefMut, expr, .. } => place_of(expr).map(|p| (p, true)),
        _ => None,
    }
}

fn place_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident { name, .. } => Some(name.clone()),
        Expr::Field { base, field, .. } => place_of(base).map(|p| format!("{p}.{field}")),
        _ => None,
    }
}

fn shortest<'b>(a: &'b str, b: &'b str) -> &'b str {
    if a.len() <= b.len() { a } else { b }
}

fn overlaps(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}.")) || b.starts_with(&format!("{a}."))
}

fn ranges_overlap(a0: u32, a1: u32, b0: u32, b1: u32) -> bool {
    a0 < b1 && b0 < a1
}

fn named(name: &str, span: Span) -> Type {
    Type::Named {
        name: name.to_string(),
        args: vec![],
        span,
    }
}

fn is_copy(ty: &Type, structs: &HashMap<String, StructDef>) -> bool {
    fn rec(ty: &Type, structs: &HashMap<String, StructDef>, seen: &mut Vec<String>) -> bool {
        match ty {
            Type::Named { name, .. } | Type::Qualified { name, .. } => {
                if matches!(name.as_str(), "Int" | "Float" | "Bool" | "Unit" | "Unknown") {
                    return true;
                }
                if seen.iter().any(|s| s == name) {
                    return false;
                }
                seen.push(name.clone());
                let ok = structs.get(name).is_some_and(|def| {
                    def.fields.iter().all(|f| rec(&f.ty, structs, seen))
                });
                seen.pop();
                ok
            }
            Type::Tensor { .. } | Type::Fn { .. } => false,
            Type::Ref { mutable, .. } => !mutable,
        }
    }
    rec(ty, structs, &mut Vec::new())
}
