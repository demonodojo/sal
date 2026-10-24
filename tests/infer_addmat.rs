use sal_compiler::ast::{Expr, Item};
use sal_compiler::infer::infer_program;
use sal_compiler::parser::parse;

#[test]
fn matmul_tensor_add_lhs_shape() {
    let src = r#"
fn test(h: Tensor[F32, 8, 128] on cpu, w: Tensor[F32, 128, 128] on cpu, w_up: Tensor[F32, 256, 128] on cpu) -> Tensor[F32, 8, 256] on cpu
    let x1 = matmul(h, w)
    matmul(x1 + h, transpose(w_up))

fn main() -> Int ! alloc
    0
"#;
    let prog = parse(src).expect("parse");
    let Item::Fn(f) = &prog.items[0] else { panic!("fn") };
    let tail = f.body.tail.as_ref().expect("tail");
    eprintln!("tail={tail:#?}");
    if let Expr::Call { args, .. } = tail.as_ref() {
        eprintln!("matmul arg0={:#?}", args[0]);
    }
    infer_program(&prog).expect("infer should accept matmul after tensor add");
}
