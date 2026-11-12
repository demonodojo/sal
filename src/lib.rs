pub mod ast;
pub mod attrs;
pub mod borrowck;
pub mod compile;
pub mod consteval;
pub mod deprecation;
pub mod device;
pub mod diag;
pub mod drops;
pub mod effects;
pub mod fmt;
pub mod fuse;
pub mod incremental;
pub mod interp;
pub mod loops;
pub mod infer;
pub mod methods;
pub mod modules;
pub mod ir;
pub mod layout;
pub mod lexer;
pub mod llvm;
pub mod ownership;
pub mod packages;
pub mod protocols;
pub mod parser;
pub mod qualify;
pub mod scalarize;
pub mod span;
pub mod specialize;
pub mod som_export;
pub mod string_expr;
pub mod typed;

pub use compile::{
    compile_file, compile_program, compile_source, device_toolchain_present, CompileArtifacts,
    CompileOptions,
};

pub mod target;
