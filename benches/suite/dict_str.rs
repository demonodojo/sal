//! Misma carga que `benches/suite/dict_str.sal`.
use std::collections::HashMap;

fn run() -> i32 {
    let mut d: HashMap<String, i64> = HashMap::new();
    for i in 0..std::hint::black_box(1_000_000_i64) {
        let k = (i % 5003).to_string();
        let c = match d.get(&k) {
            Some(v) => *v,
            None => 0,
        };
        d.insert(k, c + 1);
    }
    let ks: Vec<String> = d.keys().cloned().collect();
    let mut total = 0_usize;
    for k in &ks {
        total += k.len();
    }
    ((total + d.len()) & 255) as i32
}

fn main() {
    std::process::exit(run());
}
