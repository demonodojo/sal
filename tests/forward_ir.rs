use sal_compiler::fuse::fuse_module;
use sal_compiler::ir::{ir_to_text, lower_program};
use sal_compiler::parser::parse;

#[test]
fn forward_single_fused_region() {
    let src = include_str!("../examples/forward.sal");
    let p = parse(src).expect("parse forward");
    let mut ir = lower_program(&p);
    fuse_module(&mut ir);
    let text = ir_to_text(&ir);
    assert!(text.contains("region on p"), "{text}");
    assert!(text.contains("Matmul"), "{text}");
    assert!(text.contains("peak_bytes") || text.contains("peak="), "{text}");
    // Single fused region; relu folded into matmul epilogue (no separate intermediate).
    assert_eq!(
        text.matches("region on").count(),
        1,
        "expected one fused region:\n{text}"
    );
    assert!(
        text.contains("MapEpilogue") && text.contains("relu"),
        "expected fused relu epilogue:\n{text}"
    );
    assert!(
        text.contains("peak_symbolic") || text.contains("?"),
        "dynamic axis should leave a symbolic peak size:\n{text}"
    );
    assert!(
        !text.contains("4096") && !text.contains("2048"),
        "peak_bytes must come from shapes, not magic constants:\n{text}"
    );
    // `?` on x becomes a named runtime dim arg passed to sal_matmul_f32.
    assert!(
        text.contains("x_d0") && text.contains("dim_params"),
        "expected named runtime length for `?` axis:\n{text}"
    );
    let forward = ir.functions.iter().find(|f| f.name == "forward").expect("forward");
    let mm = forward
        .instructions
        .iter()
        .find_map(|i| match i {
            sal_compiler::ir::IrInst::Call { func, args, .. }
                if func == "sal_matmul_f32" =>
            {
                Some(args)
            }
            _ => None,
        })
        .expect("matmul call");
    assert!(
        mm.len() >= 5,
        "matmul Call must carry m,k,n length args: {mm:?}"
    );
    assert_eq!(mm[2], "x_d0", "batch `?` must be the named dim arg");
}

#[test]
fn string_plus_lowers_concat_then_append() {
    let src = r#"
fn join(s: String, n: String, m: String) -> String ! alloc
    s + n + m
"#;
    let p = parse(src).expect("parse");
    let ir = lower_program(&p);
    let text = ir_to_text(&ir);
    let join = text
        .split("fn main:")
        .next()
        .unwrap_or(&text);
    assert!(
        join.contains("sal_str_concat"),
        "first + of a String variable must be str_concat:\n{text}"
    );
    assert!(
        join.contains("sal_str_append"),
        "later + must be str_append on the temporary:\n{text}"
    );
    assert!(
        !join.contains("Binary"),
        "string + must not lower to integer add:\n{text}"
    );
    assert!(
        !join.contains("sal_free"),
        "operands owned by bindings are not freed by +:\n{text}"
    );
}

fn lower_text(src: &str) -> String {
    let p = parse(src).expect("parse");
    ir_to_text(&lower_program(&p))
}

#[test]
fn string_plus_typed_left_from_call_and_field() {
    // The decision is infer's type of the left operand, not the syntax.
    let by_call = lower_text(
        r#"
fn tag(x: Int) -> String ! alloc
    str_from_int(x) + "a"
"#,
    );
    assert!(
        by_call.contains("sal_str_concat") && !by_call.contains("Binary"),
        "str_from_int(x) + lit must be a concat:\n{by_call}"
    );
    assert!(
        !by_call.contains("sal_free"),
        "str_from_int is a cast, not a fresh temp:\n{by_call}"
    );

    let by_field = lower_text(
        r#"
struct Msg
    text: String
    n: Int

fn show(m: Msg) -> String ! alloc
    m.text + "!"
"#,
    );
    assert!(
        by_field.contains("sal_str_concat") && !by_field.contains("Binary"),
        "m.text + lit must be a concat:\n{by_field}"
    );
}

#[test]
fn string_accumulator_appends_in_place() {
    let text = lower_text(
        r#"
fn go(acc: String, piece: String) -> String ! alloc
    acc = acc + piece + "\n"
    acc
"#,
    );
    assert!(
        text.contains("sal_str_append"),
        "acc = acc + ... must append:\n{text}"
    );
    assert!(
        !text.contains("sal_str_concat"),
        "acc = acc + ... must not copy acc:\n{text}"
    );
    // Not the accumulator pattern: `x = y + ...` copies `y`.
    let copy = lower_text(
        r#"
fn go(a: String, b: String) -> String ! alloc
    a = b + a
    a
"#,
    );
    assert!(
        copy.contains("sal_str_concat"),
        "a = b + a must start with a copy of b:\n{copy}"
    );
}

#[test]
fn string_plus_frees_fresh_right_temporaries() {
    let fresh = lower_text(
        r#"
fn show(s: String, n: Int) -> String ! alloc
    s + int_to_str(n)
"#,
    );
    let concat_pos = fresh.find("sal_str_concat").expect("concat");
    let free_pos = fresh.find("sal_free").expect("temp must be freed");
    assert!(
        concat_pos < free_pos,
        "the int_to_str temp is freed after the concat:\n{fresh}"
    );

    let nested = lower_text(
        r#"
fn show(s: String, t: String, u: String) -> String ! alloc
    s + (t + u)
"#,
    );
    assert_eq!(
        nested.matches("sal_free").count(),
        1,
        "the inner (t + u) temp is freed once:\n{nested}"
    );

    let cast = lower_text(
        r#"
fn show(s: String, h: Int) -> String ! alloc
    s + str_from_int(h)
"#,
    );
    assert!(
        !cast.contains("sal_free"),
        "str_from_int(h) is a borrowed handle, never freed:\n{cast}"
    );
}
