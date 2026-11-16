//! Misma carga que `benches/suite/fib.sal`.
fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

fn run() -> i32 {
    (fib(std::hint::black_box(32)) & 255) as i32
}

fn main() {
    std::process::exit(run());
}
