#!/usr/bin/env python3
"""Export sosomodel tiny `.salt` tensors (from som2salt) to F32 GGUF for llama.cpp."""

from __future__ import annotations

import argparse
import json
import struct
import sys
from pathlib import Path

import gguf
import numpy as np

SALT_MAGIC = b"SALT"


def read_salt_f32(path: Path) -> tuple[list[int], np.ndarray]:
    raw = path.read_bytes()
    if raw[:4] != SALT_MAGIC:
        raise ValueError(f"{path}: bad magic")
    off = 4
    _ver = struct.unpack_from("<H", raw, off)[0]
    off += 2
    _elem, rank = struct.unpack_from("BB", raw, off)
    off += 2
    dims: list[int] = []
    for _ in range(rank):
        (d,) = struct.unpack_from("<Q", raw, off)
        off += 8
        dims.append(int(d))
    count = 1
    for d in dims:
        count *= d
    floats = struct.unpack_from(f"<{count}f", raw, off)
    arr = np.array(floats, dtype=np.float32)
    if dims:
        arr = arr.reshape(dims)
    return dims, arr


def som_tensor_to_gguf(name: str) -> str | None:
    if name == "embed":
        return "token_embd.weight"
    if name == "lm_head":
        return "output.weight"
    if not name.startswith("L") or "." not in name:
        return None
    layer_s, rest = name.split(".", 1)
    layer = int(layer_s[1:], 10)
    mapping = {
        "attn_norm": "attn_norm.weight",
        "ffn_norm": "ffn_norm.weight",
        "attn_q": "attn_q.weight",
        "attn_k": "attn_k.weight",
        "attn_v": "attn_v.weight",
        "attn_output": "attn_output.weight",
        "ffn_up": "ffn_up.weight",
        "ffn_down": "ffn_down.weight",
    }
    suffix = mapping.get(rest)
    if suffix is None:
        return None
    return f"blk.{layer}.{suffix}"


def salt_filename(tensor_name: str) -> str:
    return tensor_name.replace(".", "_") + ".salt"


def export_gguf(salt_dir: Path, out_path: Path) -> None:
    meta_path = salt_dir / "manifest.json"
    meta = json.loads(meta_path.read_text())
    hidden = int(meta["hidden_dim"])
    layers = int(meta["num_layers"])
    heads = int(meta["num_heads"])
    kv_heads = int(meta["num_kv_heads"])
    ffn = int(meta["ffn_dim"])
    vocab = int(meta["vocab_size"])
    ctx = int(meta.get("max_seq", 128))

    out_path.parent.mkdir(parents=True, exist_ok=True)
    writer = gguf.GGUFWriter(str(out_path), "llama")
    writer.add_architecture()
    writer.add_block_count(layers)
    writer.add_context_length(ctx)
    writer.add_embedding_length(hidden)
    writer.add_feed_forward_length(ffn)
    writer.add_head_count(heads)
    writer.add_head_count_kv(kv_heads)
    writer.add_layer_norm_rms_eps(1e-5)
    writer.add_vocab_size(vocab)
    writer.add_file_type(gguf.LlamaFileType.ALL_F32)
    writer.add_rope_freq_base(10000.0)

    # llama.cpp requires tokenizer metadata even for weight-only bench loads.
    writer.add_tokenizer_model("llama")
    writer.add_tokenizer_pre("default")
    tokens = ["<unk>"] + [f"<{i}>" for i in range(1, vocab)]
    writer.add_token_list(tokens)
    writer.add_token_types([2] * len(tokens))
    writer.add_unk_token_id(0)
    writer.add_bos_token_id(1 if vocab > 1 else 0)
    writer.add_eos_token_id(2 if vocab > 2 else 1)
    writer.add_add_bos_token(True)
    writer.add_add_eos_token(False)

    shapes: dict[str, list[int]] = meta["tensor_shapes"]
    for som_name in shapes:
        gguf_name = som_tensor_to_gguf(som_name)
        if gguf_name is None:
            continue
        salt_path = salt_dir / salt_filename(som_name)
        if not salt_path.is_file():
            raise FileNotFoundError(salt_path)
        _dims, data = read_salt_f32(salt_path)
        writer.add_tensor(gguf_name, data)
        if gguf_name.endswith(".ffn_up.weight"):
            gate_name = gguf_name.replace(".ffn_up.weight", ".ffn_gate.weight")
            writer.add_tensor(gate_name, data.copy())

    last = layers - 1
    out_norm_path = salt_dir / salt_filename(f"L{last:02d}.attn_norm")
    if out_norm_path.is_file():
        _dims, out_norm = read_salt_f32(out_norm_path)
        writer.add_tensor("output_norm.weight", out_norm)

    writer.write_header_to_file()
    writer.write_kv_data_to_file()
    writer.write_tensors_to_file()
    writer.close()


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--salt-dir",
        type=Path,
        default=root / "target/infer_salt/tiny",
        help="Directory with manifest.json and *.salt from som2salt",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=root / "target/infer_salt/tiny/tiny-f32.gguf",
    )
    args = parser.parse_args()
    if not (args.salt_dir / "manifest.json").is_file():
        print(f"missing {args.salt_dir}/manifest.json — run: cargo run --bin som2salt", file=sys.stderr)
        return 1
    export_gguf(args.salt_dir, args.out)
    print(f"Wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
