//! Borrow checker (`check_borrows`). Moves stay in `ownership.rs` (`E_MOVED`).
//! This pass adds `E_BORROW`: a loan that overlaps a mutation or another
//! exclusive loan, a reference that outlives its referent, and moving a
//! non-`Copy` value out of a reference (`*r`).

use std::collections::HashMap;

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
}

struct MoveOf {
    place: String,
    span: Span,
}

#[derive(Clone)]
enum Origin {
    Owned,
    /// Parameter whose type is already `&T` or `&mut T`.
    ParamRef,
    /// `r = &place` or `r = &mut place`.
    Borrow { place: String },
}

#[derive(Clone)]
struct Binding {
    ty: Type,
    origin: Origin,
    /// `span.start` of the `&` / `&mut` that bound this name, when it is a loan.
    loan_at: Option<u32>,
}

struct Cx {
    types: HashMap<(u32, u32), Type>,
    structs: HashMap<String, StructDef>,
    scopes: Vec<HashMap<String, Binding>>,
    /// End byte of the last use of the loan created at this `&` span start.
    named_end: HashMap<u32, u32>,
    loans: Vec<Loan>,
    moves: Vec<MoveOf>,
    diags: Vec<Diagnostic>,
    whiles: Vec<Span>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Read,
    Move,
}

pub fn check_borrows(prog: &Program, infer: &InferOutput) -> DiagResult<()> {
    let structs = collect_structs(prog);
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
        diags.extend(check_fn(f, entries, &structs));
    }
    if diags.is_empty() {
        Ok(())
    } else {
        diags.sort_by_key(|d| (d.span.start, d.span.col));
        Err(diags)
    }
}

fn collect_structs(prog: &Program) -> HashMap<String, StructDef> {
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

fn check_fn(f: &FnDef, entries: &[ExprTypeEntry], structs: &HashMap<String, StructDef>) -> Vec<Diagnostic> {
    let mut types = HashMap::new();
    for e in entries {
        types.insert((e.span_start, e.span_end), e.ty.clone());
    }
    let mut cx = Cx {
        types,
        structs: structs.clone(),
        scopes: vec![HashMap::new()],
        named_end: HashMap::new(),
        loans: Vec::new(),
        moves: Vec::new(),
        diags: Vec::new(),
        whiles: Vec::new(),
    };
    for p in &f.params {
        let origin = if is_ref(&p.ty) { Origin::ParamRef } else { Origin::Owned };
        cx.bind(&p.name, p.ty.clone(), origin, None);
    }
    cx.scan_block(&f.body);
    cx.scopes.truncate(1);
    cx.walk_block(&f.body, true);
    cx.finish();
    cx.diags
}

fn is_ref(ty: &Type) -> bool {
    matches!(ty, Type::Ref { .. })
}

impl Cx {
    fn bind(&mut self, name: &str, ty: Type, origin: Origin, loan_at: Option<u32>) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(
                name.to_string(),
                Binding { ty, origin, loan_at },
            );
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn ty_of(&self, e: &Expr) -> Option<Type> {
        let sp = e.span();
        if let Some(t) = self.types.get(&(sp.start, sp.end)) {
            return Some(t.clone());
        }
        if let Expr::Ident { name, .. } = e {
            return self.lookup(name).map(|b| b.ty.clone());
        }
        None
    }

    fn copy_ty(&self, ty: &Type) -> bool {
        is_copy(ty, &self.structs)
    }

    fn bound_ty(&self, anno: &Option<Type>, init: &Expr, span: Span) -> Type {
        anno.clone()
            .or_else(|| self.ty_of(init))
            .unwrap_or_else(|| named("Unknown", span))
    }

    fn note_use(&mut self, name: &str, end: u32) {
        let at = self.lookup(name).and_then(|b| b.loan_at);
        if let Some(at) = at {
            self.named_end
                .entry(at)
                .and_modify(|e| *e = (*e).max(end))
                .or_insert(end);
        }
    }

    fn scan_block(&mut self, b: &Block) {
        self.push();
        for st in &b.stmts {
            self.scan_stmt(st);
        }
        if let Some(t) = &b.tail {
            self.scan_expr(t);
        }
        self.pop();
    }

    fn scan_stmt(&mut self, st: &Stmt) {
        match st {
            Stmt::Let { name, init, ty, span, .. } => {
                self.scan_expr(init);
                if let Some((place, _)) = ref_of(init) {
                    let start = init.span().start;
                    self.named_end.insert(start, init.span().end);
                    let bound = self.bound_ty(ty, init, *span);
                    self.bind(name, bound, Origin::Borrow { place }, Some(start));
                } else {
                    let bound = self.bound_ty(ty, init, *span);
                    self.bind(name, bound, Origin::Owned, None);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.scan_expr(value);
                if let Expr::Ident { name, .. } = target {
                    if let Some((place, _)) = ref_of(value) {
                        let start = value.span().start;
                        self.named_end.insert(start, value.span().end);
                        let bound = self.bound_ty(&None, value, value.span());
                        self.bind(name, bound, Origin::Borrow { place }, Some(start));
                    }
                }
                self.scan_expr(target);
            }
            Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } => self.scan_expr(e),
            Stmt::Return { value: None, .. } | Stmt::Break { .. } => {}
            Stmt::While { cond, body, span } => {
                self.whiles.push(*span);
                self.scan_expr(cond);
                self.scan_block(body);
            }
        }
    }

    fn scan_expr(&mut self, e: &Expr) {
        match e {
            Expr::Ident { name, span } => self.note_use(name, span.end),
            Expr::Call { func, args, .. } => {
                self.scan_expr(func);
                for a in args {
                    self.scan_expr(a);
                }
            }
            Expr::Lambda { body, .. } => self.scan_expr(body),
            Expr::Binary { left, right, .. } => {
                self.scan_expr(left);
                self.scan_expr(right);
            }
            Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } => {
                self.scan_expr(expr)
            }
            Expr::Block(b) | Expr::On { body: b, .. } => self.scan_block(b),
            Expr::TensorLit { rows, .. } => {
                for row in rows {
                    for c in row {
                        self.scan_expr(c);
                    }
                }
            }
            Expr::Field { base, .. } => self.scan_expr(base),
            Expr::Match { scrutinee, arms, .. } => {
                self.scan_expr(scrutinee);
                for arm in arms {
                    self.scan_expr(&arm.body);
                }
            }
            Expr::If { cond, then_block, elsifs, else_block, .. } => {
                self.scan_expr(cond);
                self.scan_block(then_block);
                for arm in elsifs {
                    self.scan_expr(&arm.cond);
                    self.scan_block(&arm.body);
                }
                if let Some(b) = else_block {
                    self.scan_block(b);
                }
            }
            Expr::String { parts, span, .. } => {
                for part in parts {
                    if let StringPart::Interp(name) = part {
                        self.note_use(name, span.end);
                    }
                }
            }
            Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } => {}
        }
    }

    fn walk_block(&mut self, b: &Block, fn_tail: bool) {
        self.push();
        let limit = b.span.end;
        for st in &b.stmts {
            self.walk_stmt(st);
        }
        if let Some(t) = &b.tail {
            if fn_tail {
                self.returned(t);
            }
            self.walk_expr(t, Use::Move, limit);
        }
        self.pop();
    }

    fn walk_stmt(&mut self, st: &Stmt) {
        let limit = stmt_end(st);
        match st {
            Stmt::Let { name, init, ty, span, .. } => {
                if let Some((place, _)) = ref_of(init) {
                    self.borrow_expr(init, true);
                    let bound = self.bound_ty(ty, init, *span);
                    self.bind(name, bound, Origin::Borrow { place }, Some(init.span().start));
                } else {
                    self.walk_expr(init, Use::Move, limit);
                    let bound = self.bound_ty(ty, init, *span);
                    self.bind(name, bound, Origin::Owned, None);
                }
            }
            Stmt::Assign { target, value, span } => {
                if let Expr::Ident { name, .. } = target {
                    if let Some((place, _)) = ref_of(value) {
                        self.borrow_expr(value, true);
                        let bound = self.bound_ty(&None, value, value.span());
                        self.bind(name, bound, Origin::Borrow { place }, Some(value.span().start));
                    } else {
                        self.walk_expr(value, Use::Move, limit);
                        let bound = self.bound_ty(&None, value, value.span());
                        self.bind(name, bound, Origin::Owned, None);
                    }
                } else {
                    self.walk_expr(value, Use::Move, limit);
                }
                if let Some(place) = place_of(target) {
                    self.moves.push(MoveOf { place, span: *span });
                } else {
                    self.walk_expr(target, Use::Read, limit);
                }
            }
            Stmt::Expr(e) => self.walk_expr(e, Use::Read, limit),
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.returned(v);
                    self.walk_expr(v, Use::Move, limit);
                }
            }
            Stmt::Break { .. } => {}
            Stmt::While { cond, body, span } => {
                self.walk_expr(cond, Use::Read, span.end);
                self.walk_block(body, false);
            }
        }
    }

    fn borrow_expr(&mut self, e: &Expr, named: bool) {
        let Some((place, mutable)) = ref_of(e) else { return };
        let sp = e.span();
        let end = if named {
            self.named_end.get(&sp.start).copied().unwrap_or(sp.end)
        } else {
            sp.end
        };
        self.loans.push(Loan {
            place,
            mutable,
            start: sp.start,
            end,
            span: sp,
        });
        if let Expr::Unary { expr, .. } = e {
            self.walk_expr(expr, Use::Read, sp.end);
        }
    }

    fn walk_expr(&mut self, e: &Expr, use_kind: Use, limit: u32) {
        match e {
            Expr::Ident { name, span } => {
                if use_kind == Use::Move {
                    if let Some(b) = self.lookup(name) {
                        if !self.copy_ty(&b.ty) {
                            self.moves.push(MoveOf {
                                place: name.clone(),
                                span: *span,
                            });
                        }
                    }
                }
            }
            Expr::Field { base, span, .. } => {
                if use_kind == Use::Move {
                    if let Some(place) = place_of(e) {
                        if self.ty_of(e).is_none_or(|t| !self.copy_ty(&t)) {
                            self.moves.push(MoveOf { place, span: *span });
                        }
                    }
                }
                self.walk_expr(base, Use::Read, limit);
            }
            Expr::Unary { op, expr, span } => match op {
                UnOp::Ref | UnOp::RefMut => self.borrow_expr(e, false),
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
                self.walk_expr(func, Use::Read, call_end);
                for (i, a) in args.iter().enumerate() {
                    if ref_of(a).is_some() {
                        self.borrow_one(a, call_end);
                    } else if fname
                        .as_deref()
                        .is_some_and(|n| crate::ownership::operand_is_moved(n, i, args.len()))
                    {
                        self.walk_expr(a, Use::Move, call_end);
                    } else {
                        self.walk_expr(a, Use::Read, call_end);
                    }
                }
            }
            Expr::Binary { left, right, .. } => {
                self.walk_expr(left, Use::Read, limit);
                self.walk_expr(right, Use::Read, limit);
            }
            Expr::If { cond, then_block, elsifs, else_block, .. } => {
                self.walk_expr(cond, Use::Read, limit);
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
                self.walk_expr(scrutinee, Use::Read, limit);
                for arm in arms {
                    self.walk_expr(&arm.body, use_kind, limit);
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
            Expr::String { .. } | Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } => {}
        }
    }

    /// Temporary loan of `&place` that lasts until `end` (the enclosing call).
    fn borrow_one(&mut self, e: &Expr, end: u32) {
        let Some((place, mutable)) = ref_of(e) else { return };
        let sp = e.span();
        self.loans.push(Loan {
            place,
            mutable,
            start: sp.start,
            end,
            span: sp,
        });
        if let Expr::Unary { expr, .. } = e {
            self.walk_expr(expr, Use::Read, sp.end);
        }
    }

    fn deref_moves_out(&self, operand: &Expr, deref_expr: &Expr) -> bool {
        if let Some(ty) = self.ty_of(deref_expr) {
            return !self.copy_ty(&ty);
        }
        match self.ty_of(operand) {
            Some(Type::Ref { inner, .. }) => !self.copy_ty(&inner),
            _ => false,
        }
    }

    fn returned(&mut self, e: &Expr) {
        match e {
            Expr::Block(b) => {
                if let Some(t) = &b.tail {
                    self.returned(t);
                }
            }
            Expr::If { then_block, else_block, elsifs, .. } => {
                if let Some(t) = &then_block.tail {
                    self.returned(t);
                }
                for arm in elsifs {
                    if let Some(t) = &arm.body.tail {
                        self.returned(t);
                    }
                }
                if let Some(b) = else_block {
                    if let Some(t) = &b.tail {
                        self.returned(t);
                    }
                }
            }
            _ => {
                if self.escapes_local(e) {
                    self.diags.push(Diagnostic::new(
                        ErrorCode::EBorrow,
                        "returning a reference to a local",
                        e.span(),
                    ));
                }
            }
        }
    }

    /// A returned `&` is valid only when its root is a reference parameter.
    fn escapes_local(&self, e: &Expr) -> bool {
        match e {
            Expr::Unary { op: UnOp::Ref | UnOp::RefMut, expr, .. } => match place_of(expr) {
                Some(place) => !self.root_is_param_ref(&place),
                None => self.ty_of(e).is_some_and(|t| is_ref(&t)),
            },
            Expr::Ident { name, .. } => match self.lookup(name).map(|b| b.origin.clone()) {
                Some(Origin::ParamRef) => false,
                Some(Origin::Borrow { place }) => !self.root_is_param_ref(&place),
                Some(Origin::Owned) => self.lookup(name).is_some_and(|b| is_ref(&b.ty)),
                None => false,
            },
            Expr::Field { base, .. } => {
                self.ty_of(e).is_some_and(|t| is_ref(&t)) && self.escapes_local(base)
            }
            _ => false,
        }
    }

    fn root_is_param_ref(&self, place: &str) -> bool {
        let root = place.split('.').next().unwrap_or(place);
        matches!(self.lookup(root).map(|b| &b.origin), Some(Origin::ParamRef))
    }

    fn finish(&mut self) {
        for loan in &mut self.loans {
            for w in &self.whiles {
                if loan.start < w.start && loan.end >= w.start && loan.end <= w.end {
                    loan.end = w.end;
                }
            }
        }
        let loans = self.loans.clone();
        for (i, a) in loans.iter().enumerate() {
            for b in loans.iter().skip(i + 1) {
                if !overlaps(&a.place, &b.place) || !ranges_overlap(a.start, a.end, b.start, b.end)
                {
                    continue;
                }
                if a.mutable || b.mutable {
                    let span = if b.start >= a.start { b.span } else { a.span };
                    self.diags.push(Diagnostic::new(
                        ErrorCode::EBorrow,
                        format!("`{}` is already borrowed", shortest(&a.place, &b.place)),
                        span,
                    ));
                }
            }
        }
        for m in &self.moves {
            for loan in &loans {
                if !overlaps(&m.place, &loan.place) {
                    continue;
                }
                if m.span.start >= loan.start && m.span.start < loan.end {
                    self.diags.push(Diagnostic::new(
                        ErrorCode::EBorrow,
                        format!("`{}` is already borrowed", m.place),
                        m.span,
                    ));
                    break;
                }
            }
        }
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
