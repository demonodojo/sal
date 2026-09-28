use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::{compile_file, device_toolchain_present};
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn exec_column_cpu_builds_and_runs() {
    let file = root().join("examples/column_cpu.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile column_cpu.sal");
    assert!(
        art.llvm.contains("call void @sal_tensor_bin_f32"),
        "expected tensor bin call in LLVM:\n{}",
        art.llvm
    );
    let bin = art.binary.expect("linked binary");
    let out = Command::new(&bin)
        .output()
        .expect("run column_cpu binary");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exec_column_gpu_emulated_links() {
    let file = root().join("examples/column_gpu.sal");
    let link = device_toolchain_present("gpu");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "gpu".into(),
        project_root: root(),
        skip_link: !link,
    };
    let art = compile_file(&file, &opts).expect("compile column_gpu.sal");
    assert!(
        art.llvm.contains("call void @sal_tensor_bin_f32"),
        "expected gpu tensor bin call:\n{}",
        art.llvm
    );
    assert!(
        art.llvm.contains("call void @sal_on_enter(i32 1)"),
        "expected gpu on_enter in LLVM:\n{}",
        art.llvm
    );
    if link {
        let bin = art.binary.expect("linked gpu binary");
        let out = Command::new(&bin)
            .output()
            .expect("run column_gpu binary");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
