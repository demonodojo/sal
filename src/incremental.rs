use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{Item, Program};
use crate::infer::infer_program;
use crate::modules::{resolve_import_path_with_manifest, resolve_module_graph};
use crate::parser::parse;
use crate::typed::TypedProgram;

#[derive(Debug, Clone)]
pub struct CompileFlags {
    pub release: bool,
    pub instrument: bool,
    pub device: String,
    /// Stub `@main` when the module has no `fn main`; must differ in the cache key from import objects.
    pub emit_entry_main: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMeta {
    pub key: String,
    /// How many times the `.ll` file was written for this key.
    pub llvm_writes: u64,
    pub ll_path: String,
    pub obj_path: String,
}

/// Cache key without import-graph deps (modules with no `import`, or no source path).
pub fn cache_key(prog: &Program, typed: &TypedProgram, flags: &CompileFlags) -> String {
    cache_key_with_deps(prog, typed, flags, &[])
}

/// Same as [`cache_key`], plus a digest of the typed ASTs of imported modules
/// (see [`import_graph_digest`]). An empty `deps_digest` yields the same key as
/// [`cache_key`].
pub fn cache_key_with_deps(
    prog: &Program,
    typed: &TypedProgram,
    flags: &CompileFlags,
    deps_digest: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(serde_json::to_string(prog).unwrap_or_default());
    hasher.update(serde_json::to_string(typed).unwrap_or_default());
    hasher.update([
        flags.release as u8,
        flags.instrument as u8,
        flags.emit_entry_main as u8,
    ]);
    hasher.update(flags.device.as_bytes());
    if !deps_digest.is_empty() {
        hasher.update(deps_digest);
    }
    hex::encode(hasher.finalize())
}

/// Resolve `import` path strings relative to the importing file's directory,
/// then to `project_root`. Accepts `"std/prelude.sal"`, `"./b.sal"`, or bare
/// names (tries adding `.sal` if missing).
pub fn resolve_import_path(
    import_path: &str,
    source_file: &Path,
    project_root: &Path,
) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = source_file.parent() {
        push_import_candidates(&mut candidates, dir, import_path);
    }
    push_import_candidates(&mut candidates, project_root, import_path);
    candidates.into_iter().find(|p| p.is_file())
}

fn push_import_candidates(out: &mut Vec<PathBuf>, base: &Path, import_path: &str) {
    out.push(base.join(import_path));
    if !import_path.ends_with(".sal") {
        out.push(base.join(format!("{import_path}.sal")));
    }
}

/// Transitive digest of typed ASTs for every module reachable via `import`
/// from `prog`. Empty if there are no resolvable imports (so the cache key
/// matches the no-import case).
pub fn import_graph_digest(
    prog: &Program,
    source_file: &Path,
    project_root: &Path,
) -> Vec<u8> {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut ordered: Vec<PathBuf> = Vec::new();
    collect_import_paths(prog, source_file, project_root, &mut visited, &mut ordered);
    if ordered.is_empty() {
        return Vec::new();
    }
    ordered.sort();
    let mut hasher = Sha256::new();
    for path in &ordered {
        hasher.update(path.to_string_lossy().as_bytes());
        match typed_ast_for_file(path, project_root) {
            Some(typed) => {
                hasher.update(serde_json::to_string(&typed).unwrap_or_default());
            }
            None => {
                if let Ok(bytes) = fs::read(path) {
                    hasher.update(&bytes);
                }
            }
        }
    }
    hasher.finalize().to_vec()
}

fn collect_import_paths(
    prog: &Program,
    source_file: &Path,
    project_root: &Path,
    visited: &mut HashSet<PathBuf>,
    ordered: &mut Vec<PathBuf>,
) {
    for item in &prog.items {
        let Item::Import(imp) = item else {
            continue;
        };
        let Some(path) =
            resolve_import_path_with_manifest(&imp.path, source_file, project_root)
        else {
            continue;
        };
        let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !visited.insert(key) {
            continue;
        }
        ordered.push(path.clone());
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(dep) = parse(&src) else {
            continue;
        };
        collect_import_paths(&dep, &path, project_root, visited, ordered);
    }
}

fn typed_ast_for_file(path: &Path, project_root: &Path) -> Option<TypedProgram> {
    let src = fs::read_to_string(path).ok()?;
    let prog = parse(&src).ok()?;
    let entries = if let Ok(graph) = resolve_module_graph(path, project_root) {
        if let Some(m) = graph.order.iter().find(|m| m.path == path) {
            match crate::modules::infer_module(m, &graph) {
                Ok(out) => out.expr_types_by_fn,
                Err(_) => Vec::new(),
            }
        } else {
            match infer_program(&prog) {
                Ok(out) => out.expr_types_by_fn,
                Err(_) => Vec::new(),
            }
        }
    } else {
        match infer_program(&prog) {
            Ok(out) => out.expr_types_by_fn,
            Err(_) => Vec::new(),
        }
    };
    Some(TypedProgram::from_program(prog, entries))
}

pub fn cache_dir(base: &Path, key: &str) -> PathBuf {
    base.join("target").join("incremental").join(key)
}

/// Path to the meta JSON for a cache key (legacy name kept for callers).
pub fn cache_path(base: &Path, key: &str) -> PathBuf {
    cache_dir(base, key).join("meta.json")
}

pub fn ll_cache_path(base: &Path, key: &str) -> PathBuf {
    cache_dir(base, key).join("module.ll")
}

pub fn obj_cache_path(base: &Path, key: &str) -> PathBuf {
    cache_dir(base, key).join("module.o")
}

pub fn load_cached_meta(path: &Path) -> Option<CacheMeta> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn store_cached_meta(path: &Path, meta: &CacheMeta) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(meta).unwrap_or_default())
}

pub fn is_cache_hit(path: &Path, key: &str) -> bool {
    match load_cached_meta(path) {
        Some(meta) if meta.key == key => {
            let base = path.parent().and_then(|p| p.parent()).and_then(|p| p.parent());
            // meta lives at target/incremental/{key}/meta.json — artifacts next to it
            let dir = path.parent();
            let ll_ok = dir
                .map(|d| d.join("module.ll").is_file())
                .unwrap_or(false);
            let obj_ok = dir
                .map(|d| d.join("module.o").is_file())
                .unwrap_or(false);
            let _ = base;
            ll_ok || obj_ok
        }
        _ => false,
    }
}

pub fn file_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}
