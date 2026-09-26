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
