//! Misma carga que `benches/suite/scan.sal`.
fn run() -> i32 {
    let mut s = String::new();
    for _ in 0..std::hint::black_box(200_000) {
        s += "the quick brown fox jumps ";
    }
    let mut count = 0_i64;
    for _ in 0..5 {
        let b = s.as_bytes();
        let n = b.len();
        let mut i = 0;
        while i < n {
            let c = b[i];
            if c == 97 || c == 101 || c == 105 || c == 111 || c == 117 {
                count += 1;
            }
            i += 1;
        }
    }
    // `% 251` needs every bit of `count` (see scan.sal).
    (count % 251) as i32
}

fn main() {
    std::process::exit(run());
}
