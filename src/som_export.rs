//! Export F32 tensors from a sosomodel tree (`.som` + `shards/*.tensor`) to `.salt` v1.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sosomodel::index::TensorIndex;
use sosomodel::layout::{DTYPE_F32, MANIFEST_FILE, SHARDS_DIR};
use sosomodel::manifest::Manifest;

#[derive(Debug, Clone)]
pub struct InferManifest {
    pub hidden_dim: u32,
    pub num_layers: u32,
    pub num_heads: u32,
    pub num_kv_heads: u32,
    pub ffn_dim: u32,
    pub vocab_size: u32,
    pub max_seq: u32,
}

pub fn read_infer_manifest(model_dir: &Path) -> Result<InferManifest, String> {
    let raw = fs::read(model_dir.join(MANIFEST_FILE)).map_err(|e| e.to_string())?;
    let m = Manifest::parse(&raw).map_err(|_| "manifest.som parse failed".to_string())?;
    Ok(InferManifest {
        hidden_dim: m.hidden_dim,
        num_layers: m.num_layers,
        num_heads: m.num_heads,
        num_kv_heads: m.num_kv_heads,
        ffn_dim: m.ffn_dim,
        vocab_size: m.vocab_size,
        max_seq: m.max_seq,
    })
}

fn write_salt_f32(path: &Path, shape: &[u32], data: &[f32]) -> Result<(), String> {
    let rank = shape.len();
    if rank == 0 || rank > 255 {
        return Err(format!("bad rank {rank} for {}", path.display()));
    }
    let expected: usize = shape.iter().map(|&d| d as usize).product();
    if data.len() != expected {
        return Err(format!(
            "{}: shape product {expected} != data len {}",
            path.display(),
            data.len()
        ));
    }
    let mut f = fs::File::create(path).map_err(|e| e.to_string())?;
    f.write_all(b"SALT").map_err(|e| e.to_string())?;
    f.write_all(&1u16.to_le_bytes()).map_err(|e| e.to_string())?;
    f.write_all(&[0u8, rank as u8]).map_err(|e| e.to_string())?;
    for &d in shape {
        f.write_all(&(d as u64).to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    for v in data {
        f.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn load_f32_tensor(model_dir: &Path, entry: &sosomodel::index::TensorEntry) -> Result<Vec<f32>, String> {
    if entry.dtype != DTYPE_F32 {
        return Err(format!(
            "{}: dtype {} (only F32 export for sal infer bench)",
            entry.name, entry.dtype
        ));
    }
    let shard_path = model_dir.join(SHARDS_DIR).join(&entry.shard);
    let bytes = fs::read(&shard_path).map_err(|e| format!("read {}: {e}", shard_path.display()))?;
    let start = entry.offset as usize;
    let end = start.saturating_add(entry.byte_len as usize);
    if end > bytes.len() {
        return Err(format!(
            "{}: range {start}..{end} exceeds shard {} ({} bytes)",
            entry.name,
            entry.shard,
            bytes.len()
        ));
    }
    let slice = &bytes[start..end];
    let n = entry.elems();
    if slice.len() != n * 4 {
        return Err(format!(
            "{}: expected {} f32 bytes, got {}",
            entry.name,
            n * 4,
            slice.len()
        ));
    }
    let mut out = Vec::with_capacity(n);
    for chunk in slice.chunks_exact(4) {
        let b: [u8; 4] = chunk.try_into().map_err(|_| "f32 chunk size".to_string())?;
        out.push(f32::from_le_bytes(b));
    }
    Ok(out)
}

fn salt_name(tensor_name: &str) -> String {
    tensor_name.replace('.', "_") + ".salt"
}

/// Export all F32 tensors listed in `index.som` into `out_dir` as `.salt` files.
pub fn export_model_to_salt(model_dir: &Path, out_dir: &Path) -> Result<InferManifest, String> {
    fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let manifest = read_infer_manifest(model_dir)?;
    let index_raw = fs::read(model_dir.join(sosomodel::layout::INDEX_FILE)).map_err(|e| e.to_string())?;
    let index = TensorIndex::parse(&index_raw).map_err(|_| "index.som parse failed".to_string())?;
    for e in &index.entries {
        let data = load_f32_tensor(model_dir, e)?;
        let out = out_dir.join(salt_name(&e.name));
        write_salt_f32(&out, &e.shape, &data)?;
    }
    let seq = 8u32;
    let h = manifest.hidden_dim;
    let mut inp = Vec::with_capacity((seq * h) as usize);
    for i in 0..(seq * h) {
        inp.push(0.01 * ((i % 17) as f32 + 1.0));
    }
    write_salt_f32(&out_dir.join("bench_input.salt"), &[seq, h], &inp)?;
    write_salt_f32(&out_dir.join("decode_token.salt"), &[1, h], &inp[..h as usize])?;
    let max_seq = manifest.max_seq.max(128);
    let kv_len = (max_seq * manifest.num_heads * (h / manifest.num_heads)) as usize;
    write_salt_f32(
        &out_dir.join("kv_cache.salt"),
        &[max_seq, manifest.num_heads, h / manifest.num_heads],
        &vec![0.0f32; kv_len],
    )?;
    let mut tensor_shapes = serde_json::Map::new();
    for e in &index.entries {
        tensor_shapes.insert(
            e.name.clone(),
            serde_json::Value::Array(
                e.shape
                    .iter()
                    .map(|&d| serde_json::Value::from(d))
                    .collect(),
            ),
        );
    }
    let meta = serde_json::json!({
        "hidden_dim": manifest.hidden_dim,
        "num_layers": manifest.num_layers,
        "num_heads": manifest.num_heads,
        "num_kv_heads": manifest.num_kv_heads,
        "ffn_dim": manifest.ffn_dim,
        "vocab_size": manifest.vocab_size,
        "max_seq": manifest.max_seq,
        "tensor_shapes": tensor_shapes,
    });
    fs::write(
        out_dir.join("manifest.json"),
        serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(manifest)
}

pub fn default_soso_tiny_model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../soso/target/tiny-model")
}

pub fn default_salt_out() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/infer_salt/tiny")
}
