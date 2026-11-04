//! Scope-end `free` insertion for owned `String` locals.
//!
//! Runs on the checked AST right before lowering. A local is dropped when it
//! is bound once (`let` or first assignment) to a fresh heap string, is never reassigned (except
//! the in-place `x = x + …`) and never escapes: it is only read by borrowing
//! builtins and by `+`. Everything else keeps the old behaviour (no free), so
//! handle-typed code (`str_as_int`, `vec_push`, user calls) stays sound.
//!
//! Frees go at the end of the block that declares the local and before every
//! `return` in its scope. A block value is bound to a temporary first so the
//! free runs after the value is computed.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::span::Span;
use crate::string_expr::add_chain_leftmost;

/// Variant name -> per-field "may own heap memory" flags (a field typed `Int`,
/// `Float` or `Bool` is Copy; anything else, including a type parameter, may not be).
type Variants = HashMap<String, Vec<bool>>;

/// Per user fn, per parameter: the fn only reads it (never lets it escape), so a caller
/// keeps ownership of a value passed there.
type ReadOnly = HashMap<String, Vec<bool>>;

thread_local! {
    /// Flat structs (see `ir::flat_struct_kind`): name -> per-field "is a String".
    static FLATS: std::cell::RefCell<HashMap<String, Vec<bool>>> = std::cell::RefCell::new(HashMap::new());
}

fn collect_flats(prog: &Program) {
    let mut m = HashMap::new();
    for item in &prog.items {
        if let Item::Struct(sd) = item {
            if crate::ir::flat_struct_kind(sd).is_some() {
                m.insert(sd.name.clone(), sd.fields.iter().map(|f| crate::string_expr::is_string_type(&f.ty)).collect());
            }
        }
    }
    FLATS.with(|f| *f.borrow_mut() = m);
}

fn is_flat(name: &str) -> bool {
    FLATS.with(|f| f.borrow().contains_key(name))
}

/// `S(a, b, ..)` of a flat struct whose String fields are all fresh values: the list
/// that stores it owns the block and its strings.
fn is_fresh_flat_ctor(e: &Expr) -> bool {
    let Expr::Call { func, args, .. } = e else {
        return false;
    };
    let Expr::Ident { name, .. } = func.as_ref() else {
        return false;
    };
    FLATS.with(|f| {
        f.borrow().get(name).is_some_and(|strs| {
            strs.len() == args.len() && strs.iter().zip(args).all(|(is_str, a)| !is_str || is_fresh_value(a))
        })
    })
}

/// Builtins that only read a `String` argument and whose own result does not alias it.
fn reads_string_arg(name: &str) -> bool {
    matches!(
        name,
        "str_len"
            | "str_eq"
            | "str_char"
            | "str_contains"
            | "str_skip"
            | "str_hash"
            | "str_to_f64_bits"
            | "print_str"
            | "eprint_str"
            | "write_file"
            | "mkdir_p"
            | "path_readable"
            | "getenv"
            | "str_slice"
            | "read_file"
    )
}

/// A fresh string that is not a literal: a temporary somebody must release.
fn is_fresh_temp(e: &Expr) -> bool {
    !matches!(e, Expr::String { .. }) && is_fresh_value(e)
}

struct Hoist {
    n: u32,
}

impl Hoist {
    /// Binds each fresh temporary passed to a reader builtin to its own local, pushed to
    /// `out` ahead of the statement, so the scope-end release frees it.
    fn expr(&mut self, e: Expr, out: &mut Vec<Stmt>) -> Expr {
        match e {
            Expr::Call { func, type_args, args, span } => {
                let reads = matches!(func.as_ref(), Expr::Ident { name, .. } if reads_string_arg(name));
                let args = args
                    .into_iter()
                    .map(|a| {
                        let a = self.expr(a, out);
                        if reads && is_fresh_temp(&a) {
                            self.n += 1;
                            let name = format!("__tmp{}", self.n);
                            let asp = a.span();
                            let fake = Span { start: u32::MAX - self.n, end: u32::MAX - self.n, line: 0, col: 0 };
                            out.push(Stmt::Let {
                                name: name.clone(),
                                ty: None,
                                init: a,
                                frozen: false,
                                stack: false,
                                span: fake,
                            });
                            Expr::Ident { name, span: asp }
                        } else {
                            a
                        }
                    })
                    .collect();
                Expr::Call { func, type_args, args, span }
            }
            Expr::Binary { op, left, right, span } => {
                let left = Box::new(self.expr(*left, out));
                let right = Box::new(self.expr(*right, out));
                Expr::Binary { op, left, right, span }
            }
            Expr::If { cond, then_block, elsifs, else_block, span } => Expr::If {
                cond,
                then_block: self.block(then_block),
                elsifs: elsifs
                    .into_iter()
                    .map(|a| Elsif { cond: a.cond, body: self.block(a.body), span: a.span })
                    .collect(),
                else_block: else_block.map(|b| self.block(b)),
                span,
            },
            other => other,
        }
    }

    fn block(&mut self, b: Block) -> Block {
        let mut stmts = Vec::new();
        for st in b.stmts {
            match st {
                Stmt::Break { span } => stmts.push(Stmt::Break { span }),
                Stmt::Let { name, ty, init, frozen, stack, span } => {
                    let init = self.expr(init, &mut stmts);
                    stmts.push(Stmt::Let { name, ty, init, frozen, stack, span });
                }
                Stmt::Assign { target, value, span } => {
                    let value = self.expr(value, &mut stmts);
                    stmts.push(Stmt::Assign { target, value, span });
                }
                Stmt::Expr(e) => {
                    let e = self.expr(e, &mut stmts);
                    stmts.push(Stmt::Expr(e));
                }
                Stmt::While { cond, body, span } => {
                    let body = self.block(body);
                    stmts.push(Stmt::While { cond, body, span });
                }
                Stmt::Return { value, span } => {
                    let value = value.map(|v| self.expr(v, &mut stmts));
                    stmts.push(Stmt::Return { value, span });
                }
            }
        }
        let tail = b.tail.map(|t| Box::new(self.expr(*t, &mut stmts)));
        Block { stmts, tail, span: b.span }
    }
}

pub fn insert_drops(prog: &Program) -> Program {
    let mut hoisted = prog.clone();
    for item in &mut hoisted.items {
        if let Item::Fn(f) = item {
            let span = f.body.span;
            let body = std::mem::replace(&mut f.body, Block { stmts: vec![], tail: None, span });
            f.body = Hoist { n: 0 }.block(body);
        }
    }
    collect_flats(prog);
    let mut variants = Variants::new();
    for item in &prog.items {
        if let Item::Enum(e) = item {
            for v in &e.variants {
                let owned = v.fields.iter().map(|t| !is_copy_scalar(t)).collect();
                variants.insert(v.name.clone(), owned);
            }
        }
    }
    let ro = read_only_params(prog, &variants);
    let mut out = hoisted;
    for item in &mut out.items {
        if let Item::Fn(f) = item {
            drop_fn(f, &variants, &ro);
        }
    }
    out
}

/// Optimistic fixpoint: every parameter starts read-only and loses that when its fn lets
/// it escape under the current assumptions about the other fns.
fn read_only_params(prog: &Program, variants: &Variants) -> ReadOnly {
    let mut ro = ReadOnly::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            ro.insert(f.name.clone(), vec![true; f.params.len()]);
        }
    }
    loop {
        let mut changed = false;
        for item in &prog.items {
            let Item::Fn(f) = item else { continue };
            let facts = fn_facts(f, variants, &ro);
            let flags = ro.get_mut(&f.name).unwrap();
            for (i, p) in f.params.iter().enumerate() {
                if flags[i] && facts.escapes(&p.name) {
                    flags[i] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            return ro;
        }
    }
}

fn fn_facts(f: &FnDef, variants: &Variants, ro: &ReadOnly) -> Facts {
    let mut facts = Facts { variants: variants.clone(), ro: ro.clone(), ..Facts::default() };
    for p in &f.params {
        facts.bind(&p.name);
    }
    block(&f.body, &mut facts);
    propagate_payload_escapes(&mut facts);
    facts
}

/// Builtins whose `String` arguments are only read.
pub(crate) fn borrows_args(name: &str) -> bool {
    matches!(
        name,
        "str_len"
            | "str_eq"
            | "str_char"
            | "str_contains"
            | "str_skip"
            | "str_hash"
            | "str_slice"
            | "str_concat"
            | "str_to_f64_bits"
            | "print_str"
            | "eprint_str"
            | "write_file"
            | "mkdir_p"
            | "path_readable"
            | "getenv"
            | "map_get"
            | "read_file"
            | "list_len"
            | "list_get"
            | "list_pop"
            | "dict_get"
            | "dict_has"
            | "dict_len"
            | "dict_remove"
            | "dict_keys"
            | "dict_values"
            | "enum_owns"
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
    )
}

/// Builtins that return a fresh heap string nobody else owns.
fn returns_fresh(name: &str) -> bool {
    matches!(
        name,
        "int_to_str" | "char_to_str" | "str_slice" | "strdup" | "read_file" | "str_concat"
    )
}

/// How a fresh local is released: `free` (String), `list_free`, `bytes_free`,
/// `dict_free`, or `enum_free` (the box of a variant with fields; `variants`
/// maps every variant name to whether it has fields).
///
/// The flag says the container owns its elements: it is only released when every
/// value stored in it is a fresh owned value (see `is_fresh_value`).
fn fresh_kind(init: &Expr, variants: &Variants) -> Option<(&'static str, bool)> {
    match init {
        Expr::String { .. } => Some(("free", false)),
        Expr::Call { func, type_args, .. } => {
            let Expr::Ident { name, .. } = func.as_ref() else {
                return None;
            };
            if returns_fresh(name) {
                Some(("free", false))
            } else if variants.get(name.as_str()).is_some_and(|f| !f.is_empty()) {
                Some(("enum_free", false))
            } else if name == "bytes_new" {
                Some(("bytes_free", false))
            } else if name == "list_new" && list_flat_arg(type_args) {
                Some(("list_free", true))
            } else if name == "list_new" && list_elem_args(type_args) {
                Some(("list_free", list_owns_elems(type_args)))
            } else if matches!(name.as_str(), "dict_keys" | "dict_values") {
                // A fresh list whose String elements are copies the list owns.
                Some(("list_free", true))
            } else if name == "dict_new" && dict_args(type_args) {
                Some(("dict_free", dict_owns_values(type_args)))
            } else {
                None
            }
        }
        Expr::Binary { op: BinOp::Add, .. } if add_is_string_chain(init) => Some(("free", false)),
        _ => None,
    }
}

fn list_flat_arg(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, args, .. })] if args.is_empty() && is_flat(name))
}

fn list_owns_elems(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, .. })] if name == "String")
}

fn dict_owns_values(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [_, TypeArg::Type(Type::Named { name, .. })] if name == "String")
}

/// A value whose ownership is certainly the container's once stored: a string
/// literal, a fresh-string call or a `+` string chain. A name or a call to a
/// user function may be someone else's value, so storing it taints the container.
pub(crate) fn is_fresh_value(e: &Expr) -> bool {
    match e {
        Expr::String { .. } => true,
        Expr::Call { func, .. } => {
            matches!(func.as_ref(), Expr::Ident { name, .. } if returns_fresh(name))
        }
        Expr::Binary { op: BinOp::Add, .. } => add_is_string_chain(e),
        _ => false,
    }
}

/// `list_new[T]()` with `T` Int, Float, Bool or String (or no type argument,
/// which is Int). A String list owns its elements and `list_free` releases them.
fn list_elem_args(type_args: &[TypeArg]) -> bool {
    match type_args {
        [] => true,
        [TypeArg::Type(Type::Named { name, args, .. })] => {
            args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool" | "String")
        }
        _ => false,
    }
}

/// `dict_new[K, V]()` with `K` Int or String and `V` Int, Float, Bool or String.
fn dict_args(type_args: &[TypeArg]) -> bool {
    let scalar = |t: &TypeArg, names: &[&str]| match t {
        TypeArg::Type(Type::Named { name, args, .. }) => {
            args.is_empty() && names.contains(&name.as_str())
        }
        _ => false,
    };
    match type_args {
        [k, v] => scalar(k, &["Int", "String"]) && scalar(v, &["Int", "Float", "Bool", "String"]),
        _ => false,
    }
}

/// A `+` chain is a string concatenation (so its result is a fresh string) when
/// some operand is syntactically a string: a literal or a fresh-string call.
/// A string right operand forces a string left operand (`E_TYPE` otherwise).
fn add_is_string_chain(e: &Expr) -> bool {
    match e {
        Expr::Binary { op: BinOp::Add, left, right, .. } => {
            add_is_string_chain(left) || is_string_operand(right)
        }
        other => is_string_operand(other),
    }
}

fn is_string_operand(e: &Expr) -> bool {
    match e {
        Expr::String { .. } => true,
        Expr::Call { func, .. } => {
            matches!(func.as_ref(), Expr::Ident { name, .. } if returns_fresh(name))
        }
        _ => false,
    }
}

#[derive(Default)]
struct Facts {
    escaped: HashSet<String>,
    bindings: HashMap<String, u32>,
    variants: Variants,
    /// Containers that received a value not known to be fresh.
    tainted: HashSet<String>,
    /// `match x` over a local: the names its variant patterns bind.
    matches: Vec<(String, Vec<String>)>,
    ro: ReadOnly,
    /// Names that escape only at whole statements of their own loop depth: the local still
    /// drops, guarded by a flag cleared right after each such statement.
    soft: HashSet<String>,
    /// Statement (span) -> names escaping at it.
    sites: HashMap<(u32, u32), Vec<String>>,
    cur: Option<(u32, u32)>,
    /// Inside blocks `xform_block` does not visit: nothing there can clear a flag.
    opaque: u32,
    loop_depth: u32,
    bind_depth: HashMap<String, u32>,
}

impl Facts {
    fn bind(&mut self, name: &str) {
        *self.bindings.entry(name.to_string()).or_insert(0) += 1;
        let d = self.loop_depth;
        self.bind_depth.entry(name.to_string()).or_insert(d);
    }

    fn escapes(&self, name: &str) -> bool {
        self.escaped.contains(name) || self.soft.contains(name)
    }
}

fn stmt_key(sp: Span) -> (u32, u32) {
    (sp.start, sp.end)
}

fn arg(a: &Expr, safe: bool, f: &mut Facts) {
    if let Expr::Ident { name, .. } = a {
        if !safe {
            match f.cur {
                Some(key) if f.bind_depth.get(name).is_some_and(|d| *d == f.loop_depth) => {
                    f.soft.insert(name.clone());
                    let v = f.sites.entry(key).or_default();
                    if !v.contains(name) {
                        v.push(name.clone());
                    }
                }
                _ => {
                    f.escaped.insert(name.clone());
                }
            }
        }
    } else {
        expr(a, f);
    }
}

fn expr(e: &Expr, f: &mut Facts) {
    match e {
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } => {}
        Expr::Ident { name, .. } => {
            f.escaped.insert(name.clone());
        }
        Expr::Call { func, args, .. } => {
            let borrow = matches!(func.as_ref(), Expr::Ident { name, .. } if borrows_args(name));
            if !matches!(func.as_ref(), Expr::Ident { .. }) {
                expr(func, f);
            }
            let ro = match func.as_ref() {
                Expr::Ident { name, .. } => f.ro.get(name).cloned(),
                _ => None,
            };
            for (i, a) in args.iter().enumerate() {
                let reads = ro.as_ref().and_then(|r| r.get(i)).copied().unwrap_or(false);
                arg(a, borrow || reads, f);
            }
        }
        Expr::Lambda { params, body, .. } => {
            for p in params {
                f.bind(p);
            }
            f.opaque += 1;
            expr(body, f);
            f.opaque -= 1;
        }
        Expr::Binary { left, right, .. } => {
            arg(left, true, f);
            arg(right, true, f);
        }
        Expr::Unary { expr: x, .. } => arg(x, true, f),
        Expr::Block(b) => {
            f.opaque += 1;
            block(b, f);
            f.opaque -= 1;
        }
        Expr::On { body, kernel_index, .. } => {
            if let Some(k) = kernel_index {
                for n in &k.names {
                    f.bind(n);
                }
            }
            f.opaque += 1;
            block(body, f);
            f.opaque -= 1;
        }
        Expr::To { expr: x, .. } => arg(x, false, f),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    arg(c, false, f);
                }
            }
        }
        Expr::Field { base, .. } => arg(base, true, f),
        Expr::Match { scrutinee, arms, .. } => {
            // Variant patterns only read the box; a bare binding aliases the whole value.
            let binds_whole = arms.iter().any(|a| {
                matches!(&a.pattern, Pattern::Ident(n, _) if !f.variants.contains_key(n))
            });
            if let Expr::Ident { name, .. } = scrutinee.as_ref() {
                let mut bound = Vec::new();
                for a in arms {
                    bound_names(&a.pattern, &f.variants, &mut bound);
                }
                f.matches.push((name.clone(), bound));
            }
            arg(scrutinee, !binds_whole, f);
            f.opaque += 1;
            for a in arms {
                pattern(&a.pattern, f);
                expr(&a.body, f);
            }
            f.opaque -= 1;
        }
        Expr::If { .. } => {
            f.opaque += 1;
            walk_if(e, f);
            f.opaque -= 1;
        }
        Expr::Try { expr: x, .. } => arg(x, false, f),
    }
}

/// The branches of an `if`; `xform_block` visits them when the `if` is a whole statement
/// expression, a `let` or assignment value, or a block tail.
fn walk_if(e: &Expr, f: &mut Facts) {
    let Expr::If { cond, then_block, elsifs, else_block, .. } = e else {
        return;
    };
    arg(cond, true, f);
    block(then_block, f);
    for a in elsifs {
        arg(&a.cond, true, f);
        block(&a.body, f);
    }
    if let Some(b) = else_block {
        block(b, f);
    }
}

/// An expression in a position `xform_expr` rewrites.
fn top(e: &Expr, safe: bool, f: &mut Facts) {
    if matches!(e, Expr::If { .. }) {
        walk_if(e, f);
    } else {
        arg(e, safe, f);
    }
}

fn bound_names(p: &Pattern, variants: &Variants, out: &mut Vec<String>) {
    if let Pattern::Variant { name, args, .. } = p {
        let owned = variants.get(name.as_str());
        for (i, a) in args.iter().enumerate() {
            match a {
                Pattern::Ident(n, _) => {
                    // A Copy field cannot alias heap memory; unknown variants stay conservative.
                    if owned.and_then(|o| o.get(i)).copied().unwrap_or(true) {
                        out.push(n.clone());
                    }
                }
                other => bound_names(other, variants, out),
            }
        }
    }
}

/// A payload bound by a pattern aliases a field of the box; if that name escapes,
/// the box can no longer be freed (nor the box of a scrutinee it came from).
fn propagate_payload_escapes(f: &mut Facts) {
    loop {
        let mut changed = false;
        for (scrut, bound) in &f.matches {
            if !f.escaped.contains(scrut) && bound.iter().any(|b| f.escapes(b)) {
                f.escaped.insert(scrut.clone());
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

fn pattern(p: &Pattern, f: &mut Facts) {
    match p {
        Pattern::Wild(_) | Pattern::Int(_, _) => {}
        Pattern::Ident(n, _) => f.bind(n),
        Pattern::Variant { args, .. } => {
            for a in args {
                pattern(a, f);
            }
        }
    }
}

/// `list_push(xs, v)` (also `list_set`, `dict_put`) as a statement, or bound to
/// a `_name`, discards the returned alias of `xs`, so `xs` is only borrowed there.
fn push_stmt(e: &Expr, f: &mut Facts) -> bool {
    let Expr::Call { func, args, .. } = e else {
        return false;
    };
    if !matches!(func.as_ref(), Expr::Ident { name, .. } if matches!(name.as_str(), "list_push" | "list_set" | "dict_put"))
        || args.is_empty()
    {
        return false;
    }
    match &args[0] {
        Expr::Ident { .. } => {}
        inner => {
            if !push_stmt(inner, f) {
                expr(inner, f);
            }
        }
    }
    // The stored value is the last argument (`list_push(xs, v)`, `list_set(xs, i, v)`, `dict_put(d, k, v)`).
    if let (Expr::Ident { name: container, .. }, Some(v)) = (&args[0], args.last()) {
        if args.len() >= 2 && !is_fresh_value(v) && !is_fresh_flat_ctor(v) {
            f.tainted.insert(container.clone());
        }
    }
    for a in &args[1..] {
        arg(a, false, f);
    }
    true
}

fn block(b: &Block, f: &mut Facts) {
    for st in &b.stmts {
        let outer = f.cur;
        let live = f.opaque == 0;
        match st {
            Stmt::Break { .. } => {}
            Stmt::Let { name, init, span, .. } => {
                f.cur = live.then(|| stmt_key(*span));
                f.bind(name);
                top(init, false, f);
            }
            Stmt::Expr(e) => {
                f.cur = live.then(|| stmt_key(e.span()));
                if !push_stmt(e, f) {
                    if matches!(e, Expr::If { .. }) {
                        walk_if(e, f);
                    } else {
                        expr(e, f);
                    }
                }
            }
            Stmt::Assign { target, value, span } => {
                f.cur = live.then(|| stmt_key(*span));
                if let Expr::Ident { name, .. } = target {
                    if !is_accumulate(name, value) {
                        f.bind(name);
                    }
                } else {
                    expr(target, f);
                }
                let discard = matches!(target, Expr::Ident { name, .. } if name.starts_with('_'));
                if !(discard && push_stmt(value, f)) {
                    top(value, false, f);
                }
            }
            Stmt::Return { value, span } => {
                f.cur = live.then(|| stmt_key(*span));
                if let Some(v) = value {
                    arg(v, false, f);
                }
            }
            Stmt::While { cond, body, .. } => {
                f.cur = None;
                arg(cond, true, f);
                f.loop_depth += 1;
                block(body, f);
                f.loop_depth -= 1;
            }
        }
        f.cur = outer;
    }
    if let Some(t) = &b.tail {
        let outer = f.cur;
        f.cur = None;
        top(t, false, f);
        f.cur = outer;
    }
}

/// `x = x + …`: appends in place, not a new binding.
fn is_accumulate(name: &str, value: &Expr) -> bool {
    matches!(value, Expr::Binary { op: BinOp::Add, .. })
        && matches!(add_chain_leftmost(value), Expr::Ident { name: l, .. } if l == name)
}

struct Ctx {
    facts: Facts,
    params: HashSet<String>,
    live: Vec<(String, &'static str, bool)>,
    tmp: u32,
}

impl Ctx {
    /// The release function for a droppable local, if `name = init` declares one.
    fn candidate(&self, name: &str, ty: &Option<Type>, init: &Expr) -> Option<(&'static str, bool)> {
        let (kind, owned) = fresh_kind(init, &self.facts.variants)?;
        if owned && self.facts.tainted.contains(name) {
            return None;
        }
        if let Some(t) = ty {
            if kind != "free" || !crate::string_expr::is_string_type(t) {
                return None;
            }
        }
        let ok = !self.params.contains(name)
            && self.facts.bindings.get(name) == Some(&1)
            && !self.facts.escaped.contains(name);
        ok.then_some((kind, self.facts.soft.contains(name)))
    }
}

fn is_copy_scalar(t: &Type) -> bool {
    matches!(t, Type::Named { name, args, .. } if args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool"))
}

fn drop_fn(f: &mut FnDef, variants: &Variants, ro: &ReadOnly) {
    let facts = fn_facts(f, variants, ro);
    let mut cx = Ctx {
        facts,
        params: f.params.iter().map(|p| p.name.clone()).collect(),
        live: Vec::new(),
        tmp: 0,
    };
    let span = f.body.span;
    let body = std::mem::replace(&mut f.body, Block { stmts: vec![], tail: None, span });
    f.body = xform_block(body, &mut cx);
}

fn free_call(name: &str, release: &str, span: Span) -> Stmt {
    Stmt::Expr(Expr::Call {
        func: Box::new(Expr::Ident { name: release.into(), span }),
        type_args: vec![],
        args: vec![Expr::Ident { name: name.into(), span }],
        span,
    })
}

fn flag_name(name: &str) -> String {
    format!("__live_{name}")
}

fn int_expr(value: i64, span: Span) -> Expr {
    Expr::Int { value, span }
}

/// `__live_x = 1` after the declaration of a flagged local.
fn flag_init(name: &str, span: Span) -> Stmt {
    Stmt::Let {
        name: flag_name(name),
        ty: None,
        init: int_expr(1, span),
        frozen: false,
        stack: false,
        span,
    }
}

fn flag_clear(name: &str, span: Span) -> Stmt {
    Stmt::Assign {
        target: Expr::Ident { name: flag_name(name), span },
        value: int_expr(0, span),
        span,
    }
}

/// The release of a live local: guarded by its flag when a statement may have moved it.
fn release(name: &str, kind: &str, flagged: bool, span: Span) -> Stmt {
    let call = free_call(name, kind, span);
    if !flagged {
        return call;
    }
    Stmt::Expr(Expr::If {
        cond: Box::new(Expr::Binary {
            op: BinOp::Eq,
            left: Box::new(Expr::Ident { name: flag_name(name), span }),
            right: Box::new(int_expr(1, span)),
            span,
        }),
        then_block: Block { stmts: vec![call], tail: None, span },
        elsifs: vec![],
        else_block: None,
        span,
    })
}

/// Clears the flag of every flagged live local the statement at `key` moved.
fn clear_moved(cx: &Ctx, key: (u32, u32), span: Span, out: &mut Vec<Stmt>) {
    if let Some(names) = cx.facts.sites.get(&key) {
        for n in names {
            if cx.live.iter().any(|(l, _, fl)| l == n && *fl) {
                out.push(flag_clear(n, span));
            }
        }
    }
}

fn xform_expr(e: Expr, cx: &mut Ctx) -> Expr {
    match e {
        Expr::If { cond, then_block, elsifs, else_block, span } => Expr::If {
            cond,
            then_block: xform_block(then_block, cx),
            elsifs: elsifs
                .into_iter()
                .map(|a| Elsif { cond: a.cond, body: xform_block(a.body, cx), span: a.span })
                .collect(),
            else_block: else_block.map(|b| xform_block(b, cx)),
            span,
        },
        other => other,
    }
}

fn xform_block(b: Block, cx: &mut Ctx) -> Block {
    let mark = cx.live.len();
    let mut stmts = Vec::new();
    let mut diverged = false;
    for st in b.stmts {
        if diverged {
            stmts.push(st);
            continue;
        }
        match st {
            Stmt::Break { span } => stmts.push(Stmt::Break { span }),
            Stmt::Let { name, ty, init, frozen, stack, span } => {
                let cand = cx.candidate(&name, &ty, &init);
                let init = xform_expr(init, cx);
                stmts.push(Stmt::Let { name: name.clone(), ty, init, frozen, stack, span });
                clear_moved(cx, stmt_key(span), span, &mut stmts);
                if let Some((k, flagged)) = cand {
                    if flagged {
                        stmts.push(flag_init(&name, span));
                    }
                    cx.live.push((name, k, flagged));
                }
            }
            Stmt::Assign { target, value, span } => {
                let decl = match &target {
                    Expr::Ident { name, .. } if !is_accumulate(name, &value) => {
                        cx.candidate(name, &None, &value).map(|(k, fl)| (name.clone(), k, fl))
                    }
                    _ => None,
                };
                let value = xform_expr(value, cx);
                stmts.push(Stmt::Assign { target, value, span });
                clear_moved(cx, stmt_key(span), span, &mut stmts);
                if let Some(d) = decl {
                    if d.2 {
                        stmts.push(flag_init(&d.0, span));
                    }
                    cx.live.push(d);
                }
            }
            Stmt::Expr(e) => {
                let span = e.span();
                stmts.push(Stmt::Expr(xform_expr(e, cx)));
                clear_moved(cx, stmt_key(span), span, &mut stmts);
            }
            Stmt::While { cond, body, span } => {
                let body = xform_block(body, cx);
                stmts.push(Stmt::While { cond, body, span });
            }
            Stmt::Return { value, span } => {
                if cx.live.is_empty() {
                    stmts.push(Stmt::Return { value, span });
                } else {
                    let value = match value {
                        Some(v) => {
                            cx.tmp += 1;
                            let t = format!("__ret{}", cx.tmp);
                            stmts.push(Stmt::Let {
                                name: t.clone(),
                                ty: None,
                                init: v,
                                frozen: false,
                                stack: false,
                                span,
                            });
                            Some(Expr::Ident { name: t, span })
                        }
                        None => None,
                    };
                    clear_moved(cx, stmt_key(span), span, &mut stmts);
                    for (n, k, fl) in cx.live.iter().rev() {
                        stmts.push(release(n, k, *fl, span));
                    }
                    stmts.push(Stmt::Return { value, span });
                }
                diverged = true;
            }
        }
    }
    let mut tail = b.tail.map(|t| Box::new(xform_expr(*t, cx)));
    if !diverged {
        let own: Vec<(String, &'static str, bool)> = cx.live[mark..].to_vec();
        if !own.is_empty() {
            if let Some(t) = tail.take() {
                cx.tmp += 1;
                let name = format!("__ret{}", cx.tmp);
                let span = b.span;
                stmts.push(Stmt::Let {
                    name: name.clone(),
                    ty: None,
                    init: *t,
                    frozen: false,
                    stack: false,
                    span,
                });
                for (n, k, fl) in own.iter().rev() {
                    stmts.push(release(n, k, *fl, span));
                }
                tail = Some(Box::new(Expr::Ident { name, span }));
            } else {
                for (n, k, fl) in own.iter().rev() {
                    stmts.push(release(n, k, *fl, b.span));
                }
            }
        }
    }
    cx.live.truncate(mark);
    Block { stmts, tail, span: b.span }
}
