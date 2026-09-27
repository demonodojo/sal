use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use clap::{Parser, Subcommand};
use sal_compiler::ast::Program;
use sal_compiler::compile::{compile_file, compile_program, CompileOptions};
use sal_compiler::diag::Diagnostic;
use sal_compiler::fmt::format_program;
use sal_compiler::fuse::fuse_module;
use sal_compiler::infer::infer_program;
use sal_compiler::ir::{ir_to_text, lower_program, lower_program_with_callables};
use sal_compiler::modules::{callable_fn_names, resolve_module_graph};
use sal_compiler::llvm::{collect_host_tensors, emit_llvm_with_tensors, LlvmOptions};
use sal_compiler::parser::parse;
use sal_compiler::typed::TypedProgram;

#[derive(Parser)]
#[command(name = "sal", about = "Bootstrap compiler for sal")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Check {
        file: PathBuf,
        #[arg(long, default_value = "cpu")]
        device: String,
        #[arg(long, default_value = "human")]
        error_format: String,
    },
    Build {
        file: PathBuf,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        instrument: bool,
        #[arg(long, default_value = "cpu")]
        device: String,
        #[arg(long)]
        instrument_out: Option<PathBuf>,
        #[arg(long, default_value = "human")]
        error_format: String,
    },
    Run {
        file: PathBuf,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        instrument: bool,
        #[arg(long, default_value = "cpu")]
        device: String,
        #[arg(long)]
        instrument_out: Option<PathBuf>,
        #[arg(long, default_value = "human")]
        error_format: String,
    },
    Fmt {
        file: PathBuf,
    },
    Emit {
        kind: String,
        file: PathBuf,
    },
    /// Compile from a JSON AST (`sal emit ast` output).
    Compile {
        /// Path to a JSON-serialized `Program`.
        #[arg(long)]
        ast: PathBuf,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        instrument: bool,
        #[arg(long, default_value = "cpu")]
        device: String,
        #[arg(long, default_value = "human")]
        error_format: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match cli.command {
        Commands::Check {
            file,
            device,
            error_format,
        } => {
            if let Err(diags) = run_check(&file, &root, &device) {
                print_diags(&diags, &error_format);
                std::process::exit(1);
            }
        }
        Commands::Build {
            file,
            release,
            instrument,
            device,
            instrument_out: _,
            error_format,
        } => match build(&file, &root, release, instrument, &device, false) {
            Ok(path) => println!("{}", path.display()),
            Err(diags) => {
                print_diags(&diags, &error_format);
                std::process::exit(1);
            }
        },
        Commands::Run {
            file,
            release,
            instrument,
            device,
            instrument_out,
            error_format,
        } => {
            let bin = match build(&file, &root, release, instrument, &device, false) {
                Ok(p) => p,
                Err(diags) => {
                    print_diags(&diags, &error_format);
                    std::process::exit(1);
                }
            };
            let mut cmd = Command::new(bin);
            if instrument {
                cmd.env("SAL_INSTRUMENT", "1");
            }
            let output = cmd.output().expect("run");
            let _ = std::io::stdout().write_all(&output.stdout);
            let _ = std::io::stderr().write_all(&output.stderr);
            // `--instrument-out` captures the process stderr (JSON event lines).
            if let Some(out) = instrument_out {
                if let Err(e) = std::fs::write(&out, &output.stderr) {
                    eprintln!("failed to write --instrument-out: {e}");
                }
            }
            if !output.status.success() {
                std::process::exit(output.status.code().unwrap_or(1));
            }
        }
        Commands::Fmt { file } => {
            let src = std::fs::read_to_string(&file).expect("read");
            let prog = parse(&src).expect("parse");
            print!("{}", format_program(&prog));
        }
        Commands::Emit { kind, file } => {
            if kind == "ir" {
                let graph = match resolve_module_graph(&file, &root) {
                    Ok(g) => g,
                    Err(diags) => {
                        print_diags(&diags, "human");
                        std::process::exit(1);
                    }
                };
                let root_mod = graph.order.last().expect("empty module graph");
                let callables = callable_fn_names(root_mod, &graph);
                let mut ir = lower_program_with_callables(&root_mod.program, &callables);
                fuse_module(&mut ir);
                print!("{}", ir_to_text(&ir));
                return;
            }
            let src = std::fs::read_to_string(&file).expect("read");
            let prog = parse(&src).expect("parse");
            match kind.as_str() {
                "ast" => println!("{}", serde_json::to_string_pretty(&prog).expect("json")),
                "typed" => {
                    let infer = infer_program(&prog).expect("infer");
                    let typed = TypedProgram::from_program(prog, infer.expr_types_by_fn);
                    println!("{}", serde_json::to_string_pretty(&typed).expect("json"));
                }
                "llvm" => {
                    let mut ir = lower_program(&prog);
                    fuse_module(&mut ir);
                    let tensors = collect_host_tensors(&prog);
                    print!(
                        "{}",
                        emit_llvm_with_tensors(
                            &ir,
                            &LlvmOptions {
                                instrument: false,
                                extern_user_fns: HashMap::new(),
                                emit_entry_main: true,
                            },
                            &tensors
                        )
                    );
                }
                other => {
                    eprintln!("unknown emit kind: {other}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Compile {
            ast,
            release,
            instrument,
            device,
            error_format,
        } => {
            let raw = std::fs::read_to_string(&ast).expect("read ast json");
            let prog: Program = match serde_json::from_str(&raw) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("failed to deserialize Program JSON: {e}");
                    std::process::exit(1);
                }
            };
            match compile_program(
                &prog,
                &opts(&root, release, instrument, &device, false),
            ) {
                Ok(art) => {
                    if let Some(bin) = art.binary {
                        println!("{}", bin.display());
                    }
                }
                Err(diags) => {
                    print_diags(&diags, &error_format);
                    std::process::exit(1);
                }
            }
        }
    }
}

fn print_diags(diags: &[Diagnostic], error_format: &str) {
    if error_format == "json" {
        for d in diags {
            eprintln!("{}", serde_json::to_string(d).expect("diag json"));
        }
    } else {
        for d in diags {
            eprintln!("{}", d.format_human());
        }
    }
}

fn opts(
    root: &PathBuf,
    release: bool,
    instrument: bool,
    device: &str,
    skip_link: bool,
) -> CompileOptions {
    CompileOptions {
        release,
        instrument,
        device: device.to_string(),
        project_root: root.clone(),
        skip_link,
    }
}

fn run_check(
    file: &PathBuf,
    root: &PathBuf,
    device: &str,
) -> Result<(), Vec<Diagnostic>> {
    // skip_link: check does not require a physical GPU/TPU toolchain.
    compile_file(file, &opts(root, false, false, device, true)).map(|_| ())
}

fn build(
    file: &PathBuf,
    root: &PathBuf,
    release: bool,
    instrument: bool,
    device: &str,
    skip_link: bool,
) -> Result<PathBuf, Vec<Diagnostic>> {
    let art = compile_file(file, &opts(root, release, instrument, device, skip_link))?;
    art.binary.ok_or_else(|| {
        vec![Diagnostic::new(
            sal_compiler::diag::ErrorCode::EInternal,
            "no binary produced",
            Default::default(),
        )]
    })
}
