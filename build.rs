use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn hash_tree(path: &Path, hash: &mut Sha256) {
    let mut files: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for file in files {
        if file.is_dir() {
            hash_tree(&file, hash);
        } else {
            hash.update(file.to_string_lossy().as_bytes());
            hash.update([0]);
            hash.update(fs::read(&file).unwrap());
        }
    }
}
fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=runtime");
    let mut hash = Sha256::new();
    hash_tree(Path::new("src"), &mut hash);
    hash_tree(Path::new("runtime"), &mut hash);
    let id: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=SAL_COMPILER_ID={id}");
}
