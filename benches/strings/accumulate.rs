// Misma carga que benches/strings/accumulate.sal.
fn main() {
    let mut acc = String::new();
    let piece = String::from("ab");
    for _ in 0..2_000_000 {
        acc = acc + &piece;
    }
    std::process::exit((std::hint::black_box(acc).len() & 255) as i32);
}
