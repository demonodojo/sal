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
    /// Type of each expression by span (from the inference, `StringEnv::types`).
    static EXPR_TYPES: std::cell::RefCell<HashMap<(u32, u32), Type>> = std::cell::RefCell::new(HashMap::new());
    /// The descriptor of each ownable struct (`ir::struct_desc`), for copies released at scope end.
    static STRUCT_DESC: std::cell::RefCell<HashMap<String, String>> = std::cell::RefCell::new(HashMap::new());
    /// Field name and type of every struct, so a borrowed field can be copied when it takes ownership.
    static FIELDS: std::cell::RefCell<HashMap<String, Vec<(String, Type)>>> = std::cell::RefCell::new(HashMap::new());
    /// Structs a list can own (see `ir::struct_desc`): name -> the class of each field.
    static OWNABLE: std::cell::RefCell<HashMap<String, Vec<FieldOwn>>> = std::cell::RefCell::new(HashMap::new());
    /// Names of the enum types of the program (a `List[E]` of them owns its boxes).
    static ENUM_TYPES: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
    /// Variant constructor names (`Some`, `None`, …). A call of one owns its payload.
    static VARIANT_CTORS: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
    /// User fns whose `String` result is always fresh: the caller owns it (see `owned_return_fns`).
    static OWNED_FNS: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
    /// Locals passed to `list_push_share`: the list borrows the element, so a later
    /// `list_get` is the same pointer and the list must not be released with `list_free`.
    static SHARED_LISTS: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
    /// Names of the user fns (of the program and of the modules it calls). A container one
    /// returns by value is moved to the caller (SPEC «Memoria»): the caller owns and releases it.
    static USER_FNS: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
}

/// The release of the container a user fn returns by value, from the type of the call.
fn user_call_kind(name: &str, call: &Expr) -> Option<&'static str> {
    if !USER_FNS.with(|f| f.borrow().contains(name)) {
        return None;
    }
    match type_at(call)? {
        Type::Named { name, .. } => match name.as_str() {
            "List" => Some("list_free"),
            "Dict" => Some("dict_free"),
            "Bytes" => Some("bytes_free"),
            _ => None,
        },
        _ => None,
    }
}

fn collect_enum_types(enums: &HashMap<String, EnumDef>) {
    let set = enums.keys().cloned().collect::<HashSet<String>>();
    let ctors = enums
        .values()
        .flat_map(|e| e.variants.iter().map(|v| v.name.clone()))
        .collect::<HashSet<String>>();
    ENUM_TYPES.with(|f| *f.borrow_mut() = set);
    VARIANT_CTORS.with(|f| *f.borrow_mut() = ctors);
}

fn is_variant_ctor(name: &str) -> bool {
    VARIANT_CTORS.with(|f| f.borrow().contains(name))
}

fn is_enum_type(name: &str) -> bool {
    ENUM_TYPES.with(|f| f.borrow().contains(name))
}

/// `list_new[E]()` of a user enum: the list owns the boxes stored in it.
fn list_enum_arg(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, args, .. })] if args.is_empty() && is_enum_type(name))
}

/// `list_new[String]()`: the list owns its strings.
fn list_string_arg(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, args, .. })] if args.is_empty() && name == "String")
}

/// A variant constructor call `V(..)` builds a new box, and a unit variant `V` is a
/// tag: both are fresh values for a list to own (a box owns the fresh fields it was
/// built with, see `enum_owned_mask`).
fn is_fresh_variant_ctor(e: &Expr, variants: &Variants) -> bool {
    let has_fields = |name: &str| variants.get(name).is_some_and(|f| !f.is_empty());
    match e {
        Expr::Call { func, .. } => matches!(func.as_ref(), Expr::Ident { name, .. } if has_fields(name)),
        Expr::Ident { name, .. } => variants.get(name.as_str()).is_some_and(|f| f.is_empty()),
        _ => false,
    }
}

/// The class of a struct field for ownership: a Copy scalar, a String, an enum box, or a
/// struct box (see `ir::struct_desc`).
#[derive(Clone, Copy)]
enum FieldOwn {
    Scalar,
    Str,
    Enum,
    Struct,
}

/// The program's structs and enums, the imported ones included (as the lowering sees them).
fn collect_ownable(structs: &HashMap<String, StructDef>, enums: &HashMap<String, EnumDef>) {
    let mut m = HashMap::new();
    for sd in structs.values() {
        if crate::ir::struct_desc(&sd.name, &structs, &enums).is_none() {
            continue;
        }
        let kinds = sd
            .fields
            .iter()
            .map(|f| match &f.ty {
                _ if crate::string_expr::is_string_type(&f.ty) => FieldOwn::Str,
                Type::Named { name, .. } if enums.contains_key(name) => FieldOwn::Enum,
                Type::Named { name, .. } if structs.contains_key(name) => FieldOwn::Struct,
                _ => FieldOwn::Scalar,
            })
            .collect();
        m.insert(sd.name.clone(), kinds);
    }
    OWNABLE.with(|f| *f.borrow_mut() = m);
    let fields = structs
        .iter()
        .map(|(n, sd)| {
            (
                n.clone(),
                sd.fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect(),
            )
        })
        .collect();
    FIELDS.with(|f| *f.borrow_mut() = fields);
    let descs = structs
        .keys()
        .filter_map(|n| crate::ir::struct_desc(n, &structs, &enums).map(|d| (n.clone(), d)))
        .collect();
    STRUCT_DESC.with(|f| *f.borrow_mut() = descs);
}

/// `list_get` borrows. Only an explicit `desc_copy` (inserted where the borrow takes
/// ownership) is a fresh struct box. `struct:<desc>` names the release.
fn struct_copy_kind(local: &str, init: &Expr) -> Option<&'static str> {
    let _ = local;
    let Expr::Call { func, args, .. } = init else {
        return None;
    };
    // `x.clone()` of a describable struct is a fresh box too (SPEC «Memoria»).
    if matches!(func.as_ref(), Expr::Ident { name, .. } if name == "struct_clone") {
        let Some(Type::Named { name: sn, args: targs, .. }) = type_at(init) else {
            return None;
        };
        if !targs.is_empty() || !is_ownable_struct(&sn) {
            return None;
        }
        let desc = STRUCT_DESC.with(|t| t.borrow().get(&sn).cloned())?;
        return Some(Box::leak(format!("struct:{desc}").into_boxed_str()));
    }
    if !matches!(func.as_ref(), Expr::Ident { name, .. } if name == "desc_copy") {
        return None;
    }
    let desc = match args.first() {
        Some(Expr::String { value, .. }) if value.starts_with('S') => value.clone(),
        _ => return None,
    };
    Some(Box::leak(format!("struct:{desc}").into_boxed_str()))
}

fn is_ownable_struct(name: &str) -> bool {
    OWNABLE.with(|f| f.borrow().contains_key(name))
}

/// The callee takes ownership of a describable struct's box (`list_push`, a ctor, …).
fn callee_transfers_struct_box(name: &str) -> bool {
    matches!(name, "list_push" | "list_set" | "dict_put") || is_ownable_struct(name)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum ArgMove {
    Read,
    TransferBox,
    TakeFields,
}

/// `S(a, b, ..)` of a struct a list can own, whose String fields are fresh values, whose
/// enum fields are variant constructors and whose struct fields are fresh constructors:
/// the list that stores it owns the blocks, boxes and strings.
fn is_fresh_struct_ctor(e: &Expr, variants: &Variants) -> bool {
    let Expr::Call { func, args, .. } = e else {
        return false;
    };
    let Expr::Ident { name, .. } = func.as_ref() else {
        return false;
    };
    OWNABLE.with(|f| {
        f.borrow().get(name).is_some_and(|kinds| {
            kinds.len() == args.len()
                && kinds.iter().zip(args).all(|(k, a)| match k {
                    FieldOwn::Scalar => true,
                    FieldOwn::Str => is_fresh_value(a),
                    FieldOwn::Enum => is_fresh_variant_ctor(a, variants),
                    FieldOwn::Struct => is_fresh_struct_ctor(a, variants),
                })
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
            | "str_index_of"
            | "str_index_from"
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
    !matches!(e, Expr::String { .. }) && (is_fresh_value(e) || is_owned_call(e))
}

/// A place: local, field of a place, or deref of a place (SPEC «Memoria» / Rust).
fn is_place(e: &Expr) -> bool {
    match e {
        Expr::Ident { .. } => true,
        Expr::Field { base, .. } => is_place(base),
        Expr::Unary {
            op: UnOp::Deref,
            expr,
            ..
        } => is_place(expr),
        _ => false,
    }
}

/// A call to a user fn whose `String` result the caller owns (see `owned_return_fns`).
fn is_owned_call(e: &Expr) -> bool {
    matches!(e, Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident { name, .. } if OWNED_FNS.with(|f| f.borrow().contains(name))))
}

struct Hoist {
    n: u32,
    /// The function being hoisted: its temporaries get names unique across the program.
    fn_name: String,
    /// Local lists of the function (`xs`, a parameter or `xs = list_new[S]()`) -> the struct `S`.
    elems: HashMap<String, String>,
}

thread_local! {
    /// Struct type of each `list_get(..).f` temporary (`__tmp_<fn>_<n>`), for the lowering.
    static TMP_STRUCTS: std::cell::RefCell<HashMap<String, String>> = std::cell::RefCell::new(HashMap::new());
}

/// The struct type of each field-read temporary, for `StringEnv::struct_types`.
pub fn tmp_struct_types() -> HashMap<String, String> {
    TMP_STRUCTS.with(|t| t.borrow().clone())
}

thread_local! {
    /// The read-only flags of the last `insert_drops`.
    static READ_ONLY: std::cell::RefCell<ReadOnly> = std::cell::RefCell::new(HashMap::new());
    /// Per user fn, per parameter: `true` when the fn consumes that parameter (last `insert_drops`).
    static CONSUMED: std::cell::RefCell<ReadOnly> = std::cell::RefCell::new(HashMap::new());
    /// Per user fn, per parameter: `true` when the whole value is stored or returned
    /// (a Copy field read does not count).
    static KEPT: std::cell::RefCell<ReadOnly> = std::cell::RefCell::new(HashMap::new());
    /// Last whole-graph fixpoint. Compiling a module graph lowers one module at a time and each
    /// lowering would otherwise walk every function again.
    static FIXPOINT_CACHE: std::cell::RefCell<Option<(u64, ReadOnly, ReadOnly, ReadOnly)>> =
        std::cell::RefCell::new(None);
}

/// Per user fn, per parameter: `false` when the fn lets that parameter escape (last `insert_drops`).
pub fn last_read_only_params() -> HashMap<String, Vec<bool>> {
    READ_ONLY.with(|r| r.borrow().clone())
}

/// Per user fn, per parameter: `true` when the fn takes ownership of that parameter's data.
pub fn last_consumed_params() -> HashMap<String, Vec<bool>> {
    CONSUMED.with(|r| r.borrow().clone())
}

/// The struct `S` of each local list of `f` of type `List[S]` (by a parameter or by
/// `xs = list_new[S]()`), for the field-read temporaries.
fn list_elem_structs(f: &FnDef) -> HashMap<String, String> {
    fn elem_of(t: &Type) -> Option<String> {
        match t {
            Type::Named { name, args, .. } if name == "List" => match args.first() {
                Some(Type::Named { name, args, .. }) if args.is_empty() && is_ownable_struct(name) => Some(name.clone()),
                _ => None,
            },
            _ => None,
        }
    }
    fn note(name: &str, init: &Expr, out: &mut HashMap<String, String>) {
        if let Expr::Call { func, type_args, .. } = init {
            if matches!(
                func.as_ref(),
                Expr::Ident { name, .. } if matches!(name.as_str(), "list_new" | "list_new_owned")
            ) {
                if let [TypeArg::Type(Type::Named { name: sn, args, .. })] = type_args.as_slice() {
                    if args.is_empty() && is_ownable_struct(sn) {
                        out.insert(name.to_string(), sn.clone());
                    }
                }
            }
        }
    }
    fn block(b: &Block, out: &mut HashMap<String, String>) {
        for st in &b.stmts {
            match st {
                Stmt::Let { name, init, .. } => note(name, init, out),
                Stmt::Assign { target: Expr::Ident { name, .. }, value, .. } => note(name, value, out),
                Stmt::While { body, .. } => block(body, out),
                _ => {}
            }
        }
    }
    let mut out = HashMap::new();
    for p in &f.params {
        if let Some(sn) = elem_of(&p.ty) {
            out.insert(p.name.clone(), sn);
        }
    }
    block(&f.body, &mut out);
    out
}

/// `list_get(..)` or `list_pop(..)`: a read of an element of a list.
fn is_elem_read(e: &Expr) -> bool {
    matches!(e, Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident { name, .. } if name == "list_get" || name == "list_pop"))
}

impl Hoist {
    /// Binds each fresh temporary passed to a reader builtin, and each non-place under
    /// `&` / `&mut` (`f(&g())`), to its own local ahead of the statement, so the scope-end
    /// release (or struct glue) frees it — same as Rust (SPEC «Liberación de locales»).
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
            // `&g()` / `&mut g()`: the rvalue is not a place, so bind it (Rust temporary).
            Expr::Unary { op, expr, span } => {
                let expr = self.expr(*expr, out);
                let expr = if matches!(op, UnOp::Ref | UnOp::RefMut) && !is_place(&expr) {
                    self.n += 1;
                    let name = format!("__tmp{}", self.n);
                    let esp = expr.span();
                    let fake = Span {
                        start: u32::MAX - self.n,
                        end: u32::MAX - self.n,
                        line: 0,
                        col: 0,
                    };
                    out.push(Stmt::Let {
                        name: name.clone(),
                        ty: None,
                        init: expr,
                        frozen: false,
                        stack: false,
                        span: fake,
                    });
                    Expr::Ident { name, span: esp }
                } else {
                    expr
                };
                Expr::Unary {
                    op,
                    expr: Box::new(expr),
                    span,
                }
            }
            // `list_get(xs, i).f`: the element copy is bound to a local first, so the scope-end
            // release frees it (`struct_copy_kind`).
            Expr::Field { base, field, span } => {
                let base = self.expr(*base, out);
                // Only the element of a list whose struct a copy can release is bound (the copy must
                // be typed for the lowering, which `struct_types` gives the temporary).
                let elem_struct = match &base {
                    Expr::Call { args, .. } if is_elem_read(&base) => match args.first() {
                        Some(Expr::Ident { name: xs, .. }) => self.elems.get(xs).cloned(),
                        _ => None,
                    },
                    _ => None,
                };
                let base = if let Some(sn) = elem_struct {
                    self.n += 1;
                    let name = format!("__tmp_{}_{}", self.fn_name, self.n);
                    let bsp = base.span();
                    TMP_STRUCTS.with(|t| t.borrow_mut().insert(name.clone(), sn));
                    let fake = Span { start: u32::MAX - self.n, end: u32::MAX - self.n, line: 0, col: 0 };
                    out.push(Stmt::Let { name: name.clone(), ty: None, init: base, frozen: false, stack: false, span: fake });
                    Expr::Ident { name, span: bsp }
                } else {
                    base
                };
                Expr::Field { base: Box::new(base), field, span }
            }
            // `match list_get(xs, i) { … }`: the element read is bound to a local first, so the
            // scope-end release frees a copy that owned lists hand out (`candidate`).
            Expr::Match { scrutinee, arms, span } => {
                let scrutinee = self.expr(*scrutinee, out);
                let scrutinee = if is_elem_read(&scrutinee) {
                    self.n += 1;
                    let name = format!("__tmp{}", self.n);
                    let ssp = scrutinee.span();
                    let fake = Span { start: u32::MAX - self.n, end: u32::MAX - self.n, line: 0, col: 0 };
                    out.push(Stmt::Let { name: name.clone(), ty: None, init: scrutinee, frozen: false, stack: false, span: fake });
                    Expr::Ident { name, span: ssp }
                } else {
                    scrutinee
                };
                Expr::Match { scrutinee: Box::new(scrutinee), arms, span }
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

pub fn insert_drops(
    prog: &Program,
    types: &HashMap<(u32, u32), Type>,
    structs: &HashMap<String, StructDef>,
    enums: &HashMap<String, EnumDef>,
    callees: &[&FnDef],
) -> Program {
    EXPR_TYPES.with(|t| *t.borrow_mut() = types.clone());
    collect_ownable(structs, enums);
    collect_enum_types(enums);
    OWNED_FNS.with(|f| *f.borrow_mut() = owned_return_fns(prog));
    let user_fns: HashSet<String> = prog
        .items
        .iter()
        .filter_map(|i| if let Item::Fn(f) = i { Some(f.name.clone()) } else { None })
        .chain(callees.iter().map(|f| f.name.clone()))
        .collect();
    USER_FNS.with(|f| *f.borrow_mut() = user_fns);
    let mut hoisted = prog.clone();
    for item in &mut hoisted.items {
        if let Item::Fn(f) = item {
            let elems = list_elem_structs(f);
            let span = f.body.span;
            let body = std::mem::replace(&mut f.body, Block { stmts: vec![], tail: None, span });
            f.body = Hoist { n: 0, fn_name: f.name.clone(), elems }.block(body);
        }
    }
    let mut variants = Variants::new();
    for item in &prog.items {
        if let Item::Enum(e) = item {
            for v in &e.variants {
                let owned = v.fields.iter().map(|t| !is_copy_scalar(t)).collect();
                variants.insert(v.name.clone(), owned);
            }
        }
    }
    let ro = read_only_params(prog, &variants, callees);
    READ_ONLY.with(|r| *r.borrow_mut() = ro.clone());
    let mut out = hoisted;
    for item in &mut out.items {
        if let Item::Fn(f) = item {
            drop_fn(f, &variants, &ro);
        }
    }
    out
}

/// The user fns that return a `String` the caller owns: every `return` value and the tail is a
/// fresh builtin value or a local bound once to one (and never assigned again). A fn with a
/// branching or looping statement at the top level is not analysed, so no `return` hides in
/// it. Computed from the builtins only, so the result does not depend on the fn order.
fn owned_return_fns(prog: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            if crate::string_expr::is_string_type(&f.ret) && owned_return_body(f) {
                out.insert(f.name.clone());
            }
        }
    }
    out
}

fn owned_block_string(b: &Block, params: &HashSet<String>, fresh_locals: &HashSet<String>) -> bool {
    for st in &b.stmts {
        if let Stmt::Return { value: Some(v), .. } = st {
            if !owned_string_expr(v, params, fresh_locals) {
                return false;
            }
        }
    }
    b.tail
        .as_ref()
        .is_some_and(|t| owned_string_expr(t, params, fresh_locals))
}

fn owned_string_expr(e: &Expr, params: &HashSet<String>, fresh_locals: &HashSet<String>) -> bool {
    match e {
        Expr::Block(b) => owned_block_string(b, params, fresh_locals),
        Expr::If {
            then_block,
            else_block,
            ..
        } => {
            owned_block_string(then_block, params, fresh_locals)
                && else_block
                    .as_ref()
                    .is_some_and(|eb| owned_block_string(eb, params, fresh_locals))
        }
        Expr::Match { arms, .. } => arms.iter().all(|a| owned_string_expr(&a.body, params, fresh_locals)),
        _ => owned_string_return(e, params, fresh_locals),
    }
}

fn owned_return_body(f: &FnDef) -> bool {
    let b = &f.body;
    let params: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    let mut fresh_locals: HashSet<String> = HashSet::new();
    let mut ok = true;
    for st in &b.stmts {
        match st {
            Stmt::Let { name, init, .. } => {
                if is_branching(init) {
                    if !owned_string_expr(init, &params, &fresh_locals) {
                        ok = false;
                    } else if binds_once(&b.stmts, name) {
                        fresh_locals.insert(name.clone());
                    }
                } else if fresh_builtin(init) && binds_once(&b.stmts, name) {
                    fresh_locals.insert(name.clone());
                }
            }
            // `s = v` without a type is a binding too (the parser makes it an assignment).
            Stmt::Assign { target: Expr::Ident { name, .. }, value, .. } => {
                if is_branching(value) {
                    if !owned_string_expr(value, &params, &fresh_locals) {
                        ok = false;
                    } else if binds_once(&b.stmts, name) {
                        fresh_locals.insert(name.clone());
                    }
                } else if fresh_builtin(value) && binds_once(&b.stmts, name) {
                    fresh_locals.insert(name.clone());
                }
            }
            Stmt::While { .. } => ok = false,
            _ => {}
        }
    }
    let mut returns = Vec::new();
    for st in &b.stmts {
        if let Stmt::Return { value: Some(v), .. } = st {
            returns.push(v);
        }
    }
    let tail_ok = b
        .tail
        .as_ref()
        .is_some_and(|t| owned_string_expr(t, &params, &fresh_locals));
    ok && tail_ok && returns.iter().all(|v| owned_string_expr(v, &params, &fresh_locals))
}

/// A builtin whose result is a fresh `String` (see `fresh_kind`), without the user fns.
fn fresh_builtin(e: &Expr) -> bool {
    matches!(fresh_kind_builtin(e), Some("free"))
}

/// A `String` the caller owns: a fresh builtin, a fresh local, or a field read of a parameter
/// (`return n.name` after `take`).
fn owned_string_return(e: &Expr, params: &HashSet<String>, fresh_locals: &HashSet<String>) -> bool {
    if fresh_builtin(e) {
        return true;
    }
    if matches!(e, Expr::Ident { name, .. } if fresh_locals.contains(name)) {
        return true;
    }
    if let Expr::Field { base, .. } = e {
        if let Expr::Ident { name, .. } = base.as_ref() {
            return params.contains(name);
        }
    }
    false
}

fn fresh_kind_builtin(e: &Expr) -> Option<&'static str> {
    match e {
        Expr::String { .. } => Some("free"),
        Expr::Call { func, .. } => match func.as_ref() {
            Expr::Ident { name, .. } if returns_fresh(name) => Some("free"),
            _ => None,
        },
        Expr::Binary { op: BinOp::Add, .. } if add_is_string_chain(e) => Some("free"),
        _ => None,
    }
}

/// An `if` or `match` expression at the top of a statement or tail: it may hold a `return`.
fn is_branching(e: &Expr) -> bool {
    matches!(e, Expr::If { .. } | Expr::Match { .. })
}

/// `name` is bound by exactly one top-level statement, and that binding is the only one.
fn binds_once(stmts: &[Stmt], name: &str) -> bool {
    stmts
        .iter()
        .filter(|st| match st {
            Stmt::Let { name: n, .. } => n == name,
            Stmt::Assign { target: Expr::Ident { name: n, .. }, .. } => n == name,
            _ => false,
        })
        .count()
        == 1
}

fn fixpoint_key(fns: &[&FnDef]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    (fns.as_ptr() as usize).hash(&mut h);
    for f in fns {
        f.name.hash(&mut h);
        f.params.len().hash(&mut h);
        f.body.stmts.len().hash(&mut h);
        f.body.span.start.hash(&mut h);
        f.body.span.end.hash(&mut h);
    }
    h.finish()
}

/// Optimistic fixpoint: every parameter starts read-only and loses that when its fn lets
/// it escape under the current assumptions about the other fns.
fn read_only_params(prog: &Program, variants: &Variants, callees: &[&FnDef]) -> ReadOnly {
    let mut ro = ReadOnly::new();
    let mut cons = ReadOnly::new();
    // The module's own functions, then callees defined in other modules of the graph.
    // A call such as `tp_free_shallow` lives in another file; without its body the
    // caller never learns that the struct was consumed and frees it again.
    let mut fns: Vec<&FnDef> = Vec::new();
    let mut seen = HashSet::new();
    for item in &prog.items {
        if let Item::Fn(f) = item {
            if seen.insert(f.name.clone()) {
                fns.push(f);
            }
        }
    }
    for f in callees {
        if seen.insert(f.name.clone()) {
            fns.push(f);
        }
    }
    let cache_key = if callees.is_empty() {
        0
    } else {
        fixpoint_key(callees)
    };
    if cache_key != 0 {
        let hit = FIXPOINT_CACHE.with(|c| {
            c.borrow().as_ref().and_then(|(k, ro, cons, kept)| {
                if *k == cache_key {
                    Some((ro.clone(), cons.clone(), kept.clone()))
                } else {
                    None
                }
            })
        });
        if let Some((ro, cons, kept)) = hit {
            CONSUMED.with(|c| *c.borrow_mut() = cons);
            KEPT.with(|k| *k.borrow_mut() = kept);
            return ro;
        }
    }
    let mut kept_map = ReadOnly::new();
    for f in &fns {
        // A by-value parameter is owned by the fn (SPEC «Memoria»): the caller gave it up, so it
        // is never read-only. Only `&T` and inferred borrows leave the caller the release.
        ro.insert(f.name.clone(), f.params.iter().map(|p| p.mode != ParamMode::Take).collect());
        cons.insert(f.name.clone(), vec![false; f.params.len()]);
        kept_map.insert(f.name.clone(), vec![false; f.params.len()]);
    }
    loop {
        let mut changed = false;
        for f in fns.iter().copied() {
            let facts = fn_facts(f, variants, &ro, &cons);
            let flags = ro.get_mut(&f.name).unwrap();
            let took = cons.get_mut(&f.name).unwrap();
            let kept = kept_map.get_mut(&f.name).unwrap();
            for (i, p) in f.params.iter().enumerate() {
                if flags[i] && facts.escapes(&p.name) {
                    flags[i] = false;
                    changed = true;
                }
                if !took[i] && facts.consumed.contains(&p.name) {
                    took[i] = true;
                    changed = true;
                }
                if !kept[i] && facts.kept.contains(&p.name) {
                    kept[i] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            if cache_key != 0 {
                FIXPOINT_CACHE.with(|c| {
                    *c.borrow_mut() = Some((cache_key, ro.clone(), cons.clone(), kept_map.clone()));
                });
            }
            CONSUMED.with(|c| *c.borrow_mut() = cons);
            KEPT.with(|k| *k.borrow_mut() = kept_map);
            return ro;
        }
    }
}

fn fn_facts(f: &FnDef, variants: &Variants, ro: &ReadOnly, cons: &ReadOnly) -> Facts {
    let mut facts = Facts {
        variants: variants.clone(),
        ro: ro.clone(),
        cons: cons.clone(),
        moved: crate::borrowck::published_moved_locals(&f.name),
        ..Facts::default()
    };
    for p in &f.params {
        facts.bind(&p.name);
        facts.param_tys.insert(p.name.clone(), p.ty.clone());
    }
    block(&f.body, &mut facts);
    propagate_payload_escapes(&mut facts);
    facts
}

/// Builtins whose `String` arguments are only read.
pub(crate) fn borrows_args(name: &str) -> bool {
    matches!(
        name,
        // `x.clone()` reads its receiver through `&self` (SPEC «Memoria»).
        "struct_clone" | "enum_copy" | "list_clone" | "dict_clone" | "bytes_clone" | "clone_copy"
            | "str_len"
            | "str_eq"
            | "str_char"
            | "str_contains"
            | "str_index_of"
            | "str_index_from"
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
            | "list_box_id"
            | "list_get"
            | "list_pop"
            | "desc_copy"
            | "dict_get"
            | "dict_has"
            | "dict_len"
            | "dict_remove"
            | "dict_keys"
            | "dict_values"
            | "enum_owns"
            | "enum_box_id"
            | "list_push_share"
            | "str_share"
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
            | "matmul"
            | "qmatmul"
            | "attention"
            | "relu"
            | "silu"
            | "rms_norm"
            | "rope"
            | "softmax"
            | "argmax"
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
            if returns_fresh(name) || OWNED_FNS.with(|f| f.borrow().contains(name)) {
                Some(("free", false))
            } else if variants.get(name.as_str()).is_some_and(|f| !f.is_empty()) {
                Some(("enum_free", false))
            } else if name == "bytes_new" {
                Some(("bytes_free", false))
            } else if name == "list_new" && list_flat_arg(type_args) {
                Some(("list_free", true))
            } else if name == "list_new" && list_enum_arg(type_args) {
                Some(("list_free", true))
            } else if name == "list_new" && list_elem_args(type_args) {
                Some(("list_free", list_owns_elems(type_args)))
            } else if name == "list_new" && list_glue_arg(type_args) {
                // A list of structs with glue releases each element through `drop.elem.<S>`
                // (`sal_list_set_drop`): the local owns the list like any other (SPEC «Memoria»).
                Some(("list_free", false))
            } else if matches!(name.as_str(), "dict_keys" | "dict_values") {
                // A fresh list whose String elements are copies the list owns.
                Some(("list_free", true))
            } else if name == "dict_get" {
                Some(("enum_free", false))
            } else if name == "bytes_view" || name == "bytes_view_mem" {
                Some(("bytes_free", false))
            } else if name == "dict_new" && dict_args(type_args) {
                Some(("dict_free", dict_owns_values(type_args)))
            } else if name == "list_clone" {
                // `xs.clone()` is a fresh container that owns a copy of every element.
                Some(("list_free", false))
            } else if name == "dict_clone" {
                Some(("dict_free", false))
            } else if name == "bytes_clone" {
                Some(("bytes_free", false))
            } else {
                user_call_kind(name, init).map(|k| (k, false))
            }
        }
        Expr::Binary { op: BinOp::Add, .. } if add_is_string_chain(init) => Some(("free", false)),
        Expr::TensorLit { .. } => Some(("tensor_free", false)),
        Expr::Match { scrutinee, .. } => match scrutinee.as_ref() {
            Expr::Call { func, .. } => {
                matches!(func.as_ref(), Expr::Ident { name, .. } if name == "dict_get")
                    .then_some(("enum_free", false))
            }
            _ => None,
        },
        _ => None,
    }
}

/// `list_new[S]()` with `S` a non-generic struct that has no descriptor: its elements are
/// released by the glue of `S`.
fn list_glue_arg(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, args, .. })]
        if args.is_empty() && !is_ownable_struct(name) && FIELDS.with(|f| f.borrow().contains_key(name)))
}

fn list_flat_arg(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, args, .. })] if args.is_empty() && is_ownable_struct(name))
}

/// The descriptor of a struct list's element (`list_new[P]()`), when the struct is describable.
fn list_flat_desc(type_args: &[TypeArg]) -> Option<String> {
    match type_args {
        [TypeArg::Type(Type::Named { name, args, .. })] if args.is_empty() && is_ownable_struct(name) => {
            STRUCT_DESC.with(|t| t.borrow().get(name).cloned())
        }
        _ => None,
    }
}

fn list_owns_elems(type_args: &[TypeArg]) -> bool {
    matches!(type_args, [TypeArg::Type(Type::Named { name, .. })] if name == "String")
}

fn dict_owns_values(type_args: &[TypeArg]) -> bool {
    match type_args {
        [_, TypeArg::Type(Type::Named { name, args, .. })] if args.is_empty() => {
            matches!(name.as_str(), "String") || is_ownable_enum(name) || is_ownable_struct(name)
        }
        _ => false,
    }
}

fn is_ownable_enum(name: &str) -> bool {
    is_enum_type(name)
}

/// A value whose ownership is certainly the container's once stored: a string
/// literal, a fresh-string call or a `+` string chain. A name or a call to a
/// user function may be someone else's value, so storing it taints the container.
/// A parameter of `String`, a describable struct or an enum. The caller has moved or copied
/// it, so a list that stores it owns that value.
fn is_owned_param(e: &Expr, f: &Facts) -> bool {
    let Expr::Ident { name, .. } = e else {
        return false;
    };
    let Some(Type::Named { name: ty, args, .. }) = f.param_tys.get(name) else {
        return false;
    };
    args.is_empty() && (ty == "String" || is_ownable_struct(ty) || is_ownable_enum(ty))
}

/// `desc_copy` builds a value the destination owns. Storing it does not alias the source.
pub(crate) fn is_desc_copy(e: &Expr) -> bool {
    matches!(e, Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident { name, .. } if name == "desc_copy"))
}

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
        // A list of lists owns its inner lists: its elements carry `sal_list_free_elem`.
        [TypeArg::Type(Type::Named { name, args, .. })] if name == "List" && args.len() == 1 => true,
        [TypeArg::Type(Type::Named { name, args, .. })] => {
            args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool" | "String")
        }
        _ => false,
    }
}

/// `dict_new[K, V]()` with `K` Int or String and `V` a flat scalar, String, enum or describable struct.
fn dict_args(type_args: &[TypeArg]) -> bool {
    let key_ok = |t: &TypeArg| match t {
        TypeArg::Type(Type::Named { name, args, .. }) => {
            args.is_empty() && matches!(name.as_str(), "Int" | "String")
        }
        _ => false,
    };
    let val_ok = |t: &TypeArg| match t {
        TypeArg::Type(Type::Named { name, args, .. }) if args.is_empty() => matches!(
            name.as_str(),
            "Int" | "Float" | "Bool" | "String"
        ) || is_ownable_enum(name) || is_ownable_struct(name),
        _ => false,
    };
    matches!(type_args, [k, v] if key_ok(k) && val_ok(v))
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
    /// Handed out for real (returned, stored, or passed to a callee that consumes it).
    /// A Copy field (`return p.n`) is in `escaped` but not here.
    consumed: HashSet<String>,
    /// The whole value was stored or returned. A Copy field read is not here, so a function
    /// that only reads `p.n` does not keep `p`.
    kept: HashSet<String>,
    /// The argument under `arg` is passed to a callee that consumes it.
    consume_arg: bool,
    /// `consume_arg` is a user function, not a builtin that merely fails `borrows_args`.
    /// A parameter passed whole sits on the soft path, which records an escape but not
    /// consumption; only this flag promotes that parameter so callers take it.
    user_take: bool,
    bindings: HashMap<String, u32>,
    variants: Variants,
    /// Containers that received a value not known to be fresh.
    tainted: HashSet<String>,
    /// `match x` over a local: the names its variant patterns bind.
    matches: Vec<(String, Vec<String>)>,
    ro: ReadOnly,
    cons: ReadOnly,
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
    /// Inside a struct constructor call: value arguments move out fully (not soft).
    hard_move: u32,
    /// Parameter types. Storing one of these in a list moves a value this function owns.
    param_tys: HashMap<String, Type>,
    /// Struct copies (`list_get`) passed to a user fn: only the fields move; the malloc box stays.
    box_shell: HashSet<String>,
    /// The locals the borrow checker says this fn moves (`None`: no program was checked).
    moved: Option<HashSet<String>>,
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

fn arg(a: &Expr, mov: ArgMove, f: &mut Facts) {
    let safe = mov == ArgMove::Read;
    // A field read in a moving position (`return t.name`, `x = t.name`, an argument the callee
    // keeps) hands out a value that aliases the base: the base must not be released.
    if !safe {
        let mut root: &Expr = a;
        let mut is_field = false;
        while let Expr::Field { base, .. } = root {
            root = base;
            is_field = true;
        }
        // A Copy leaf (`return p.n`) aliases nothing on the heap. A non-Copy field
        // (`P(p.name, p.n)`) shares the caller's pointer: the base escapes so this
        // function does not release it, but it is not `kept`. Only the parameter
        // ident itself, stored or returned, makes the caller clone.
        let copy_leaf = is_field && expr_is_copy_scalar(a);
        if let (true, Expr::Ident { name, .. }) = (is_field, root) {
            f.escaped.insert(name.clone());
            if !copy_leaf {
                f.consumed.insert(name.clone());
            }
        }
    }
    if let Expr::Ident { name, .. } = a {
        if !safe {
            f.kept.insert(name.clone());
            if f.hard_move > 0 {
                f.escaped.insert(name.clone());
                f.consumed.insert(name.clone());
                f.soft.remove(name);
            } else {
                match f.cur {
                    Some(key) if f.bind_depth.get(name).is_some_and(|d| *d == f.loop_depth) => {
                        f.soft.insert(name.clone());
                        if f.user_take {
                            f.consumed.insert(name.clone());
                        }
                        if mov == ArgMove::TakeFields {
                            f.box_shell.insert(name.clone());
                        }
                        let v = f.sites.entry(key).or_default();
                        if !v.contains(name) {
                            v.push(name.clone());
                        }
                    }
                    _ => {
                        f.escaped.insert(name.clone());
                        if f.consume_arg || f.hard_move > 0 {
                            f.consumed.insert(name.clone());
                        }
                        if mov == ArgMove::TakeFields {
                            f.box_shell.insert(name.clone());
                        }
                    }
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
            f.consumed.insert(name.clone());
        }
        Expr::Call { func, args, .. } => {
            let borrow = matches!(func.as_ref(), Expr::Ident { name, .. } if borrows_args(name));
            // Constructor arguments are hard moves. Names of another module are qualified (`mod__P`).
            let hard = matches!(func.as_ref(), Expr::Ident { name, .. }
                if is_ownable_struct(name) || is_ownable_struct(name.rsplit("__").next().unwrap_or(name)));
            if hard {
                f.hard_move += 1;
            }
            if !matches!(func.as_ref(), Expr::Ident { .. }) {
                expr(func, f);
            }
            let ro = match func.as_ref() {
                Expr::Ident { name, .. } => f.ro.get(name).cloned(),
                _ => None,
            };
            let callee = match func.as_ref() {
                Expr::Ident { name, .. } => name.as_str(),
                _ => "",
            };
            if callee == "list_push_share" {
                if let Some(Expr::Ident { name: xs, .. }) = args.first() {
                    f.tainted.insert(xs.clone());
                }
            }
            // `str_as_int(&s)` stores the string as a handle. The local must not be
            // released: the handle is what later reads it.
            if callee == "str_as_int" {
                for a in args {
                    let mut cur = a;
                    while let Expr::Unary { expr, .. } = cur {
                        cur = expr;
                    }
                    if let Expr::Ident { name, .. } = cur {
                        f.escaped.insert(name.clone());
                        f.soft.remove(name);
                    }
                }
            }
            for (i, a) in args.iter().enumerate() {
                let reads = ro.as_ref().and_then(|r| r.get(i)).copied().unwrap_or(false);
                let user_fn = f.cons.contains_key(callee);
                let user_takes = f.cons.get(callee).and_then(|c| c.get(i)).copied().unwrap_or(false);
                // `enum_set(e, i, v)` borrows the box and moves `v` into a slot (runtime
                // stores the handle; freeing `v` afterwards is use-after-free).
                let enum_set_take = callee == "enum_set" && i == 2;
                let enum_set_borrow = callee == "enum_set" && i != 2;
                f.consume_arg = if enum_set_take {
                    true
                } else if enum_set_borrow {
                    false
                } else if user_fn {
                    user_takes
                } else {
                    !borrow
                };
                f.user_take = user_fn && user_takes;
                let mov = if enum_set_take {
                    ArgMove::TransferBox
                } else if enum_set_borrow || borrow || reads {
                    ArgMove::Read
                } else if hard || callee_transfers_struct_box(callee) {
                    ArgMove::TransferBox
                } else {
                    ArgMove::TakeFields
                };
                arg(a, mov, f);
                f.consume_arg = false;
                f.user_take = false;
            }
            if hard {
                f.hard_move -= 1;
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
            arg(left, ArgMove::Read, f);
            arg(right, ArgMove::Read, f);
        }
        Expr::Unary { expr: x, .. } => arg(x, ArgMove::Read, f),
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
        Expr::To { expr: x, .. } => arg(x, ArgMove::TakeFields, f),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    arg(c, ArgMove::TakeFields, f);
                }
            }
        }
        Expr::Field { base, .. } => arg(base, ArgMove::Read, f),
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
            arg(
                scrutinee,
                if binds_whole { ArgMove::TakeFields } else { ArgMove::Read },
                f,
            );
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
        Expr::Try { expr: x, .. } => arg(x, ArgMove::TakeFields, f),
    }
}

/// The branches of an `if`; `xform_block` visits them when the `if` is a whole statement
/// expression, a `let` or assignment value, or a block tail.
fn walk_if(e: &Expr, f: &mut Facts) {
    let Expr::If { cond, then_block, elsifs, else_block, .. } = e else {
        return;
    };
    arg(cond, ArgMove::Read, f);
    block(then_block, f);
    for a in elsifs {
        arg(&a.cond, ArgMove::Read, f);
        block(&a.body, f);
    }
    if let Some(b) = else_block {
        block(b, f);
    }
}

/// An expression in a position `xform_expr` rewrites.
fn top(e: &Expr, mov: ArgMove, f: &mut Facts) {
    if matches!(e, Expr::If { .. }) {
        walk_if(e, f);
    } else {
        arg(e, mov, f);
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
            if !f.consumed.contains(scrut) && bound.iter().any(|b| f.consumed.contains(b)) {
                f.consumed.insert(scrut.clone());
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
        Pattern::Wild(_) | Pattern::Int(_, _) | Pattern::Str(_, _) | Pattern::Regex(_, _) => {}
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
        if args.len() >= 2
            && !is_fresh_value(v)
            && !is_desc_copy(v)
            && !is_fresh_struct_ctor(v, &f.variants)
            && !is_fresh_variant_ctor(v, &f.variants)
            && !is_owned_param(v, f)
        {
            f.tainted.insert(container.clone());
        }
    }
    for a in &args[1..] {
        arg(a, ArgMove::TransferBox, f);
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
                top(init, ArgMove::TakeFields, f);
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
                    top(value, ArgMove::TakeFields, f);
                }
            }
            Stmt::Return { value, span } => {
                f.cur = live.then(|| stmt_key(*span));
                if let Some(v) = value {
                    arg(v, ArgMove::TakeFields, f);
                }
            }
            Stmt::While { cond, body, .. } => {
                f.cur = None;
                arg(cond, ArgMove::Read, f);
                f.loop_depth += 1;
                block(body, f);
                f.loop_depth -= 1;
            }
        }
        f.cur = outer;
    }
    if let Some(t) = &b.tail {
        let outer = f.cur;
        // A plain tail is a statement of its own for the moves it makes (its flag is cleared
        // before it runs). A tail `if`/`match` keys its moves in its branches instead.
        let live = f.opaque == 0;
        f.cur = (live && is_call_tail(t)).then(|| stmt_key(t.span()));
        top(t, ArgMove::TakeFields, f);
        f.cur = outer;
    }
}

fn is_control_expr(e: &Expr) -> bool {
    matches!(e, Expr::If { .. } | Expr::Match { .. })
}

/// A tail that calls a function: the arguments it moves are its own statement's moves.
fn is_call_tail(e: &Expr) -> bool {
    // Struct and variant constructors are calls too, but they build a value, not a call that
    // moves its arguments: their names start with an uppercase letter.
    matches!(e, Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident { name, .. }
        if name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')))
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
    /// Locals built as `list_new_owned[E]()`: enum lists the pass releases (see `own_enum_list`).
    owned_elems: HashMap<String, &'static str>,
    /// Struct copies whose box was freed right after a scalarizing call (`clear_moved`).
    shell_freed: HashSet<String>,
    /// Locals bound more than once whose every binding is a fresh value of one kind: a
    /// reassignment releases the old value first (SPEC «Liberación»).
    rebound: HashSet<String>,
}

/// Every `name = init` of `b` (declarations and reassignments, not the in-place `x = x + …`).
fn binding_inits<'a>(b: &'a Block, out: &mut HashMap<String, Vec<&'a Expr>>) {
    fn in_expr<'a>(e: &'a Expr, out: &mut HashMap<String, Vec<&'a Expr>>) {
        match e {
            Expr::Block(b) | Expr::On { body: b, .. } => binding_inits(b, out),
            Expr::If { then_block, elsifs, else_block, .. } => {
                binding_inits(then_block, out);
                for el in elsifs {
                    binding_inits(&el.body, out);
                }
                if let Some(b) = else_block {
                    binding_inits(b, out);
                }
            }
            Expr::Match { arms, .. } => {
                for a in arms {
                    in_expr(&a.body, out);
                }
            }
            _ => {}
        }
    }
    for st in &b.stmts {
        match st {
            Stmt::Let { name, init, .. } => {
                out.entry(name.clone()).or_default().push(init);
                in_expr(init, out);
            }
            Stmt::Assign { target, value, .. } => {
                if let Expr::Ident { name, .. } = target {
                    if !is_accumulate(name, value) {
                        out.entry(name.clone()).or_default().push(value);
                    }
                }
                in_expr(value, out);
            }
            Stmt::Expr(e) => in_expr(e, out),
            Stmt::While { body, .. } => binding_inits(body, out),
            Stmt::Return { value: Some(v), .. } => in_expr(v, out),
            Stmt::Return { .. } | Stmt::Break { .. } => {}
        }
    }
    if let Some(t) = &b.tail {
        in_expr(t, out);
    }
}

fn rebound_locals(body: &Block, variants: &Variants) -> HashSet<String> {
    let mut inits = HashMap::new();
    binding_inits(body, &mut inits);
    inits
        .into_iter()
        .filter(|(name, es)| {
            if es.len() < 2 {
                return false;
            }
            let Some((k, _)) = fresh_kind(es[0], variants) else { return false };
            matches!(k, "free" | "list_free" | "dict_free" | "bytes_free")
                && es.iter().all(|e| {
                    struct_copy_kind(name, e).is_none()
                        && !matches!(e, Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident { name, .. } if name == "list_pop"))
                        && fresh_kind(e, variants).is_some_and(|(k2, _)| k2 == k)
                })
        })
        .map(|(n, _)| n)
        .collect()
}

/// An enum list the pass releases is built as `list_new_owned`, so the lowering stores the
/// element descriptor `E` and the list frees its boxes. Other enum lists keep `list_new`:
/// their elements are borrowed, and `list_free` must not release them.
fn own_enum_list(name: &str, released: bool, init: &mut Expr, owned: &mut HashMap<String, &'static str>) {
    if !released {
        return;
    }
    if let Expr::Call { func, type_args, .. } = init {
        if let Expr::Ident { name: callee, .. } = func.as_mut() {
            if callee == "list_new" && (list_enum_arg(type_args) || list_flat_arg(type_args) || list_string_arg(type_args)) {
                *callee = "list_new_owned".into();
                // What an owned list hands out (`list_get`, `list_pop`) is a fresh value of its
                // element kind: a box (`enum_free`) or a string (`free`). Struct elements are
                // not tracked here.
                if list_enum_arg(type_args) {
                    owned.insert(name.to_string(), "enum_free");
                } else if list_string_arg(type_args) {
                    owned.insert(name.to_string(), "free");
                } else if let Some(desc) = list_flat_desc(type_args) {
                    // A struct element moved out by `list_pop`, or copied by `list_get`, is a
                    // box the local releases with `struct_free` (see `struct_copy_kind`).
                    owned.insert(name.to_string(), Box::leak(format!("struct:{desc}").into_boxed_str()));
                }
            }
        }
    }
}

impl Ctx {
    /// The release of a value an expression statement discards: a fresh value (see
    /// `fresh_kind`), a copy `list_get` hands out, or an element an owned list gives up
    /// (`list_pop`). `None` when the statement keeps nothing to release.
    fn discard_kind(&self, e: &Expr) -> Option<&'static str> {
        if let Expr::Call { func, args, .. } = e {
            if matches!(func.as_ref(), Expr::Ident { name, .. } if name == "list_pop") {
                if let Some(Expr::Ident { name: xs, .. }) = args.first() {
                    if self.facts.bindings.get(xs) == Some(&1) {
                        if let Some(k) = self.owned_elems.get(xs) {
                            return Some(*k);
                        }
                    }
                }
            }
        }
        if let Some(k) = struct_copy_kind("", e) {
            return Some(k);
        }
        if let Expr::Match { scrutinee, .. } = e {
            if matches!(
                scrutinee.as_ref(),
                Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident { name, .. } if name == "dict_get")
            ) {
                return Some("enum_free");
            }
        }
        fresh_kind(e, &self.facts.variants).map(|(k, _)| k)
    }
}

impl Ctx {
    /// The release function for a droppable local, if `name = init` declares one.
    fn candidate(&self, name: &str, ty: &Option<Type>, init: &Expr) -> Option<(&'static str, bool)> {
        // `t = s` moves an owned local into `t`: `t` is the new owner of what `s` released (its
        // flag is cleared by that move, so `s` no longer releases it).
        if let Expr::Ident { name: src, .. } = init {
            if let Some((_, k, _)) = self.live.iter().rev().find(|(l, _, _)| l == src) {
                let ok = !self.params.contains(name)
                    && self.facts.bindings.get(name) == Some(&1)
                    && (!self.facts.escaped.contains(name) || self.facts.box_shell.contains(name));
                if !ok {
                    return None;
                }
                if self.facts.moved.as_ref().is_some_and(|m| m.contains(name)) && !self.facts.soft.contains(name) {
                    return None;
                }
                return Some((*k, self.facts.soft.contains(name)));
            }
        }
        if let Some(Type::Named { name: n, args, .. }) = ty {
            if n == "any" && !args.is_empty() {
                let flagged = self.facts.bindings.get(name) == Some(&1)
                    && (!self.facts.escaped.contains(name) || self.facts.box_shell.contains(name));
                if !self.params.contains(name) && flagged {
                    return Some(("any_free", true));
                }
            }
        }
        // `list_pop(xs)` moves the element out of the list: a fresh value of the owned
        // list's element kind. `list_get` borrows and is not released here.
        let elem_release = match init {
            Expr::Call { func, args, .. }
                if matches!(func.as_ref(), Expr::Ident { name, .. } if name == "list_pop") =>
            {
                match args.first() {
                    Some(Expr::Ident { name: xs, .. }) if self.facts.bindings.get(xs) == Some(&1) => {
                        self.owned_elems.get(xs).copied()
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let (kind, owned) = match elem_release.or_else(|| struct_copy_kind(name, init)) {
            Some(k) => (k, false),
            None => fresh_kind(init, &self.facts.variants)?,
        };
        // SPEC «Memoria»: a moved local is never released at scope end (the new owner has it).
        // A taint from a non-fresh store only holds while the moves are not known.
        // A local moved on some path is released only where its liveness flag says it still owns
        // the value (`soft`, SPEC «Liberación de locales»: the move clears the flag).
        if self.facts.moved.as_ref().is_some_and(|m| m.contains(name)) && !self.facts.soft.contains(name) {
            return None;
        }
        if owned && self.facts.tainted.contains(name) && self.facts.moved.is_none() {
            return None;
        }
        if let Some(t) = ty {
            if kind != "free" || !crate::string_expr::is_string_type(t) {
                return None;
            }
        }
        let ok = !self.params.contains(name)
            && (self.facts.bindings.get(name) == Some(&1) || self.rebound.contains(name))
            && (!self.facts.escaped.contains(name) || self.facts.box_shell.contains(name));
        ok.then_some((kind, self.facts.soft.contains(name)))
    }
}

fn is_copy_scalar(t: &Type) -> bool {
    matches!(t, Type::Named { name, args, .. } if args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool"))
}

/// The expression's inferred type is `Int`, `Float`, or `Bool`. Unknown types are not Copy.
fn expr_is_copy_scalar(e: &Expr) -> bool {
    let sp = e.span();
    EXPR_TYPES.with(|t| {
        t.borrow()
            .get(&(sp.start, sp.end))
            .is_some_and(is_copy_scalar)
    })
}

/// Locals bound to a `list_get` copy of a describable struct (for box-shell release).
fn mark_box_shell_calls_expr(e: &Expr, copies: &HashMap<String, &'static str>, facts: &mut Facts) {
    match e {
        Expr::Call { func, args, .. } => {
            let user = matches!(func.as_ref(), Expr::Ident { name, .. }
                if !borrows_args(name) && !callee_transfers_struct_box(name));
            if user {
                for a in args {
                    if let Expr::Ident { name, .. } = a {
                        if copies.contains_key(name) {
                            facts.box_shell.insert(name.clone());
                        }
                    }
                }
            }
            if !matches!(func.as_ref(), Expr::Ident { .. }) {
                mark_box_shell_calls_expr(func, copies, facts);
            }
            for a in args {
                mark_box_shell_calls_expr(a, copies, facts);
            }
        }
        Expr::Binary { left, right, .. } => {
            mark_box_shell_calls_expr(left, copies, facts);
            mark_box_shell_calls_expr(right, copies, facts);
        }
        Expr::Unary { expr, .. } => mark_box_shell_calls_expr(expr, copies, facts),
        Expr::Field { base, .. } => mark_box_shell_calls_expr(base, copies, facts),
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            mark_box_shell_calls_expr(cond, copies, facts);
            mark_box_shell_calls(then_block, copies, facts);
            for e in elsifs {
                mark_box_shell_calls_expr(&e.cond, copies, facts);
                mark_box_shell_calls(&e.body, copies, facts);
            }
            if let Some(b) = else_block {
                mark_box_shell_calls(b, copies, facts);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            mark_box_shell_calls_expr(scrutinee, copies, facts);
            for a in arms {
                mark_box_shell_calls_expr(&a.body, copies, facts);
            }
        }
        Expr::Block(b) => mark_box_shell_calls(b, copies, facts),
        Expr::Lambda { body, .. } => mark_box_shell_calls_expr(body, copies, facts),
        Expr::On { body, .. } => mark_box_shell_calls(body, copies, facts),
        Expr::To { expr, .. } => mark_box_shell_calls_expr(expr, copies, facts),
        Expr::Try { expr, .. } => mark_box_shell_calls_expr(expr, copies, facts),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for el in row {
                    mark_box_shell_calls_expr(el, copies, facts);
                }
            }
        }
        _ => {}
    }
}

fn mark_box_shell_calls(b: &Block, copies: &HashMap<String, &'static str>, facts: &mut Facts) {
    for st in &b.stmts {
        match st {
            Stmt::Expr(e) => mark_box_shell_calls_expr(e, copies, facts),
            Stmt::While { body, .. } => mark_box_shell_calls(body, copies, facts),
            Stmt::Let { init, .. } | Stmt::Assign { value: init, .. } => {
                mark_box_shell_calls_expr(init, copies, facts);
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        mark_box_shell_calls_expr(t, copies, facts);
    }
}

fn struct_copy_locals(b: &Block, elems: &HashMap<String, String>, out: &mut HashMap<String, &'static str>) {
    for st in &b.stmts {
        match st {
            Stmt::Let { name, init, .. } | Stmt::Assign { target: Expr::Ident { name, .. }, value: init, .. } => {
                if let Some(k) = struct_copy_kind(name, init)
                    .or_else(|| struct_copy_from_list(init, elems))
                    .or_else(|| struct_copy_from_types(init))
                {
                    out.insert(name.clone(), k);
                }
            }
            Stmt::While { body, .. } => struct_copy_locals(body, elems, out),
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        if let Expr::Block(b) = t.as_ref() {
            struct_copy_locals(b, elems, out);
        }
    }
}

fn struct_copy_from_types(init: &Expr) -> Option<&'static str> {
    let Expr::Call { func, span, .. } = init else {
        return None;
    };
    if !matches!(func.as_ref(), Expr::Ident { name, .. } if name == "list_pop") {
        return None;
    }
    let ty = EXPR_TYPES.with(|t| t.borrow().get(&(span.start, span.end)).cloned())?;
    let Type::Named { name: sn, args, .. } = ty else {
        return None;
    };
    if !args.is_empty() || !is_ownable_struct(&sn) {
        return None;
    }
    let desc = STRUCT_DESC.with(|t| t.borrow().get(&sn).cloned())?;
    Some(Box::leak(format!("struct:{desc}").into_boxed_str()))
}

fn struct_copy_from_list(init: &Expr, elems: &HashMap<String, String>) -> Option<&'static str> {
    let Expr::Call { func, args, .. } = init else {
        return None;
    };
    if !matches!(func.as_ref(), Expr::Ident { name, .. } if name == "list_pop") {
        return None;
    }
    let Expr::Ident { name: xs, .. } = args.first()? else {
        return None;
    };
    let sn = elems.get(xs)?;
    let desc = STRUCT_DESC.with(|t| t.borrow().get(sn).cloned())?;
    Some(Box::leak(format!("struct:{desc}").into_boxed_str()))
}

#[derive(Clone)]
struct Borr {
    desc: String,
    /// Struct name when the borrowed value is a struct; empty for `String` and enum.
    sn: String,
}

fn desc_of_type(t: &Type) -> Option<Borr> {
    // `list_get` types its element as `&T`: the descriptor is the one of `T`.
    let mut t = t;
    while let Type::Ref { inner, .. } = t {
        t = inner;
    }
    let Type::Named { name, args, .. } = t else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    match name.as_str() {
        "String" => Some(Borr { desc: "s".into(), sn: String::new() }),
        "Int" | "Float" | "Bool" | "Unit" => None,
        n if is_ownable_struct(n) => {
            let desc = STRUCT_DESC.with(|t| t.borrow().get(n).cloned())?;
            Some(Borr { desc, sn: n.to_string() })
        }
        n if is_enum_type(n) => Some(Borr { desc: "E".into(), sn: String::new() }),
        _ => None,
    }
}

fn type_at(e: &Expr) -> Option<Type> {
    let sp = e.span();
    EXPR_TYPES.with(|t| t.borrow().get(&(sp.start, sp.end)).cloned())
}

fn field_borr(sn: &str, field: &str) -> Option<Borr> {
    FIELDS.with(|f| {
        let fields = f.borrow().get(sn)?.clone();
        let ty = fields.into_iter().find(|(n, _)| n == field)?.1;
        desc_of_type(&ty)
    })
}

/// A field of a borrowed struct, including a Copy leaf the descriptor does not name.
fn field_of_borrow(e: &Expr, borrows: &HashMap<String, Borr>) -> bool {
    let Expr::Field { base, .. } = e else {
        return false;
    };
    borrow_of(base, borrows).is_some_and(|b| !b.sn.is_empty())
}

/// An argument a struct constructor may share: a borrow, a Copy field of one, or a Copy literal.
fn arg_shares(e: &Expr, borrows: &HashMap<String, Borr>) -> bool {
    borrow_of(e, borrows).is_some() || expr_is_copy_scalar(e) || field_of_borrow(e, borrows)
}

/// `Nombre(…)` built only from borrowed fields. The result aliases those pointers.
fn ctor_is_alias(name: &str, args: &[Expr], borrows: &HashMap<String, Borr>) -> bool {
    is_ownable_struct(name)
        && args.iter().all(|a| arg_shares(a, borrows))
        && args.iter().any(|a| borrow_of(a, borrows).is_some() || field_of_borrow(a, borrows))
}

/// A `list_get`, a local bound to one, a field of such a struct, a constructor of those
/// fields, or a call that returns them without taking the struct.
fn borrow_of(e: &Expr, borrows: &HashMap<String, Borr>) -> Option<Borr> {
    match e {
        Expr::Call { func, args, .. } => {
            let Expr::Ident { name, .. } = func.as_ref() else {
                return None;
            };
            if name == "list_get" {
                if let Some(Expr::Ident { name: xs, .. }) = args.first() {
                    if SHARED_LISTS.with(|s| s.borrow().contains(xs)) {
                        return None;
                    }
                }
                return desc_of_type(&type_at(e)?);
            }
            if ctor_is_alias(name, args, borrows) {
                let desc = STRUCT_DESC.with(|t| t.borrow().get(name).cloned())?;
                return Some(Borr { desc, sn: name.clone() });
            }
            if borrows_args(name) || is_ownable_struct(name) || is_variant_ctor(name) {
                return None;
            }
            // An explicit copy is a fresh value: it owns everything it holds.
            if matches!(name.as_str(), "strdup" | "struct_clone" | "enum_copy" | "list_clone" | "dict_clone" | "bytes_clone" | "clone_copy" | "desc_copy") {
                return None;
            }
            let shares = args.iter().enumerate().any(|(i, a)| {
                !user_param_owns(name, i) && (borrow_of(a, borrows).is_some() || field_of_borrow(a, borrows))
            });
            if shares {
                desc_of_type(&type_at(e)?)
            } else {
                None
            }
        }
        Expr::Ident { name, .. } => borrows.get(name).cloned(),
        Expr::Field { base, field, .. } => {
            let b = borrow_of(base, borrows)?;
            if b.sn.is_empty() {
                None
            } else {
                field_borr(&b.sn, field)
            }
        }
        _ => None,
    }
}

/// A user parameter stored or returned as a whole (`list_push(ys, p)`, `return p`).
/// Reading or recomposing its fields does not. Unknown callees are not user functions.
pub(crate) fn user_param_owns(callee: &str, index: usize) -> bool {
    KEPT.with(|k| k.borrow().get(callee).and_then(|v| v.get(index).copied()).unwrap_or(false))
}

fn arg_is_owning(callee: &str, index: usize, variants: &Variants) -> bool {
    match callee {
        "list_push" => index == 1,
        "list_set" => index == 2,
        "dict_put" => index == 2,
        "desc_copy" => false,
        _ if borrows_args(callee) => false,
        // A struct constructor shares borrowed fields. The copy, if any, is of the
        // finished value when a second owner (`list_push`, a variant) takes it.
        _ if is_ownable_struct(callee) => false,
        _ if variants.contains_key(callee) => true,
        _ => user_param_owns(callee, index),
    }
}

fn claim_expr(e: Expr, own: bool, borrows: &mut HashMap<String, Borr>, variants: &Variants) -> Expr {
    // SPEC «Memoria»: no implicit copy. A borrowed value an owner takes is `E_TYPE`/`E_BORROW`
    // upstream; the source says `x.clone()`.
    let _ = own;
    match e {
        Expr::Call { func, type_args, args, span } => {
            let callee = match func.as_ref() {
                Expr::Ident { name, .. } => name.clone(),
                _ => String::new(),
            };
            let func = if matches!(func.as_ref(), Expr::Ident { .. }) {
                func
            } else {
                Box::new(claim_expr(*func, false, borrows, variants))
            };
            let pure = ctor_is_alias(&callee, &args, borrows);
            let args = args
                .into_iter()
                .enumerate()
                .map(|(i, a)| {
                    let own_arg = if is_ownable_struct(&callee) {
                        // A constructor of only borrowed fields shares them. Any other
                        // borrowed argument is copied so the result owns every child.
                        !pure && borrow_of(&a, borrows).is_some()
                    } else {
                        arg_is_owning(&callee, i, variants)
                    };
                    claim_expr(a, own_arg, borrows, variants)
                })
                .collect();
            Expr::Call { func, type_args, args, span }
        }
        Expr::Binary { op, left, right, span } => Expr::Binary {
            op,
            left: Box::new(claim_expr(*left, false, borrows, variants)),
            right: Box::new(claim_expr(*right, false, borrows, variants)),
            span,
        },
        Expr::Unary { op, expr, span } => Expr::Unary {
            op,
            expr: Box::new(claim_expr(*expr, false, borrows, variants)),
            span,
        },
        Expr::Field { base, field, span } => Expr::Field {
            base: Box::new(claim_expr(*base, false, borrows, variants)),
            field,
            span,
        },
        Expr::If { cond, mut then_block, elsifs, else_block, span } => {
            let cond = Box::new(claim_expr(*cond, false, borrows, variants));
            claim_block(&mut then_block, own, borrows, variants);
            let elsifs = elsifs
                .into_iter()
                .map(|mut a| {
                    a.cond = claim_expr(a.cond, false, borrows, variants);
                    claim_block(&mut a.body, own, borrows, variants);
                    a
                })
                .collect();
            let else_block = else_block.map(|mut b| {
                claim_block(&mut b, own, borrows, variants);
                b
            });
            Expr::If { cond, then_block, elsifs, else_block, span }
        }
        Expr::Match { scrutinee, arms, span } => Expr::Match {
            scrutinee: Box::new(claim_expr(*scrutinee, false, borrows, variants)),
            arms: arms
                .into_iter()
                .map(|a| MatchArm {
                    body: claim_expr(a.body, own, borrows, variants),
                    pattern: a.pattern,
                    span: a.span,
                })
                .collect(),
            span,
        },
        Expr::Block(mut b) => {
            let mut inner = borrows.clone();
            claim_block(&mut b, own, &mut inner, variants);
            Expr::Block(b)
        }
        Expr::Lambda { params, body, span } => {
            let mut inner = borrows.clone();
            for p in &params {
                inner.remove(p);
            }
            Expr::Lambda {
                body: Box::new(claim_expr(*body, true, &mut inner, variants)),
                params,
                span,
            }
        }
        Expr::On { place, kernel, kernel_index, mut body, span } => {
            claim_block(&mut body, false, borrows, variants);
            Expr::On { place, kernel, kernel_index, body, span }
        }
        Expr::To { place, expr, span } => Expr::To {
            place,
            expr: Box::new(claim_expr(*expr, true, borrows, variants)),
            span,
        },
        Expr::Try { expr, span } => Expr::Try {
            expr: Box::new(claim_expr(*expr, true, borrows, variants)),
            span,
        },
        Expr::TensorLit { rows, span } => Expr::TensorLit {
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(|c| claim_expr(c, false, borrows, variants)).collect())
                .collect(),
            span,
        },
        other => other,
    }
}

fn note_borrow(name: &str, init: &Expr, borrows: &mut HashMap<String, Borr>) {
    if let Some(b) = borrow_of(init, borrows) {
        borrows.insert(name.to_string(), b);
    } else {
        borrows.remove(name);
    }
}

fn claim_stmt(st: Stmt, borrows: &mut HashMap<String, Borr>, variants: &Variants) -> Stmt {
    match st {
        Stmt::Let { name, ty, init, frozen, stack, span } => {
            let init = claim_expr(init, false, borrows, variants);
            note_borrow(&name, &init, borrows);
            Stmt::Let { name, ty, init, frozen, stack, span }
        }
        Stmt::Assign { target, value, span } => match target {
            Expr::Ident { name, span: ns } => {
                let value = claim_expr(value, false, borrows, variants);
                note_borrow(&name, &value, borrows);
                Stmt::Assign {
                    target: Expr::Ident { name, span: ns },
                    value,
                    span,
                }
            }
            target => Stmt::Assign {
                target,
                value: claim_expr(value, true, borrows, variants),
                span,
            },
        },
        Stmt::Return { value, span } => Stmt::Return {
            value: value.map(|v| claim_expr(v, true, borrows, variants)),
            span,
        },
        Stmt::While { cond, mut body, span } => {
            let cond = claim_expr(cond, false, borrows, variants);
            claim_block(&mut body, false, borrows, variants);
            Stmt::While { cond, body, span }
        }
        Stmt::Expr(e) => Stmt::Expr(claim_expr(e, false, borrows, variants)),
        other => other,
    }
}

fn claim_block(b: &mut Block, own_tail: bool, borrows: &mut HashMap<String, Borr>, variants: &Variants) {
    let mut inner = borrows.clone();
    let stmts = std::mem::take(&mut b.stmts);
    b.stmts = stmts.into_iter().map(|st| claim_stmt(st, &mut inner, variants)).collect();
    if let Some(t) = b.tail.take() {
        b.tail = Some(Box::new(claim_expr(*t, own_tail, &mut inner, variants)));
    }
}

fn shared_list_names(f: &FnDef) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_shared_block(&f.body, &mut out);
    out
}

fn collect_shared_block(b: &Block, out: &mut HashSet<String>) {
    for st in &b.stmts {
        match st {
            Stmt::Let { init, .. } => collect_shared_expr(init, out),
            Stmt::Expr(e) => collect_shared_expr(e, out),
            Stmt::Assign { target, value, .. } => {
                collect_shared_expr(target, out);
                collect_shared_expr(value, out);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    collect_shared_expr(v, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                collect_shared_expr(cond, out);
                collect_shared_block(body, out);
            }
            Stmt::Break { .. } => {}
        }
    }
    if let Some(t) = &b.tail {
        collect_shared_expr(t, out);
    }
}

fn collect_shared_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } | Expr::Ident { .. } => {}
        Expr::Call { func, args, .. } => {
            if let (Expr::Ident { name, .. }, Some(Expr::Ident { name: xs, .. })) = (func.as_ref(), args.first()) {
                if name == "list_push_share" {
                    out.insert(xs.clone());
                }
            }
            collect_shared_expr(func, out);
            for a in args {
                collect_shared_expr(a, out);
            }
        }
        Expr::Lambda { body, .. } => collect_shared_expr(body, out),
        Expr::Binary { left, right, .. } => {
            collect_shared_expr(left, out);
            collect_shared_expr(right, out);
        }
        Expr::Unary { expr, .. } | Expr::To { expr, .. } | Expr::Try { expr, .. } | Expr::Field { base: expr, .. } => {
            collect_shared_expr(expr, out);
        }
        Expr::Block(b) => collect_shared_block(b, out),
        Expr::On { body, .. } => collect_shared_block(body, out),
        Expr::TensorLit { rows, .. } => {
            for row in rows {
                for c in row {
                    collect_shared_expr(c, out);
                }
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            collect_shared_expr(scrutinee, out);
            for a in arms {
                collect_shared_expr(&a.body, out);
            }
        }
        Expr::If { cond, then_block, elsifs, else_block, .. } => {
            collect_shared_expr(cond, out);
            collect_shared_block(then_block, out);
            for a in elsifs {
                collect_shared_expr(&a.cond, out);
                collect_shared_block(&a.body, out);
            }
            if let Some(b) = else_block {
                collect_shared_block(b, out);
            }
        }
    }
}

fn drop_fn(f: &mut FnDef, variants: &Variants, ro: &ReadOnly) {
    SHARED_LISTS.with(|s| *s.borrow_mut() = shared_list_names(f));
    let mut borrows = HashMap::new();
    claim_block(&mut f.body, true, &mut borrows, variants);
    let mut facts = fn_facts(f, variants, ro, &ReadOnly::new());
    let elems = list_elem_structs(f);
    let mut copy_locals = HashMap::new();
    struct_copy_locals(&f.body, &elems, &mut copy_locals);
    mark_box_shell_calls(&f.body, &copy_locals, &mut facts);
    let mut cx = Ctx {
        facts,
        params: f.params.iter().map(|p| p.name.clone()).collect(),
        live: Vec::new(),
        tmp: 0,
        owned_elems: HashMap::new(),
        shell_freed: HashSet::new(),
        rebound: rebound_locals(&f.body, variants),
    };
    // A by-value `String`/`List`/`Dict` parameter moves into the fn: the fn releases it at
    // scope end unless it moved it. Without the moved set there is nothing to decide by.
    let mark = cx.live.len();
    // A parameter moved on some path is released behind its flag, set on entry (see `soft`).
    let mut flags: Vec<String> = Vec::new();
    if let Some(moved) = cx.facts.moved.clone() {
        for p in &f.params {
            if p.mode != ParamMode::Take {
                continue;
            }
            let tracked = cx.facts.soft.contains(&p.name);
            if moved.contains(&p.name) && !tracked {
                continue;
            }
            if let Some(k) = owned_param_release(&p.ty) {
                cx.live.push((p.name.clone(), k, tracked));
                if tracked {
                    flags.push(p.name.clone());
                }
            }
        }
    }
    let span = f.body.span;
    let body = std::mem::replace(&mut f.body, Block { stmts: vec![], tail: None, span });
    let mut body = xform_block_scope(body, &mut cx, mark);
    if !flags.is_empty() {
        let mut init: Vec<Stmt> = flags.iter().map(|n| flag_init(n, span)).collect();
        init.append(&mut body.stmts);
        body.stmts = init;
    }
    f.body = body;
}

/// The release of a by-value parameter of this type, if the drops pass owns one.
fn owned_param_release(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Named { name, args, .. } if args.is_empty() && name == "String" => Some("free"),
        Type::Named { name, args, .. } if name == "List" && args.len() == 1 => Some("list_free"),
        Type::Named { name, args, .. } if name == "Dict" && args.len() == 2 => Some("dict_free"),
        Type::Named { name, args, .. } if args.is_empty() && is_ownable_enum(name) => Some("enum_free"),
        _ => None,
    }
}

fn free_call(name: &str, release: &str, span: Span) -> Stmt {
    if let Some(desc) = release.strip_prefix("struct:") {
        // `struct_free(copy, desc)`: the copy's fields and its box.
        return Stmt::Expr(Expr::Call {
            func: Box::new(Expr::Ident { name: "struct_free".into(), span }),
            type_args: vec![],
            args: vec![
                Expr::Ident { name: name.into(), span },
                Expr::String { value: desc.into(), parts: vec![], span },
            ],
            span,
        });
    }
    Stmt::Expr(Expr::Call {
        func: Box::new(Expr::Ident { name: release.into(), span }),
        type_args: vec![],
        args: vec![Expr::Ident { name: name.into(), span }],
        span,
    })
}

/// Frees only the malloc block of a struct copy after its fields were scalarized into a call.
fn struct_box_free_call(name: &str, span: Span) -> Stmt {
    Stmt::Expr(Expr::Call {
        func: Box::new(Expr::Ident { name: "struct_free_box".into(), span }),
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

/// `__live_x = 1` after a reassignment of a flagged local (the flag is already declared).
fn flag_set(name: &str, span: Span) -> Stmt {
    Stmt::Assign {
        target: Expr::Ident { name: flag_name(name), span },
        value: int_expr(1, span),
        span,
    }
}

/// `name = value` over a live owned local: the new value is computed first (it may read the
/// old one), then the old value is released, then the name takes the new one.
fn rebind(name: &str, value: Expr, span: Span, cx: &mut Ctx, stmts: &mut Vec<Stmt>) -> bool {
    let Some((_, k, fl)) = cx.live.iter().rev().find(|(l, _, _)| l == name).cloned() else {
        return false;
    };
    cx.tmp += 1;
    let t = format!("__new{}", cx.tmp);
    let value = xform_expr(value, cx);
    stmts.push(Stmt::Let { name: t.clone(), ty: None, init: value, frozen: false, stack: false, span });
    clear_moved(cx, stmt_key(span), span, stmts);
    if !cx.facts.escaped.contains(name) && !cx.shell_freed.contains(name) {
        stmts.push(release(name, k, fl, cx.facts.box_shell.contains(name), span));
    }
    stmts.push(Stmt::Assign {
        target: Expr::Ident { name: name.to_string(), span },
        value: Expr::Ident { name: t, span },
        span,
    });
    if fl {
        stmts.push(flag_set(name, span));
    }
    drop_escaped_from_live(cx);
    true
}

fn flag_clear(name: &str, span: Span) -> Stmt {
    Stmt::Assign {
        target: Expr::Ident { name: flag_name(name), span },
        value: int_expr(0, span),
        span,
    }
}

/// The release of a live local: guarded by its flag when a statement may have moved it.
fn release(name: &str, kind: &str, flagged: bool, box_shell: bool, span: Span) -> Stmt {
    let call = free_call(name, kind, span);
    if !flagged {
        return call;
    }
    let else_block = if kind.starts_with("struct:") && box_shell {
        Some(Block {
            stmts: vec![struct_box_free_call(name, span)],
            tail: None,
            span,
        })
    } else {
        None
    };
    Stmt::Expr(Expr::If {
        cond: Box::new(Expr::Binary {
            op: BinOp::Eq,
            left: Box::new(Expr::Ident { name: flag_name(name), span }),
            right: Box::new(int_expr(1, span)),
            span,
        }),
        then_block: Block { stmts: vec![call], tail: None, span },
        elsifs: vec![],
        else_block,
        span,
    })
}

/// Clears the flag of every flagged live local the statement at `key` moved.
fn clear_moved(cx: &mut Ctx, key: (u32, u32), span: Span, out: &mut Vec<Stmt>) {
    if let Some(names) = cx.facts.sites.get(&key) {
        for n in names {
            if cx.live.iter().any(|(l, _, fl)| l == n && *fl) {
                out.push(flag_clear(n, span));
                if cx.facts.box_shell.contains(n)
                    && cx.live.iter().any(|(l, k, _)| l == n && k.starts_with("struct:"))
                {
                    out.push(struct_box_free_call(n, span));
                    cx.shell_freed.insert(n.clone());
                }
            }
        }
    }
}

/// A local that fully escaped is released elsewhere; drop it from the pending list.
fn drop_escaped_from_live(cx: &mut Ctx) {
    cx.live.retain(|(n, _, _)| {
        !cx.facts.escaped.contains(n) || cx.facts.box_shell.contains(n)
    });
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
    xform_block_scope(b, cx, mark)
}

/// `xform_block` with the scope starting at `mark`: a function body also owns the by-value
/// parameters seeded below it (SPEC «Memoria»: the callee releases what it took).
fn xform_block_scope(b: Block, cx: &mut Ctx, mark: usize) -> Block {
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
                if cand.is_some() && cx.rebound.contains(&name) && cx.live.iter().any(|(l, _, _)| l == &name) {
                    rebind(&name, init, span, cx, &mut stmts);
                    continue;
                }
                let mut init = init;
                own_enum_list(&name, matches!(cand, Some((k, _)) if k == "list_free"), &mut init, &mut cx.owned_elems);
                let init = xform_expr(init, cx);
                stmts.push(Stmt::Let { name: name.clone(), ty, init, frozen, stack, span });
                clear_moved(cx, stmt_key(span), span, &mut stmts);
                drop_escaped_from_live(cx);
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
                if let Some((n, _, _)) = &decl {
                    if cx.rebound.contains(n) && cx.live.iter().any(|(l, _, _)| l == n) {
                        let n = n.clone();
                        rebind(&n, value, span, cx, &mut stmts);
                        continue;
                    }
                }
                let mut value = value;
                if let (Some((n, k, _)), Expr::Ident { .. }) = (&decl, &target) {
                    own_enum_list(n, *k == "list_free", &mut value, &mut cx.owned_elems);
                }
                let value = xform_expr(value, cx);
                stmts.push(Stmt::Assign { target, value, span });
                clear_moved(cx, stmt_key(span), span, &mut stmts);
                drop_escaped_from_live(cx);
                if let Some(d) = decl {
                    if d.2 {
                        stmts.push(flag_init(&d.0, span));
                    }
                    cx.live.push(d);
                }
            }
            Stmt::Expr(e) => {
                let span = e.span();
                // A fresh value discarded as a statement (`xs.pop()`, `int_to_str(n)`) has no name
                // to be released by: bind it to a temporary and release it.
                match cx.discard_kind(&e) {
                    Some(k) => {
                        cx.tmp += 1;
                        let t = format!("__dis{}", cx.tmp);
                        let init = xform_expr(e, cx);
                        stmts.push(Stmt::Let { name: t.clone(), ty: None, init, frozen: false, stack: false, span });
                        stmts.push(release(&t, k, false, false, span));
                    }
                    None => stmts.push(Stmt::Expr(xform_expr(e, cx))),
                }
                clear_moved(cx, stmt_key(span), span, &mut stmts);
                drop_escaped_from_live(cx);
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
                        if !cx.facts.escaped.contains(n) && !cx.shell_freed.contains(n) {
                            stmts.push(release(n, k, *fl, cx.facts.box_shell.contains(n), span));
                        }
                    }
                    stmts.push(Stmt::Return { value, span });
                }
                diverged = true;
            }
        }
    }
    if let Some(t) = &b.tail {
        if is_call_tail(t) {
            clear_moved(cx, stmt_key(t.span()), t.span(), &mut stmts);
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
                    if !cx.facts.escaped.contains(n) && !cx.shell_freed.contains(n) {
                        stmts.push(release(n, k, *fl, cx.facts.box_shell.contains(n), span));
                    }
                }
                tail = Some(Box::new(Expr::Ident { name, span }));
            } else {
                for (n, k, fl) in own.iter().rev() {
                    if cx.facts.box_shell.contains(n)
                        && k.starts_with("struct:")
                        && !cx.shell_freed.contains(n)
                    {
                        stmts.push(struct_box_free_call(n, b.span));
                        cx.shell_freed.insert(n.clone());
                    }
                    if !cx.facts.escaped.contains(n) && !cx.shell_freed.contains(n) {
                        stmts.push(release(n, k, *fl, cx.facts.box_shell.contains(n), b.span));
                    }
                }
            }
        }
    }
    cx.live.truncate(mark);
    Block { stmts, tail, span: b.span }
}
