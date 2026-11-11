// Misma carga que benches/strings/clone.sal.
fn main() {
    let s = String::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
    let mut total: usize = 0;
    for _ in 0..2_000_000 {
        let t = s.clone();
        total += std::hint::black_box(t).len();
    }
    std::process::exit((total & 255) as i32);
}
