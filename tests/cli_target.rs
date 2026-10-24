use std::{fs, process::Command};

#[test]
fn run_preserves_arguments_exit_status_and_relative_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("main.sal");
    fs::write(&source, "fn main() -> Int ! io, alloc\n    if str_eq(argv(1), d\"dos palabras\") == 1\n        42\n    else\n        3\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_sal"))
        .current_dir(dir.path())
        .args(["run", "main.sal", "-o", "mi programa", "--", "dos palabras"])
        .output()
        .unwrap();
    assert_eq!(
        run.status.code(),
        Some(42),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(dir.path().join("mi programa").is_file());
    fs::write(&source, "fn main() -> Int\n    9\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_sal"))
        .current_dir(dir.path())
        .args(["run", "main.sal", "-o", "mi programa"])
        .output()
        .unwrap();
    assert_eq!(
        run.status.code(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
