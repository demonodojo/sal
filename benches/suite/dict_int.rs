//! Misma carga que `benches/suite/dict_int.sal`.
use std::collections::HashMap;

fn run() -> i32 {
    let n: i64 = std::hint::black_box(1_000_000);
    let mut d: HashMap<i64, i64> = HashMap::new();
    for i in 0..n {
        d.insert((i * 7919) % n, i);
    }
    let mut total = 0_i64;
    for i in 0..n * 2 {
        total += match d.get(&i) {
            Some(v) => *v,
            None => 1,
        };
    }
    ((total + d.len() as i64) & 255) as i32
}

fn main() {
    std::process::exit(run());
}
