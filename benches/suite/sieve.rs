//! Misma carga que `benches/suite/sieve.sal`.
fn run() -> i32 {
    let n: usize = std::hint::black_box(10_000_000);
    let mut flags: Vec<bool> = Vec::new();
    for _ in 0..n {
        flags.push(true);
    }
    let mut count = 0_i64;
    let mut i = 2;
    while i < n {
        if flags[i] {
            count += 1;
            let mut j = i * i;
            while j < n {
                flags[j] = false;
                j += i;
            }
        }
        i += 1;
    }
    (count & 255) as i32
}

fn main() {
    std::process::exit(run());
}
