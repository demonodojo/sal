//! `W_DEPRECATED`: a warning at each use of an item marked `@deprecated("…")`.
//!
//! The check runs on the qualified modules of a graph (`modules::resolve_module_graph`), so a use
//! from another module is matched by the name the item has there (a clashing name is already
//! renamed to its link name). Method calls are already resolved to `T__m` calls at that point.
//! A warning never stops the compilation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::span::Span;

/// The code of every deprecation warning.
pub const W_DEPRECATED: &str = "W_DEPRECATED";

#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
    pub path: PathBuf,
    pub span: Span,
}

impl Warning {
    /// `warning[W_DEPRECATED]: … (path:line:col)`, the form the CLI prints.
    pub fn format_human(&self) -> String {
        format!(
            "warning[{}]: {} ({}:{}:{})",
            self.code,
            self.message,
            self.path.display(),
            self.span.line,
            self.span.col
        )
    }
}

/// The warnings of `modules` (each with the path of its file), in module and source order.
pub fn check_deprecations(modules: &[(PathBuf, &Program)]) -> Vec<Warning> {
    let mut deprecated: HashMap<String, String> = HashMap::new();
    let mut const_names: HashSet<String> = HashSet::new();
    for (_, prog) in modules {
        for item in &prog.items {
            match item {
                Item::Fn(f) => {
                    if let Some(msg) = &f.deprecated {
                        deprecated.insert(f.name.clone(), msg.clone());
                    }
                }
                Item::Struct(s) => {
                    if let Some(msg) = &s.deprecated {
                        deprecated.insert(s.name.clone(), msg.clone());
                    }
                }
                Item::Enum(e) => {
                    if let Some(msg) = &e.deprecated {
                        deprecated.insert(e.name.clone(), msg.clone());
                    }
                }
                Item::Const(c) => {
                    if let Some(msg) = &c.deprecated {
                        deprecated.insert(c.name.clone(), msg.clone());
                        const_names.insert(c.name.clone());
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    if deprecated.is_empty() {
        return out;
    }
    for (path, prog) in modules {
        let mut walk = Walk {
            deprecated: &deprecated,
            const_names: &const_names,
            path,
            out: &mut out,
        };
        for item in &prog.items {
            walk.item(item);
        }
    }
    out
}

struct Walk<'a> {
    deprecated: &'a HashMap<String, String>,
    /// Deprecated constants. A bare name warns only for these, so a deprecated
    /// function's name used as an identifier does not warn twice.
    const_names: &'a HashSet<String>,
    path: &'a Path,
    out: &'a mut Vec<Warning>,
}

impl Walk<'_> {
    fn hit(&mut self, name: &str, span: Span) {
        if let Some(msg) = self.deprecated.get(name) {
            self.out.push(Warning {
                code: W_DEPRECATED,
                message: format!("`{name}` is deprecated: {msg}"),
                path: self.path.to_path_buf(),
                span,
            });
        }
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => {
                for p in &f.params {
                    self.ty(&p.ty);
                }
                self.ty(&f.ret);
                self.block(&f.body);
            }
            Item::Struct(s) => {
                for field in &s.fields {
                    self.ty(&field.ty);
                }
            }
            Item::Enum(e) => {
                for v in &e.variants {
                    for t in &v.fields {
                        self.ty(t);
                    }
                }
            }
            Item::Const(c) => {
                if let Some(t) = &c.ty {
                    self.ty(t);
                }
                self.expr(&c.value);
            }
            Item::Import(_) | Item::Frame(_) | Item::Protocol(_) => {}
        }
    }

    fn ty(&mut self, ty: &Type) {
        match ty {
            Type::Named { name, args, span } | Type::Qualified { name, args, span, .. } => {
                self.hit(name, *span);
                for a in args {
                    self.ty(a);
                }
            }
            Type::Fn { params, ret, .. } => {
                for p in params {
                    self.ty(p);
                }
                self.ty(ret);
            }
            Type::Tensor { .. } => {}
        }
    }

    fn block(&mut self, b: &Block) {
        for st in &b.stmts {
            self.stmt(st);
        }
        if let Some(t) = &b.tail {
            self.expr(t);
        }
    }

    fn stmt(&mut self, st: &Stmt) {
        match st {
            Stmt::Let { ty, init, .. } => {
                if let Some(t) = ty {
                    self.ty(t);
                }
                self.expr(init);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::Break { .. } => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Call {
                func,
                type_args,
                args,
                ..
            } => {
                if let Expr::Ident { name, span } = &**func {
                    self.hit(name, *span);
                } else {
                    self.expr(func);
                }
                for a in type_args {
                    if let TypeArg::Type(t) = a {
                        self.ty(t);
                    }
                }
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } => self.expr(expr),
            Expr::To { expr, .. } => self.expr(expr),
            Expr::Block(b) => self.block(b),
            Expr::On { body, .. } => self.block(body),
            Expr::TensorLit { rows, .. } => {
                for row in rows {
                    for c in row {
                        self.expr(c);
                    }
                }
            }
            Expr::Field { base, .. } => self.expr(base),
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.expr(&arm.body);
                }
            }
            Expr::If {
                cond,
                then_block,
                elsifs,
                else_block,
                ..
            } => {
                self.expr(cond);
                self.block(then_block);
                for el in elsifs {
                    self.expr(&el.cond);
                    self.block(&el.body);
                }
                if let Some(b) = else_block {
                    self.block(b);
                }
            }
            Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } | Expr::String { .. } => {}
            Expr::Ident { name, span } => {
                if self.const_names.contains(name) {
                    self.hit(name, *span);
                }
            }
        }
    }
}
