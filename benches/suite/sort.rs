//! Misma carga que `benches/suite/sort.sal`.
fn quicksort(xs: &mut Vec<i64>, lo: i64, hi: i64) {
    if lo < hi {
        let p = xs[((lo + hi) / 2) as usize];
        let mut i = lo;
        let mut j = hi;
        while i <= j {
            while xs[i as usize] < p {
                i += 1;
            }
            while xs[j as usize] > p {
                j -= 1;
            }
            if i <= j {
                xs.swap(i as usize, j as usize);
                i += 1;
                j -= 1;
            }
        }
        quicksort(xs, lo, j);
        quicksort(xs, i, hi);
    }
}

fn run() -> i32 {
    let n: i64 = std::hint::black_box(2_000_000);
    let mut xs: Vec<i64> = Vec::new();
    let mut s: i64 = 12345;
    for _ in 0..n {
        s = (s.wrapping_mul(1103515245) + 12345) & 2147483647;
        xs.push(s);
    }
    quicksort(&mut xs, 0, n - 1);
    let mut ok = 1;
    for i in 1..n as usize {
        if xs[i - 1] > xs[i] {
            ok = 0;
        }
    }
    ((xs[(n / 2) as usize] + ok) & 255) as i32
}

fn main() {
    std::process::exit(run());
}
