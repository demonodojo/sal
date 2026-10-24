use sal_compiler::infer::infer_program;
use sal_compiler::parser::parse;

fn check_src(src: &str) {
    let prog = parse(src).expect("parse");
    infer_program(&prog).expect("infer");
}

#[test]
fn broadcast_trailing_dim_ok() {
    let src = r#"
fn main() -> Int
    let a: Tensor[F32, 2, 2] on cpu = tensor[[1.0, 2.0], [3.0, 4.0]]
    let b: Tensor[F32, 2] on cpu = tensor[[1.0, 2.0]]
    let c: Tensor[F32, 2, 2] on cpu = on cpu
        a + b
    0
"#;
    check_src(src);
}

#[test]
fn reshape_volume_ok() {
    let src = r#"
fn main() -> Int
    let x: Tensor[F32, 2, 2] on cpu = tensor[[1.0, 2.0], [3.0, 4.0]]
    let y = reshape(x, 4, 1)
    0
"#;
    check_src(src);
}

#[test]
fn matmul_after_residual_add_ok() {
    let src = r#"
fn layer(h: Tensor[F32, 8, 128] on cpu, w: Tensor[F32, 128, 128] on cpu, w_up: Tensor[F32, 256, 128] on cpu) -> Tensor[F32, 8, 256] on cpu
    let residual = h
    let x1 = matmul(residual, w)
    matmul(x1 + residual, transpose(w_up))

fn main() -> Int ! alloc
    0
"#;
    check_src(src);
}

#[test]
fn tensor_append_and_decode_types_ok() {
    let src = r#"
fn decode(h: Tensor[F32, 1, 128] on cpu, w: Tensor[F32, 128, 128] on cpu, k: Tensor[F32, ?, 4, 32] on cpu, v: Tensor[F32, ?, 4, 32] on cpu) -> Tensor[F32, 1, 128] on cpu
    let q = matmul(h, w)
    let k1 = rope(q, 0)
    let row = reshape(k1, 4, 32)
    let kc = tensor_append(k, row)
    let vc = tensor_append(v, row)
    let q4 = reshape(q, 1, 4, 32)
    let a4 = attention(q4, kc, vc, true)
    reshape(a4, 1, 128)

fn main() -> Int ! alloc
    0
"#;
    check_src(src);
}

#[test]
fn tensor_add_same_rank_for_matmul_arg() {
    let src = r#"
fn f(h: Tensor[F32, 8, 128] on cpu, w: Tensor[F32, 128, 128] on cpu, w_up: Tensor[F32, 256, 128] on cpu) -> Tensor[F32, 8, 256] on cpu
    let x1 = matmul(h, w)
    matmul(x1 + h, transpose(w_up))

fn main() -> Int ! alloc
    0
"#;
    check_src(src);
}

#[test]
fn attention_fn_types_ok() {
    let src = r#"
fn go(q: Tensor[F32, 2, 2, 2] on cpu, k: Tensor[F32, 2, 1, 2] on cpu, v: Tensor[F32, 2, 1, 2] on cpu) -> Tensor[F32, 2, 2, 2] on cpu
    attention(q, k, v, true)

fn main() -> Int
    0
"#;
    check_src(src);
}
