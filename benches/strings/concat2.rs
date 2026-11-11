// Misma carga que benches/strings/concat2.sal.
fn main() {
    let a = String::from("abcdefghijklmnop");
    let b = String::from("qrstuvwxyz012345");
    let mut total: usize = 0;
    for _ in 0..2_000_000 {
        let s = a.clone() + &b;
        total += std::hint::black_box(s).len();
    }
    std::process::exit((total & 255) as i32);
}
