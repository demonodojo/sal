// Misma carga que benches/strings/large.sal.
fn main() {
    let p = "x".repeat(4096);
    let q = "y".repeat(4096);
    let mut total: usize = 0;
    for _ in 0..20_000 {
        let s = p.clone() + &q + &p + &q;
        total += std::hint::black_box(s).len();
    }
    std::process::exit((total & 255) as i32);
}
