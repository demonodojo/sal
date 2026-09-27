use sal_compiler::device::check_devices;
use sal_compiler::diag::ErrorCode;
use sal_compiler::effects::check_effects;
use sal_compiler::infer::infer_program;
use sal_compiler::ir::lower_program;
use sal_compiler::ir::IrInst;
use sal_compiler::ownership::check_ownership;
use sal_compiler::parser::parse;

fn run_semantics(src: &str) -> Result<(), Vec<sal_compiler::diag::Diagnostic>> {
    let prog = match parse(src) {
        Ok(p) => p,
        Err(d) => return Err(vec![d]),
    };
    infer_program(&prog)?;
    check_effects(&prog)?;
    check_ownership(&prog)?;
    check_devices(&prog)?;
    Ok(())
}

#[test]
fn kernel_index_parses_and_lowers() {
    let src = r#"
fn main() -> Int ! gpu
    on gpu kernel i, j in Tensor[F32, 2, 2] on gpu
        i
    0
"#;
    run_semantics(src).expect("semantics");
    let ir = lower_program(&parse(src).unwrap());
    assert!(
        ir.functions[0]
            .instructions
            .iter()
            .any(|i| matches!(i, IrInst::KernelGrid { .. })),
        "expected KernelGrid in IR: {:?}",
        ir.functions[0].instructions
    );
}

#[test]
fn kernel_index_on_cpu_is_e_device() {
    let src = r#"
fn main() -> Int ! gpu
    on cpu kernel i in Tensor[F32, 2, 2] on cpu
        0
    0
"#;
    let err = run_semantics(src).expect_err("expected E_DEVICE");
    assert!(err.iter().any(|d| d.code == ErrorCode::EDevice), "{err:?}");
}
