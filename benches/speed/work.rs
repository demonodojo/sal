//! Misma carga que `benches/speed/work.sal` (constantes OUTER/INNER deben coincidir).

const OUTER: i64 = 6000;
const INNER: i64 = 6000;

fn mix(acc: i64, x: i64) -> i64 {
    let mut t = acc.wrapping_add(x);
    t = t.wrapping_mul(1664525).wrapping_add(1013904223);
    if t < 0 { -t } else { t }
}

fn work(outer: i64, inner: i64) -> i64 {
    let mut acc = 0_i64;
    let mut i = 0_i64;
    while i < outer {
        let mut j = 0_i64;
        while j < inner {
            acc = mix(acc, i + j);
            j += 1;
        }
        i += 1;
    }
    acc
}

fn main() {
    std::process::exit(work(OUTER, INNER) as i32);
}
