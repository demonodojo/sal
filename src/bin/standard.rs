use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::compile::CompileOptions;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let root = match find_project_root() {
        Some(r) => r,
        None => {
            eprintln!(
                "standard: cannot find standard/main.sal (set SAL_HOME or run from the sal tree)"
            );
            std::process::exit(2);
        }
    };
    let entry = root.join("standard/main.sal");
    if !entry.is_file() {
        eprintln!("standard: missing {}", entry.display());
        std::process::exit(2);
    }

    let release = matches!(env::var("PROFILE").as_deref(), Ok("release"));
    let opts = CompileOptions {
        release,
        instrument: false,
        device: "cpu".into(),
        project_root: root,
        skip_link: false,
    };
    let art = match compile_file(&entry, &opts) {
        Ok(a) => a,
        Err(diags) => {
            for d in &diags {
                eprintln!("{}", d.format_human());
            }
            std::process::exit(1);
        }
    };
    let bin = match art.binary {
        Some(b) => b,
        None => {
            eprintln!("standard: compile produced no binary");
            std::process::exit(1);
        }
    };

    let output = Command::new(&bin)
        .args(&args)
        .output()
        .unwrap_or_else(|e| {
            eprintln!("standard: failed to run {}: {e}", bin.display());
            std::process::exit(1);
        });
    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);
    std::process::exit(output.status.code().unwrap_or(1));
}

fn find_project_root() -> Option<PathBuf> {
    if let Ok(home) = env::var("SAL_HOME") {
        let p = PathBuf::from(home);
        if p.join("standard/main.sal").is_file() {
            return Some(p);
        }
    }

    let baked = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if baked.join("standard/main.sal").is_file() {
        return Some(baked);
    }

    let mut dir = env::current_dir().ok()?;
    loop {
        if dir.join("standard/main.sal").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}
