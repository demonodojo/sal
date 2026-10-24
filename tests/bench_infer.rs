//! sosomodel tiny → `.salt` export and `tiny_prefill.sal` typecheck.

use std::fs;
use std::path::PathBuf;

use sal_compiler::infer::infer_program;
use sal_compiler::parser::parse;
use sal_compiler::som_export::{default_soso_tiny_model, export_model_to_salt};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn export_soso_tiny_to_salt() {
    let model = default_soso_tiny_model();
    if !model.join("manifest.som").is_file() {
        eprintln!("skip export_soso_tiny_to_salt: missing {}", model.display());
        return;
    }
    let out = manifest_dir().join("target/infer_salt/tiny_test");
    let _ = fs::remove_dir_all(&out);
    let m = export_model_to_salt(&model, &out).expect("som2salt export");
    assert_eq!(m.num_layers, 4);
    assert_eq!(m.hidden_dim, 128);
    assert!(out.join("L00_ffn_up.salt").is_file());
    assert!(out.join("bench_input.salt").is_file());
}

#[test]
fn tiny_prefill_typechecks() {
    for name in ["tiny_prefill.sal", "tiny_decode.sal"] {
        let src = fs::read_to_string(manifest_dir().join("examples/infer").join(name))
            .unwrap_or_else(|_| panic!("read {name}"));
        let prog = parse(&src).expect("parse");
        infer_program(&prog).unwrap_or_else(|e| panic!("infer {name}: {e:?}"));
    }
}
