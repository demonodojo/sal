// Misma carga que benches/strings/slice.sal.
fn main() {
    let s = String::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
    let mut total: usize = 0;
    for i in 0..2_000_000usize {
        let k = i & 31;
        let t = s[k..k + 16].to_string();
        total += std::hint::black_box(t).len();
    }
    std::process::exit((total & 255) as i32);
}
