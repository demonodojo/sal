//! Misma carga que `benches/mem/ints.sal` (la constante N debe coincidir).

const N: usize = 5_000_000;

fn main() {
    let mut xs: Vec<i64> = Vec::new();
    let mut i = 0_i64;
    while (i as usize) < N {
        xs.push(i);
        i += 1;
    }
    let mut total = 0_i64;
    let mut j = 0;
    while j < xs.len() {
        total += xs[j];
        j += 1;
    }
    std::process::exit((total & 255) as i32);
}
