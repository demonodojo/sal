//! Misma carga que `benches/speed/struct_work.sal` (la constante N debe coincidir).

const N: i64 = 200_000_000;

#[derive(Clone, Copy)]
struct P {
    x: i64,
    y: i64,
    z: i64,
}

fn step(p: P, k: i64) -> P {
    let a = (p.x + k).wrapping_mul(31).wrapping_add(p.y);
    let b = (p.y ^ a).wrapping_add(p.z);
    P {
        x: b & 1048575,
        y: a & 1048575,
        z: (p.z + k) & 1048575,
    }
}

fn work(n: i64) -> i64 {
    let mut p = P { x: 1, y: 2, z: 3 };
    let mut i = 0_i64;
    while i < n {
        p = step(p, i);
        i += 1;
    }
    (p.x + p.y + p.z) & 255
}

fn main() {
    std::process::exit(work(N) as i32);
}
