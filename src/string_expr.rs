use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::{BinOp, EnumDef, Expr, Item, Param, Program, StructDef, Type};
use crate::span::Span;
use crate::infer::InferOutput;
use crate::layout::{collect_enum_defs, collect_layout_structs};

/// Types infer recorded per expression span, plus the nominal structs/enums the
/// lowering needs for `.field` and variants. `+` on strings is decided by the
/// type of the left operand as infer saw it, never by the syntax of the operands.
#[derive(Clone, Debug, Default)]
pub struct StringEnv {
    /// `(span.start, span.end)` -> type, from `ExprTypeEntry`.
    pub types: Rc<HashMap<(u32, u32), Type>>,
    pub structs: HashMap<String, StructDef>,
    pub enums: HashMap<String, EnumDef>,
    /// Local/param names bound to a struct type (for `.field` lowering).
    pub struct_types: HashMap<String, String>,
    /// Locals bound to a lambda (`f = x => body`): parameter and body, inlined at calls.
    pub lambdas: HashMap<String, (String, Expr)>,
    /// User functions whose return type is a nominal struct (for assign/init typing).
    pub fn_struct_rets: HashMap<String, String>,
    /// User functions that return `Tensor[…]` (for IR shape on call results).
    pub fn_tensor_rets: HashMap<String, Type>,
    /// Scalarised signature of every function in the graph (see `scalarize`).
    pub fn_sigs: HashMap<String, crate::scalarize::FnSig>,
    /// Name of the binding an `x = x + ...` chain accumulates into, while its
    /// left spine is being lowered.
    pub accumulate: Option<String>,
    pub protocol_calls: HashMap<(u32, u32), (String, String, u32)>,
    pub any_coercions: HashMap<(u32, u32), (String, String)>,
    pub protocols: HashMap<String, crate::ast::ProtocolDef>,
    /// Struct and enum variant constructors across the module graph (`P(…)`, `V(…)` with fields).
    pub ctor_names: HashSet<String>,
}

impl StringEnv {
    /// Struct defs and struct-returning fns from every module in a linked graph.
    pub fn from_module_graph(progs: &[&Program]) -> Self {
        let mut structs = HashMap::new();
        let mut fn_struct_rets = HashMap::new();
        let mut fn_tensor_rets = HashMap::new();
        let mut enums = HashMap::new();
        let mut protocols = HashMap::new();
        for prog in progs {
            for (k, v) in collect_layout_structs(prog) {
                structs.entry(k).or_insert(v);
            }
            for (k, v) in collect_enum_defs(prog) {
                enums.entry(k).or_insert(v);
            }
            for item in &prog.items {
                if let Item::Protocol(p) = item {
                    protocols.insert(p.name.clone(), p.clone());
                }
                if let Item::Fn(f) = item {
                    if let Type::Named { name, args, .. } = &f.ret {
                        if args.is_empty() && structs.contains_key(name) {
                            fn_struct_rets.insert(f.name.clone(), name.clone());
                        }
                    }
                    if matches!(f.ret, Type::Tensor { .. }) {
                        fn_tensor_rets.insert(f.name.clone(), f.ret.clone());
                    }
                }
            }
        }
        let fn_sigs = crate::scalarize::sigs_for_programs(progs, &structs);
        let mut ctor_names = HashSet::new();
        for prog in progs {
            for item in &prog.items {
                match item {
                    Item::Struct(s) => {
                        ctor_names.insert(s.name.clone());
                    }
                    Item::Enum(e) => {
                        for v in &e.variants {
                            if !v.fields.is_empty() {
                                ctor_names.insert(v.name.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Self {
            types: Rc::new(HashMap::new()),
            fn_sigs,
            structs,
            enums,
            struct_types: HashMap::new(),
            lambdas: HashMap::new(),
            fn_struct_rets,
            fn_tensor_rets,
            accumulate: None,
            protocol_calls: HashMap::new(),
            any_coercions: HashMap::new(),
            protocols,
            ctor_names,
        }
    }

    pub fn from_program(prog: &Program) -> Self {
        Self::from_module_graph(&[prog])
    }

    /// Attach the expression types infer produced for this program.
    pub fn with_infer(mut self, out: &InferOutput) -> Self {
        let mut map = HashMap::new();
        for (_, entries) in &out.expr_types_by_fn {
            for e in entries {
                map.insert((e.span_start, e.span_end), e.ty.clone());
            }
        }
        self.types = Rc::new(map);
        self.protocol_calls = out.protocol_calls.clone();
        self.any_coercions = out.any_coercions.clone();
        self
    }

    pub fn protocol_call(&self, span: Span) -> Option<(String, String, u32)> {
        self.protocol_calls.get(&(span.start, span.end)).cloned()
    }

    pub fn any_coercion(&self, span: Span) -> Option<(String, String)> {
        self.any_coercions.get(&(span.start, span.end)).cloned()
    }

    /// Types from `infer_program`, or none when the program does not type.
    pub fn from_program_inferred(prog: &Program) -> Self {
        let env = Self::from_program(prog);
        match crate::infer::infer_program(prog) {
            Ok(out) => env.with_infer(&out),
            Err(_) => env,
        }
    }

    pub fn seed_params(&mut self, params: &[Param]) {
        for p in params {
            self.note_struct_binding(&p.name, &p.ty);
        }
    }

    /// Track the struct type of a binding: the declared type wins, else the initializer.
    pub fn note_init(&mut self, name: &str, declared: Option<&Type>, init: &Expr) {
        if let Some(ty) = declared {
            self.note_struct_binding(name, ty);
        } else if let Some(sn) = struct_name_from_init(init, self) {
            self.struct_types.insert(name.to_string(), sn);
        } else if let Expr::Ident { name: src, .. } = init {
            if let Some(sn) = self.struct_types.get(src).cloned() {
                self.struct_types.insert(name.to_string(), sn);
            }
        } else if let Some(Type::Named { name: sn, args, .. }) = self.expr_type(init).cloned() {
            // e.g. the result of a generic call instantiated with a struct
            if args.is_empty() && self.structs.contains_key(&sn) {
                self.struct_types.insert(name.to_string(), sn);
            }
        }
    }

    pub fn note_struct_binding(&mut self, name: &str, ty: &Type) {
        if let Type::Named { name: sn, args, .. } = ty {
            if args.is_empty() && self.structs.contains_key(sn) {
                self.struct_types.insert(name.to_string(), sn.clone());
            }
        }
    }

    pub fn expr_type(&self, e: &Expr) -> Option<&Type> {
        let sp = e.span();
        self.types.get(&(sp.start, sp.end))
    }

    pub fn expr_is_float(&self, e: &Expr) -> bool {
        matches!(self.expr_type(e), Some(Type::Named { name, args, .. }) if args.is_empty() && name == "Float")
    }

    pub fn expr_is_string(&self, e: &Expr) -> bool {
        self.expr_type(e).map(is_string_type).unwrap_or(false)
    }

    /// `+` whose left operand is a `String`: concatenation.
    pub fn add_is_string_concat(&self, e: &Expr) -> bool {
        match e {
            Expr::Binary {
                op: BinOp::Add,
                left,
                ..
            } => self.expr_is_string(left),
            _ => false,
        }
    }

    /// Left-assoc string `+`: the first pair is `str_concat`; later pairs, whose
    /// left child is itself a string `+`, are `str_append` on that accumulator.
    pub fn add_uses_append(&self, e: &Expr) -> bool {
        let Expr::Binary {
            op: BinOp::Add,
            left,
            ..
        } = e
        else {
            return false;
        };
        self.add_is_string_concat(e) && self.add_is_string_concat(left)
    }
}

fn struct_name_from_init(init: &Expr, env: &StringEnv) -> Option<String> {
    match init {
        Expr::Call { func, args, .. } => {
            if let Expr::Ident { name, .. } = func.as_ref() {
                if env.structs.contains_key(name) && !args.is_empty() {
                    return Some(name.clone());
                }
                if let Some(sn) = env.fn_struct_rets.get(name) {
                    return Some(sn.clone());
                }
            }
            None
        }
        _ => None,
    }
}

/// Leftmost leaf of a `+` chain: `((a + b) + c)` -> `a`.
pub fn add_chain_leftmost(e: &Expr) -> &Expr {
    match e {
        Expr::Binary {
            op: BinOp::Add,
            left,
            ..
        } => add_chain_leftmost(left),
        _ => e,
    }
}

/// Runtime builtins that return a fresh heap `String` nobody else owns. Used to
/// free the right operand of `+` after it has been copied into the result.
/// `str_from_int` is a pure cast and user functions may return borrowed
/// handles, so neither is here.
pub fn call_allocates_fresh_string(name: &str) -> bool {
    matches!(
        name,
        "int_to_str" | "char_to_str" | "str_slice" | "strdup" | "read_file"
    )
}

/// A left operand that is itself a fresh-string call is a temporary the first
/// `str_concat` of a chain copies from and must release.
pub fn add_left_is_fresh_call(left: &Expr) -> bool {
    match left {
        Expr::Call { func, .. } => {
            matches!(func.as_ref(), Expr::Ident { name, .. } if call_allocates_fresh_string(name))
        }
        _ => false,
    }
}

/// The right operand of a string `+` is a temporary this expression owns.
pub fn add_right_is_fresh_temp(right: &Expr, env: &StringEnv) -> bool {
    match right {
        Expr::Binary { op: BinOp::Add, .. } => env.add_is_string_concat(right),
        Expr::Call { func, .. } => {
            if let Expr::Ident { name, .. } = func.as_ref() {
                call_allocates_fresh_string(name)
            } else {
                false
            }
        }
        _ => false,
    }
}

pub fn is_string_type(t: &Type) -> bool {
    matches!(t, Type::Named { name, .. } if name == "String")
}
