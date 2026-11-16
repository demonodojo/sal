//! Misma carga que `benches/suite/tree.sal`.
enum Tree {
    Leaf,
    Node(Box<Tree>, Box<Tree>),
}

fn make(d: i64) -> Tree {
    if d == 0 {
        Tree::Leaf
    } else {
        Tree::Node(Box::new(make(d - 1)), Box::new(make(d - 1)))
    }
}

fn check(t: &Tree) -> i64 {
    match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => 1 + check(l) + check(r),
    }
}

fn run() -> i32 {
    let mut total = 0_i64;
    for _ in 0..std::hint::black_box(20) {
        let t = make(16);
        total += check(&t);
    }
    (total & 255) as i32
}

fn main() {
    std::process::exit(run());
}
