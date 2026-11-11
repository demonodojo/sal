// Misma carga que benches/strings/concat_many.sal.
fn main() {
    let (a, b, c, d) = ("alpha-".to_string(), "beta-".to_string(), "gamma-".to_string(), "delta-".to_string());
    let mut total: usize = 0;
    for _ in 0..500_000 {
        let s = a.clone() + &b + &c + &d + &a + &b + &c + &d;
        total += std::hint::black_box(s).len();
    }
    std::process::exit((total & 255) as i32);
}
