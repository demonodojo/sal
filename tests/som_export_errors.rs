//! Error paths of the sosomodel -> `.salt` exporter, driven with damaged copies of the tiny model.
use std::fs;
use std::path::{Path, PathBuf};

use sal_compiler::som_export::{default_soso_tiny_model, export_model_to_salt, read_infer_manifest};

fn scratch(tag: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-som-export")
        .join(tag);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// A fresh copy of the tiny model, or `None` when the sibling soso checkout has none.
fn model_copy(tag: &str) -> Option<(PathBuf, PathBuf)> {
    let src = default_soso_tiny_model();
    if !src.exists() {
        return None;
    }
    let root = scratch(tag);
    let model = root.join("model");
    copy_dir(&src, &model);
    Some((model, root.join("out")))
}

fn first_file(dir: &Path) -> PathBuf {
    let mut files: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files.remove(0)
}

#[test]
fn missing_manifest_is_an_error() {
    let root = scratch("missing_manifest");
    assert!(read_infer_manifest(&root).is_err());
    assert!(export_model_to_salt(&root, &root.join("out")).is_err());
}

#[test]
fn garbage_manifest_fails_to_parse() {
    let Some((model, out)) = model_copy("garbage_manifest") else { return };
    fs::write(model.join(sosomodel::layout::MANIFEST_FILE), b"not a manifest").unwrap();
    let err = export_model_to_salt(&model, &out).unwrap_err();
    assert!(err.contains("manifest.som parse failed"), "{err}");
}

#[test]
fn garbage_index_fails_to_parse() {
    let Some((model, out)) = model_copy("garbage_index") else { return };
    fs::write(model.join(sosomodel::layout::INDEX_FILE), b"not an index").unwrap();
    let err = export_model_to_salt(&model, &out).unwrap_err();
    assert!(err.contains("index.som parse failed"), "{err}");
}

#[test]
fn missing_index_is_an_error() {
    let Some((model, out)) = model_copy("missing_index") else { return };
    fs::remove_file(model.join(sosomodel::layout::INDEX_FILE)).unwrap();
    assert!(export_model_to_salt(&model, &out).is_err());
}

#[test]
fn truncated_shard_is_reported() {
    let Some((model, out)) = model_copy("truncated_shard") else { return };
    let shards = model.join(sosomodel::layout::SHARDS_DIR);
    for e in fs::read_dir(&shards).unwrap() {
        fs::write(e.unwrap().path(), b"").unwrap();
    }
    let err = export_model_to_salt(&model, &out).unwrap_err();
    assert!(err.contains("exceeds shard"), "{err}");
}

#[test]
fn missing_shard_is_reported() {
    let Some((model, out)) = model_copy("missing_shard") else { return };
    fs::remove_file(first_file(&model.join(sosomodel::layout::SHARDS_DIR))).unwrap();
    let err = export_model_to_salt(&model, &out).unwrap_err();
    assert!(err.contains("read "), "{err}");
}

#[test]
fn output_dir_that_is_a_file_is_an_error() {
    let Some((model, out)) = model_copy("out_is_file") else { return };
    fs::write(&out, b"x").unwrap();
    assert!(export_model_to_salt(&model, &out).is_err());
}

#[test]
fn intact_copy_exports_every_tensor() {
    let Some((model, out)) = model_copy("intact") else { return };
    let m = export_model_to_salt(&model, &out).unwrap();
    assert!(m.hidden_dim > 0);
    for f in ["bench_input.salt", "decode_token.salt", "kv_cache.salt", "manifest.json"] {
        assert!(out.join(f).exists(), "{f}");
    }
}
