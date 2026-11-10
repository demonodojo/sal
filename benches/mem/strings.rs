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

fn main() {
    let xs = build(N);
    let ys = xs.clone();
    let mut total = 0_usize;
    let mut i = 0;
    while i < ys.len() {
        total += ys[i].len();
        i += 1;
    }
    std::process::exit((total & 255) as i32);
}
