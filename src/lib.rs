pub mod ast;
pub mod compile;
pub mod device;
pub mod diag;
pub mod effects;
pub mod fmt;
pub mod fuse;
pub mod incremental;
pub mod infer;
pub mod ir;
pub mod lexer;
pub mod llvm;
pub mod ownership;
pub mod parser;
pub mod span;
pub mod typed;

pub use compile::{
    compile_file, compile_program, compile_source, device_toolchain_present, CompileArtifacts,
    CompileOptions,
};
