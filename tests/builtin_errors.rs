//! Type errors raised for misuse of runtime builtins: each program must be
//! rejected with E_TYPE and a message that names the broken rule.
use sal_compiler::diag::ErrorCode;
use sal_compiler::infer::infer_program;
use sal_compiler::parser::parse;

const CASES: &[(&str, &str)] = &[
    ("matmul expects two tensors", r#"matmul(1)"#),
    ("load requires tensor type arguments", r#"load("f")"#),
    ("str_bytes expects one String argument", r#"str_bytes()"#),
    ("str_bytes expects String", r#"str_bytes(1)"#),
    ("float_to_int expects one argument", r#"float_to_int()"#),
    ("int_to_float expects one argument", r#"int_to_float(1, 2)"#),
    ("str_from_int expects one argument", r#"str_from_int()"#),
    ("str_as_int expects one argument", r#"str_as_int()"#),
    ("str_as_int expects String", r#"str_as_int(1)"#),
    ("str_to_f64_bits expects one argument", r#"str_to_f64_bits()"#),
    ("str_to_f64_bits expects String", r#"str_to_f64_bits(1)"#),
    ("f64_bits_to_str expects one argument", r#"f64_bits_to_str()"#),
    ("syscall expects seven Int arguments", r#"syscall(1)"#),
    ("syscall arguments must be Int", r#"syscall("a", 0, 0, 0, 0, 0, 0)"#),
    ("bytes_new expects one Int argument", r#"bytes_new()"#),
    ("bytes_new expects Int length", r#"bytes_new("a")"#),
    ("bytes_len expects one Bytes argument", r#"bytes_len()"#),
    ("bytes_ptr expects Bytes", r#"bytes_ptr(1)"#),
    ("bytes_get_u8 expects 2 argument", r#"bytes_get_u8(1)"#),
    ("bytes_set_u8 expects 3 argument", r#"bytes_set_u8(1, 2)"#),
    ("expects Bytes or Int pointer", r#"bytes_get_u8("a", 0)"#),
    ("index/value arguments must be Int", r#"bytes_get_u8(bytes_new(4), "a")"#),
    ("bytes_fill expects (Bytes, offset, length, byte)", r#"bytes_fill(1)"#),
    ("bytes_fill expects Bytes or Int pointer", r#"bytes_fill("a", 0, 1, 2)"#),
    ("bytes_fill offset/length/byte must be Int", r#"bytes_fill(bytes_new(4), "a", 1, 2)"#),
    ("bytes_fill_u32 expects (Bytes, offset, count, value)", r#"bytes_fill_u32(1)"#),
    ("bytes_fill_u32 expects Bytes or Int pointer", r#"bytes_fill_u32("a", 0, 1, 2)"#),
    ("bytes_fill_u32 arguments must be Int", r#"bytes_fill_u32(bytes_new(4), "a", 1, 2)"#),
    ("bytes_copy expects (dst, dst_off, src, src_off, len)", r#"bytes_copy(1)"#),
    ("bytes_copy dst/src must be Bytes or Int pointer", r#"bytes_copy("a", 0, "b", 0, 1)"#),
    ("bytes_copy offsets/length must be Int", r#"bytes_copy(bytes_new(4), "a", bytes_new(4), 0, 1)"#),
    ("str_bytes_copy expects (Bytes, offset, String)", r#"str_bytes_copy(1)"#),
    ("str_bytes_copy expects Bytes", r#"str_bytes_copy("a", 0, "b")"#),
    ("str_bytes_copy offset must be Int", r#"str_bytes_copy(bytes_new(4), "a", "b")"#),
    ("str_bytes_copy expects String", r#"str_bytes_copy(bytes_new(4), 0, 5)"#),
    ("bytes_to_str expects one Bytes argument", r#"bytes_to_str()"#),
    ("bytes_to_str expects Bytes", r#"bytes_to_str(1)"#),
    ("bytes_view expects (Bytes, offset, length)", r#"bytes_view(1)"#),
    ("bytes_view expects Bytes or Int pointer", r#"bytes_view("a", 0, 1)"#),
    ("bytes_view offset/length must be Int", r#"bytes_view(bytes_new(4), "a", 1)"#),
    ("bytes_view_mem expects (data_ptr, length)", r#"bytes_view_mem(1)"#),
    ("bytes_view_mem expects Int arguments", r#"bytes_view_mem("a", 1)"#),
    ("missing tensor arg", r#"map()"#),
];

#[test]
fn builtin_misuse_is_a_type_error() {
    let mut failures = Vec::new();
    for (msg, expr) in CASES {
        let src = format!("fn main() -> Int ! alloc\n    x = {expr}\n    0\n");
        let prog = match parse(&src) {
            Ok(p) => p,
            Err(d) => {
                failures.push(format!("{expr}: parse error {}", d.message));
                continue;
            }
        };
        match infer_program(&prog) {
            Ok(_) => failures.push(format!("{expr}: accepted")),
            Err(errs) => {
                if !errs.iter().any(|d| d.code == ErrorCode::EType && d.message.contains(msg)) {
                    failures.push(format!(
                        "{expr}: wanted `{msg}`, got {:?}",
                        errs.iter().map(|d| d.message.clone()).collect::<Vec<_>>()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
