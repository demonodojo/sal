//! Export F32 tensors from a sosomodel tree (`.som` + `shards/*.tensor`) to `.salt` v1.
//!
//! The `.som` container is read here directly (header, manifest and index) so sal
//! builds without a sibling `soso` checkout. Only the fields the exporter needs are
//! parsed; the rest of the payload is left untouched.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"SOMODL01";
const SOM_HEADER_SIZE: usize = 24;
pub const MANIFEST_FILE: &str = "manifest.som";
pub const INDEX_FILE: &str = "index.som";
pub const SHARDS_DIR: &str = "shards";
pub const DTYPE_F32: u8 = 1;

/// CRC32C (Castagnoli), the checksum `sosomodel` stores in every `.som` header.
fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

/// Validate the header and CRC of a `.som` file and return its version and payload.
fn parse_som(data: &[u8]) -> Result<(u32, &[u8]), ()> {
    if data.len() < SOM_HEADER_SIZE || data[..8] != MAGIC {
        return Err(());
    }
    let crc = u32::from_le_bytes(data[8..12].try_into().map_err(|_| ())?);
    let version = u32::from_le_bytes(data[12..16].try_into().map_err(|_| ())?);
    let payload_len = u64::from_le_bytes(data[16..24].try_into().map_err(|_| ())?);
    let payload_len = usize::try_from(payload_len).map_err(|_| ())?;
    let end = SOM_HEADER_SIZE.checked_add(payload_len).ok_or(())?;
    let body = data.get(SOM_HEADER_SIZE..end).ok_or(())?;
    if crc32c(body) != crc {
        return Err(());
    }
    Ok((version, body))
}

/// Bounds-checked sequential reader for `.som` payloads.
struct Reader<'a> {
    data: &'a [u8],
    off: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, off: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ()> {
        let end = self.off.checked_add(n).ok_or(())?;
        let s = self.data.get(self.off..end).ok_or(())?;
        self.off = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, ()> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }

    fn u64(&mut self) -> Result<u64, ()> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }


    /// NUL-terminated UTF-8 string.
    fn cstr(&mut self) -> Result<&'a str, ()> {
        let rest = self.data.get(self.off..).ok_or(())?;
        let nul = rest.iter().position(|&b| b == 0).ok_or(())?;
        let s = std::str::from_utf8(&rest[..nul]).map_err(|_| ())?;
        self.off += nul + 1;
        Ok(s)
    }
}

/// The globals the exporter needs from `manifest.som`.
struct SomManifest {
    vocab_size: u32,
    hidden_dim: u32,
    num_layers: u32,
    num_heads: u32,
    num_kv_heads: u32,
    ffn_dim: u32,
    max_seq: u32,
}

impl SomManifest {
    fn parse(data: &[u8]) -> Result<Self, ()> {
        let (version, body) = parse_som(data)?;
        let mut r = Reader::new(body);
        let _name = r.cstr()?;
        let vocab_size = r.u32()?;
        let hidden_dim = r.u32()?;
        let num_layers = r.u32()?;
        let num_heads = r.u32()?;
        let ffn_dim = r.u32()?;
        let max_seq = r.u32()?;
        let num_kv_heads = if version >= 2 { r.u32()? } else { num_heads };
        if num_heads == 0 || num_kv_heads == 0 {
            return Err(());
        }
        Ok(Self {
            vocab_size,
            hidden_dim,
            num_layers,
            num_heads,
            num_kv_heads,
            ffn_dim,
            max_seq,
        })
    }
}

struct TensorEntry {
    name: String,
    shard: String,
    offset: u64,
    byte_len: u64,
    shape: Vec<u32>,
    dtype: u8,
}

impl TensorEntry {
    fn elems(&self) -> usize {
        self.shape.iter().map(|&d| d as usize).product()
    }
}

struct TensorIndex {
    entries: Vec<TensorEntry>,
}

impl TensorIndex {
    fn parse(data: &[u8]) -> Result<Self, ()> {
        let (_version, body) = parse_som(data)?;
        let mut r = Reader::new(body);
        let n = r.u32()? as usize;
        let mut entries = Vec::new();
        for _ in 0..n {
            let _id = r.u32()?;
            let name = String::from(r.cstr()?);
            let shard = String::from(r.cstr()?);
            let offset = r.u64()?;
            let byte_len = r.u64()?;
            let ndim = r.u32()? as usize;
            let mut shape = Vec::new();
            for _ in 0..ndim {
                shape.push(r.u32()?);
            }
            let dtype = r.u8()?;
            let _quant = r.u8()?;
            r.take(2)?;
            entries.push(TensorEntry {
                name,
                shard,
                offset,
                byte_len,
                shape,
                dtype,
            });
        }
        Ok(Self { entries })
    }
}

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
    let m = SomManifest::parse(&raw).map_err(|_| "manifest.som parse failed".to_string())?;
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

fn load_f32_tensor(model_dir: &Path, entry: &TensorEntry) -> Result<Vec<f32>, String> {
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
    let index_raw = fs::read(model_dir.join(INDEX_FILE)).map_err(|e| e.to_string())?;
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
    let shapes: Vec<(String, Vec<u32>)> = index
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.shape.clone()))
        .collect();
    write_manifest_json(out_dir, &manifest, &shapes)?;
    Ok(manifest)
}

fn write_manifest_json(
    out_dir: &Path,
    manifest: &InferManifest,
    shapes: &[(String, Vec<u32>)],
) -> Result<(), String> {
    let mut tensor_shapes = serde_json::Map::new();
    for (name, shape) in shapes {
        tensor_shapes.insert(
            name.clone(),
            serde_json::Value::Array(shape.iter().map(|&d| serde_json::Value::from(d)).collect()),
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
    .map_err(|e| e.to_string())
}

fn synthetic_values(seed: &mut u64, n: usize, scale: f32) -> Vec<f32> {
    (0..n)
        .map(|_| {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = ((*seed >> 33) as u32) as f32 / u32::MAX as f32;
            (unit * 2.0 - 1.0) * scale
        })
        .collect()
}

/// Write a deterministic tiny model (same shapes as the 4-layer reference) so the
/// inference benchmark runs without an external sosomodel checkout.
pub fn export_synthetic_tiny(out_dir: &Path) -> Result<InferManifest, String> {
    let manifest = InferManifest {
        hidden_dim: 128,
        num_layers: 4,
        num_heads: 4,
        num_kv_heads: 4,
        ffn_dim: 256,
        vocab_size: 256,
        max_seq: 128,
    };
    fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let h = manifest.hidden_dim as usize;
    let ffn = manifest.ffn_dim as usize;
    let vocab = manifest.vocab_size as usize;
    let mut seed = 0x5eed_1234_u64;
    let mut shapes: Vec<(String, Vec<u32>)> = Vec::new();
    let mut emit = |name: &str,
                    shape: Vec<u32>,
                    data: Vec<f32>|
     -> Result<(), String> {
        write_salt_f32(&out_dir.join(salt_name(name)), &shape, &data)?;
        shapes.push((name.to_string(), shape));
        Ok(())
    };

    emit(
        "embed",
        vec![manifest.vocab_size, manifest.hidden_dim],
        synthetic_values(&mut seed, vocab * h, 0.02),
    )?;
    emit(
        "lm_head",
        vec![manifest.vocab_size, manifest.hidden_dim],
        synthetic_values(&mut seed, vocab * h, 0.02),
    )?;
    for layer in 0..manifest.num_layers {
        let p = format!("L{layer:02}.");
        let norm = vec![1.0f32; h];
        emit(&format!("{p}attn_norm"), vec![manifest.hidden_dim], norm.clone())?;
        for w in ["attn_q", "attn_k", "attn_v", "attn_output"] {
            emit(
                &format!("{p}{w}"),
                vec![manifest.hidden_dim, manifest.hidden_dim],
                synthetic_values(&mut seed, h * h, 0.02),
            )?;
        }
        emit(&format!("{p}ffn_norm"), vec![manifest.hidden_dim], norm)?;
        emit(
            &format!("{p}ffn_up"),
            vec![manifest.ffn_dim, manifest.hidden_dim],
            synthetic_values(&mut seed, ffn * h, 0.02),
        )?;
        emit(
            &format!("{p}ffn_down"),
            vec![manifest.hidden_dim, manifest.ffn_dim],
            synthetic_values(&mut seed, h * ffn, 0.02),
        )?;
    }
    drop(emit);

    let seq = 8u32;
    let mut inp = Vec::with_capacity((seq as usize) * h);
    for i in 0..(seq as usize * h) {
        inp.push(0.01 * ((i % 17) as f32 + 1.0));
    }
    write_salt_f32(&out_dir.join("bench_input.salt"), &[seq, manifest.hidden_dim], &inp)?;
    write_salt_f32(
        &out_dir.join("decode_token.salt"),
        &[1, manifest.hidden_dim],
        &inp[..h],
    )?;
    let kv_len = (manifest.max_seq * manifest.num_heads * (manifest.hidden_dim / manifest.num_heads))
        as usize;
    write_salt_f32(
        &out_dir.join("kv_cache.salt"),
        &[
            manifest.max_seq,
            manifest.num_heads,
            manifest.hidden_dim / manifest.num_heads,
        ],
        &vec![0.0f32; kv_len],
    )?;
    write_manifest_json(out_dir, &manifest, &shapes)?;
    Ok(manifest)
}

/// Optional real sosomodel directory for the inference benchmark. Set
/// `SAL_TINY_MODEL` (or pass `som2salt --model DIR`); the benchmark itself uses
/// [`export_synthetic_tiny`] when no model is configured.
pub fn default_soso_tiny_model() -> PathBuf {
    if let Some(path) = std::env::var_os("SAL_TINY_MODEL") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tiny-model")
}

pub fn default_salt_out() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/infer_salt/tiny")
}
