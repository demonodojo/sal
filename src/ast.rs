use serde::{Deserialize, Serialize};

use crate::span::Span;

pub const AST_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Program {
    pub schema_version: u32,
    pub items: Vec<Item>,
    /// Legacy field for old JSON; the parser leaves this empty and emits
    /// `Item::Struct` / `Item::Enum` in `items` instead.
    #[serde(default)]
    pub type_defs: Vec<TypeDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Item {
    Fn(FnDef),
    Import(Import),
    Struct(StructDef),
    Enum(EnumDef),
    Frame(FrameDef),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ColumnElem {
    F32,
    F16,
    BF16,
    I8,
    String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrameColumn {
    pub name: String,
    pub elem: ColumnElem,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrameDef {
    pub name: String,
    #[serde(default)]
    pub type_params: Vec<String>,
    pub columns: Vec<FrameColumn>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Import {
    pub path: String,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FnDef {
    pub name: String,
    #[serde(default)]
    pub type_params: Vec<String>,
    pub params: Vec<Param>,
    pub ret: Type,
    pub effects: Vec<Effect>,
    pub body: Block,
    #[serde(default)]
    pub no_heap: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub mode: ParamMode,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ParamMode {
    #[default]
    Inferred,
    Borrow,
    Take,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Effect {
    Io,
    Alloc,
    Panic,
    Gpu,
    Tpu,
}

/// Argumento entre corchetes de una llamada. No es un tipo: un eje no se
/// guarda como `Type::Named` con el número por nombre.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TypeArg {
    Type(Type),
    Dim(Dim),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Type {
    Named {
        name: String,
        args: Vec<Type>,
        span: Span,
    },
    Tensor {
        elem: TensorElem,
        dims: Vec<Dim>,
        place: Place,
        span: Span,
    },
    Fn {
        params: Vec<Type>,
        ret: Box<Type>,
        effects: Vec<Effect>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum TensorElem {
    F32,
    F16,
    BF16,
    I8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Dim {
    Static(u64),
    Dynamic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    Cpu,
    Gpu,
    Tpu,
    Param(String),
}

/// Coordinate grid for `on gpu kernel`: names bind to `Int` over the tensor shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelIndex {
    pub names: Vec<String>,
    pub shape: KernelShape,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum KernelShape {
    Type(Type),
    Binding(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        ty: Option<Type>,
        init: Expr,
        #[serde(default)]
        frozen: bool,
        span: Span,
    },
    Expr(Expr),
    Assign {
        target: Expr,
        value: Expr,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    /// `while cond` newline indent body. The condition is evaluated each iteration.
    While {
        cond: Expr,
        body: Block,
        span: Span,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Expr {
    Int {
        value: i64,
        span: Span,
    },
    Float {
        value: f64,
        span: Span,
    },
    Bool {
        value: bool,
        span: Span,
    },
    String {
        value: String,
        /// Structured interpolation parts; empty when the string has no `{…}`.
        #[serde(default)]
        parts: Vec<StringPart>,
        span: Span,
    },
    Ident {
        name: String,
        span: Span,
    },
    Call {
        func: Box<Expr>,
        /// `load[F32, 4, 4](path)`: `F32` es un tipo; `4` y `?` son dimensiones.
        type_args: Vec<TypeArg>,
        args: Vec<Expr>,
        span: Span,
    },
    /// `x => body` (one parameter in this phase).
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        span: Span,
    },
    Block(Block),
    On {
        place: Place,
        kernel: bool,
        #[serde(default)]
        kernel_index: Option<KernelIndex>,
        body: Block,
        span: Span,
    },
    To {
        place: Place,
        expr: Box<Expr>,
        span: Span,
    },
    TensorLit {
        rows: Vec<Vec<Expr>>,
        span: Span,
    },
    Field {
        base: Box<Expr>,
        field: String,
        span: Span,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    If {
        cond: Box<Expr>,
        then_block: Block,
        /// Ramas `elsif`, en orden, al mismo nivel que este `if`.
        #[serde(default)]
        elsifs: Vec<Elsif>,
        #[serde(default)]
        else_block: Option<Block>,
        span: Span,
    },
    Try {
        expr: Box<Expr>,
        span: Span,
    },
}

/// Part of an interpolated string literal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StringPart {
    Lit(String),
    /// Simple `{name}` interpolation (identifier only in this phase).
    Interp(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// Una rama `elsif cond` seguida de su suite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Elsif {
    pub cond: Expr,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Pattern {
    Wild(Span),
    Ident(String, Span),
    Int(i64, Span),
    Variant {
        name: String,
        args: Vec<Pattern>,
        span: Span,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TypeDef {
    Struct(StructDef),
    Enum(EnumDef),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructDef {
    pub name: String,
    #[serde(default)]
    pub type_params: Vec<String>,
    pub fields: Vec<StructField>,
    #[serde(default)]
    pub layout_c: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructField {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnumDef {
    pub name: String,
    #[serde(default)]
    pub type_params: Vec<String>,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<Type>,
    pub span: Span,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::Bool { span, .. }
            | Expr::String { span, .. }
            |             Expr::Ident { span, .. }
            | Expr::Call { span, .. }
            | Expr::Lambda { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::On { span, .. }
            | Expr::To { span, .. }
            | Expr::TensorLit { span, .. }
            | Expr::Field { span, .. }
            | Expr::Match { span, .. }
            | Expr::If { span, .. }
            | Expr::Try { span, .. } => *span,
            Expr::Block(b) => b.span,
        }
    }
}

impl Type {
    pub fn span(&self) -> Span {
        match self {
            Type::Named { span, .. }
            | Type::Tensor { span, .. }
            | Type::Fn { span, .. } => *span,
        }
    }
}
