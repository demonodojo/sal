// Misma carga que benches/strings/accumulate_multi.sal.
fn main() {
    let mut acc = String::new();
    let (piece, sep) = (String::from("item"), String::from(","));
    for _ in 0..1_000_000 {
        acc = acc + &piece + &sep;
    }
    std::process::exit((std::hint::black_box(acc).len() & 255) as i32);
}
