// Misma carga que benches/strings/interp.sal.
fn main() {
    let (a, b, c) = ("user".to_string(), "42".to_string(), "ok".to_string());
    let mut total: usize = 0;
    for _ in 0..1_000_000 {
        let s = format!("{a}-{b}-{c}");
        total += std::hint::black_box(s).len();
    }
    std::process::exit((total & 255) as i32);
}
