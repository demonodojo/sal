use crate::ir::{FusedOp, IrModule};

/// Pase de fusión: epílogos de map/relu sobre matmul se conservan como MapEpilogue
/// (sin buffer intermedio). Una sola región por bloque `on` ya viene de `lower`.
pub fn fuse_module(m: &mut IrModule) {
    for f in &mut m.functions {
        fuse_function(f);
    }
}

fn fuse_function(f: &mut crate::ir::IrFunction) {
    for region in &mut f.regions {
        region.fused = fuse_ops(&mut region.ops) || region.fused;
        // peak_bytes ya se calculó en lower a partir de formas estáticas / simbólicas.
        // El epílogo fusionado no añade buffer: no se recalcula con constantes mágicas.
    }
}

/// Recognise matmul+map/relu as a fused epilogue pair. Keeps MapEpilogue in the IR
/// (it documents the fused epilogue; the intermediate is not a separate buffer).
fn fuse_ops(ops: &mut Vec<FusedOp>) -> bool {
    let mut fused = false;
    let mut i = 0;
    while i + 1 < ops.len() {
        let is_epilogue = matches!(
            (&ops[i], &ops[i + 1]),
            (
                FusedOp::Matmul { dest, .. },
                FusedOp::MapEpilogue { input, .. }
            ) if input == dest
        );
        if is_epilogue {
            fused = true;
            i += 2;
            continue;
        }
        // Softmax after an unrelated op is not fused away; just advance.
        i += 1;
    }
    // Also fused if a MapEpilogue is already present (from lower).
    fused
        || ops.iter().any(|o| matches!(o, FusedOp::MapEpilogue { .. }))
        || ops.iter().any(|o| matches!(o, FusedOp::ColumnBin { .. }))
}
