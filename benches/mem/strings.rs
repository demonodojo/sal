//! Misma carga que `benches/mem/strings.sal` (la constante N debe coincidir).

const N: usize = 300_000;

fn build(n: usize) -> Vec<String> {
    let mut xs = Vec::new();
    let mut i = 0;
    while i < n {
        xs.push(i.to_string());
        i += 1;
    }
    xs
}

fn run() -> i32 {
    let xs = build(N);
    let ys = xs.clone();
    let mut total = 0_usize;
    let mut i = 0;
    while i < ys.len() {
        total += ys[i].len();
        i += 1;
    }
    (total & 255) as i32
}

// `std::process::exit` no ejecuta destructores: `run` devuelve antes, así que la lista y sus
// elementos se liberan como en sal (que suelta cada local al salir de `main`).
fn main() {
    std::process::exit(run());
}
