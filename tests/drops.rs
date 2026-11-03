//! Scope-end `free` of owned String locals (`src/drops.rs`).
//!
//! The IR tests pin where `sal_free` goes and where it must not (escapes);
//! the run test checks that memory stays bounded in a hot loop.
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ir_of(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("sal-drops-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sal"));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["emit", "ir"])
        .arg(&path)
        .current_dir(root())
        .output()
        .expect("sal emit ir");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn frees(ir: &str) -> usize {
    ir.matches("func: \"sal_free\"").count()
}

#[test]
fn local_read_by_builtins_is_freed_once_at_scope_end() {
    let ir = ir_of(
        "local_end",
        "fn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    str_len(s)\n",
    );
    assert_eq!(frees(&ir), 1, "{ir}");
    let call = ir.find("sal_str_len").expect("str_len call");
    let free = ir.find("sal_free").expect("free");
    let ret = ir.find("Return").expect("return");
    assert!(call < free && free < ret, "free must follow the use and precede return:\n{ir}");
}

#[test]
fn returned_local_is_not_freed() {
    let ir = ir_of(
        "returned",
        "fn make() -> String ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    s\n",
    );
    assert_eq!(frees(&ir), 0, "{ir}");
}

#[test]
fn local_stored_as_handle_is_not_freed() {
    let ir = ir_of(
        "handle",
        "fn main() -> Int ! alloc\n    v = vec_new()\n    s = str_slice(\"hello world\", 0, 5)\n    _p = vec_push(v, str_as_int(s))\n    vec_len(v)\n",
    );
    assert_eq!(frees(&ir), 0, "{ir}");
}

#[test]
fn local_passed_to_escaping_user_function_is_not_freed() {
    let ir = ir_of(
        "user_call",
        "fn eat(s: String) -> Int ! alloc\n    t = s\n    str_len(t)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    eat(s)\n",
    );
    assert_eq!(frees(&ir), 0, "{ir}");
}

#[test]
fn reassigned_local_is_not_freed() {
    let ir = ir_of(
        "reassigned",
        "fn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    s = str_slice(\"abcdef\", 0, 3)\n    str_len(s)\n",
    );
    assert_eq!(frees(&ir), 0, "{ir}");
}

#[test]
fn accumulated_local_is_freed_after_the_loop() {
    let ir = ir_of(
        "accumulate",
        "fn main() -> Int ! alloc\n    s = \"\"\n    i = 0\n    while i < 3\n        s = s + \"ab\"\n        i = i + 1\n    str_len(s)\n",
    );
    let free = ir.rfind("sal_free").expect("free after loop");
    let w = ir.find("While").expect("loop");
    assert!(free > w, "{ir}");
    // The accumulator is freed once, after the loop, not inside it.
    let while_line = ir.lines().find(|l| l.contains("While")).unwrap();
    assert!(!while_line.contains("args: [\"t10\"]") || while_line.matches("sal_free").count() == 1, "{ir}");
}

#[test]
fn loop_body_local_is_freed_every_iteration() {
    let ir = ir_of(
        "loop_body",
        "fn main() -> Int ! alloc\n    i = 0\n    n = 0\n    while i < 3\n        t = int_to_str(i)\n        n = n + str_len(t)\n        i = i + 1\n    n\n",
    );
    let while_line = ir.lines().find(|l| l.contains("While")).expect("loop");
    assert!(while_line.contains("sal_free"), "free must be inside the loop body:\n{ir}");
}

#[test]
fn early_return_frees_live_locals_after_computing_the_value() {
    let ir = ir_of(
        "early_return",
        "fn f(n: Int) -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    if n > 0\n        return str_len(s)\n    0\n",
    );
    assert!(frees(&ir) >= 2, "free on the early-return path and at the end:\n{ir}");
    let first_free = ir.find("sal_free").unwrap();
    let first_len = ir.find("sal_str_len").unwrap();
    assert!(first_len < first_free, "value computed before the free:\n{ir}");
}

#[test]
fn branch_local_is_freed_inside_its_branch() {
    let ir = ir_of(
        "branch",
        "fn f(n: Int) -> Int ! alloc\n    r = 0\n    if n > 0\n        t = int_to_str(n)\n        r = str_len(t)\n    r\n",
    );
    assert_eq!(frees(&ir), 1, "{ir}");
}

#[test]
fn hot_loop_runs_in_bounded_memory() {
    let src = root().join("tests/fixtures/drops_bounded.sal");
    let bin = std::env::temp_dir().join(format!("sal-drops-bounded-{}", std::process::id()));
    let built = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["build"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .current_dir(root())
        .output()
        .expect("sal build");
    assert_eq!(built.status.code(), Some(0), "{}", String::from_utf8_lossy(&built.stderr));
    // 3M iterations allocate several hundred MB in total; a 64 MB address-space
    // cap only holds if the strings are returned to the allocator.
    let run = Command::new("sh")
        .arg("-c")
        .arg(format!("ulimit -v 65536; exec {}", bin.display()))
        .output()
        .expect("run");
    let _ = std::fs::remove_file(&bin);
    assert_eq!(run.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&run.stderr));
}

#[test]
fn copy_element_list_and_bytes_are_freed() {
    let ir = ir_of(
        "containers",
        "fn f() -> Int ! alloc\n    xs = list_new[Int]()\n    _a = list_push(xs, 3)\n    b = bytes_new(4)\n    _c = bytes_set_u8(b, 0, 1)\n    list_len(xs) + bytes_get_u8(b, 0)\n",
    );
    assert!(ir.contains("func: \"sal_list_free\""), "{ir}");
    assert!(ir.contains("func: \"sal_bytes_free\""), "{ir}");
}

#[test]
fn list_push_alias_keeps_the_list_alive() {
    let ir = ir_of(
        "push_alias",
        "fn f() -> Int ! alloc\n    ys = list_new()\n    zs = list_push(ys, 1)\n    list_len(zs)\n",
    );
    assert!(!ir.contains("sal_list_free"), "{ir}");
}

#[test]
fn bytes_view_keeps_the_parent_alive() {
    let ir = ir_of(
        "bytes_view",
        "fn f() -> Int ! alloc\n    b = bytes_new(8)\n    v = bytes_view(b, 0, 4)\n    bytes_len(v)\n",
    );
    assert!(!ir.contains("sal_bytes_free"), "{ir}");
}

#[test]
fn string_list_is_freed() {
    let ir = ir_of(
        "string_list",
        "fn f() -> Int ! alloc\n    xs = list_new[String]()\n    list_len(xs)\n",
    );
    assert_eq!(ir.matches("sal_list_free").count(), 1, "{ir}");
}

#[test]
fn dict_is_freed_and_put_keeps_it_alive() {
    let ir = ir_of(
        "dict_local",
        "fn f() -> Bool ! alloc\n    d = dict_new[String, String]()\n    _a = dict_put(d, \"k\", \"v\")\n    dict_has(d, \"k\")\n",
    );
    assert_eq!(ir.matches("sal_dict_free").count(), 1, "{ir}");
}

#[test]
fn dict_with_owned_struct_values_is_not_freed() {
    let ir = ir_of(
        "dict_struct",
        "struct P\n    x: Int\n    y: Int\n\nfn f() -> Bool ! alloc\n    d = dict_new[String, P]()\n    dict_has(d, \"k\")\n",
    );
    assert!(!ir.contains("sal_dict_free"), "{ir}");
}

#[test]
fn enum_box_local_matched_in_place_is_freed() {
    let ir = ir_of(
        "enum_local",
        "enum Shape\n    Dot\n    Rect(Int, Int)\n\nfn f() -> Int ! alloc\n    s = Rect(3, 4)\n    match s\n        Rect(w, h) => w * h\n        Dot => 0\n",
    );
    assert_eq!(ir.matches("sal_enum_free").count(), 1, "{ir}");
}

#[test]
fn enum_box_returned_is_not_freed_but_read_only_arg_is() {
    let ir = ir_of(
        "enum_escape",
        "enum Shape\n    Dot\n    Rect(Int, Int)\n\nfn g(s: Shape) -> Int\n    0\n\nfn f() -> Int ! alloc\n    s = Rect(3, 4)\n    g(s)\n\nfn h() -> Shape ! alloc\n    s = Rect(1, 2)\n    s\n",
    );
    // `f` keeps `s` (g only reads it); `h` returns its box.
    assert_eq!(ir.matches("sal_enum_free").count(), 1, "{ir}");
}

#[test]
fn string_list_with_a_borrowed_value_is_not_freed() {
    let ir = ir_of(
        "string_list_tainted",
        "fn f(s: String) -> Int ! alloc\n    names = list_new[String]()\n    _a = list_push(names, s)\n    list_len(names)\n",
    );
    assert!(!ir.contains("sal_list_free"), "{ir}");
}

#[test]
fn string_list_of_fresh_values_is_freed() {
    let ir = ir_of(
        "string_list_fresh",
        "fn f(n: Int) -> Int ! alloc\n    names = list_new[String]()\n    _a = list_push(names, int_to_str(n))\n    _b = list_push(names, \"lit\")\n    list_len(names)\n",
    );
    assert_eq!(ir.matches("sal_list_free").count(), 1, "{ir}");
}

#[test]
fn string_passed_to_read_only_user_fn_is_freed() {
    let ir = ir_of(
        "ro_param",
        "fn size(s: String) -> Int\n    str_len(s)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    size(s)\n",
    );
    assert_eq!(frees(&ir), 1, "{ir}");
}

#[test]
fn string_passed_to_escaping_user_fn_is_not_freed() {
    let ir = ir_of(
        "escaping_param",
        "fn id(s: String) -> String\n    s\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    t = id(s)\n    str_len(t)\n",
    );
    // `t` is the result of a user call (unknown provenance); `s` moved into `id`.
    assert_eq!(frees(&ir), 0, "{ir}");
}

#[test]
fn read_only_flows_through_a_chain_of_calls() {
    let ir = ir_of(
        "ro_chain",
        "fn inner(s: String) -> Int\n    str_len(s)\n\nfn outer(s: String) -> Int\n    inner(s)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    outer(s)\n",
    );
    assert_eq!(frees(&ir), 1, "{ir}");
}

#[test]
fn recursion_keeps_read_only_optimistic() {
    let ir = ir_of(
        "ro_rec",
        "fn count(s: String, n: Int) -> Int\n    if n == 0\n        str_len(s)\n    else\n        count(s, n - 1)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    count(s, 3)\n",
    );
    assert_eq!(frees(&ir), 1, "{ir}");
}
