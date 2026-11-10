//! Misma carga que `benches/mem/node.sal` (la constante N debe coincidir).

const N: i64 = 1_000_000;

struct Node {
    id: i64,
    name: String,
}

fn build(n: i64) -> Vec<Node> {
    let mut xs = Vec::new();
    let mut i = 0_i64;
    while i < n {
        xs.push(Node {
            id: i,
            name: (i & 1023).to_string(),
        });
        i += 1;
    }
    xs
}

fn main() {
    let xs = build(N);
    let mut total = 0_i64;
    let mut i = 0_usize;
    while i < xs.len() {
        total += xs[i].id + xs[i].name.len() as i64;
        i += 1;
    }
    std::process::exit((total & 255) as i32);
}
