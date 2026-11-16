//! Export a sosomodel directory to `.salt` tensors for sal inference benchmarks.

use std::env;
use std::path::PathBuf;

use sal_compiler::som_export::{
    default_salt_out, default_soso_tiny_model, export_model_to_salt, export_synthetic_tiny,
};

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut model = default_soso_tiny_model();
    let mut out = default_salt_out();
    let mut synthetic = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model" | "-m" => {
                i += 1;
                model = PathBuf::from(args.get(i).expect("--model path"));
            }
            "--out" | "-o" => {
                i += 1;
                out = PathBuf::from(args.get(i).expect("--out path"));
            }
            "--synthetic" => synthetic = true,
            "--help" | "-h" => {
                eprintln!("usage: som2salt [--model DIR] [--out DIR] [--synthetic]");
                eprintln!("  --synthetic writes deterministic tiny weights (no .som model needed)");
                eprintln!("  default model: {}", default_soso_tiny_model().display());
                eprintln!("  default out:   {}", default_salt_out().display());
                return;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let result = if synthetic {
        export_synthetic_tiny(&out)
    } else {
        export_model_to_salt(&model, &out)
    };
    match result {
        Ok(m) => {
            println!(
                "exported {} layers hidden={} heads={} -> {}",
                m.num_layers,
                m.hidden_dim,
                m.num_heads,
                out.display()
            );
        }
        Err(e) => {
            eprintln!("som2salt: {e}");
            std::process::exit(1);
        }
    }
}
