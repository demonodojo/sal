//! Misma carga que `benches/suite/points.sal`.
#[derive(Clone, Copy)]
struct Pt {
    x: i64,
    y: i64,
    vx: i64,
    vy: i64,
}

impl Pt {
    fn moved(&self) -> Pt {
        Pt { x: (self.x + self.vx) & 65535, y: (self.y + self.vy) & 65535, vx: self.vx, vy: self.vy }
    }
    fn energy(&self) -> i64 {
        self.vx * self.vx + self.vy * self.vy
    }
}

fn run() -> i32 {
    let n: i64 = std::hint::black_box(100_000);
    let mut ps: Vec<Pt> = Vec::new();
    for i in 0..n {
        ps.push(Pt { x: i, y: i * 3, vx: (i % 7) - 3, vy: (i % 5) - 2 });
    }
    for _ in 0..50 {
        for i in 0..n as usize {
            ps[i] = ps[i].moved();
        }
    }
    let mut total = 0_i64;
    for p in &ps {
        total += p.x + p.y + p.energy();
    }
    (total & 255) as i32
}

fn main() {
    std::process::exit(run());
}
