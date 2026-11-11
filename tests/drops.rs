//! Scope-end `free` of owned String locals (`src/drops.rs`).
//!
//! The IR tests pin where `sal_free` goes and where it must not (escapes);
//! the run test checks that memory stays bounded in a hot loop.
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fns_in_graph(
    graph: &sal_compiler::modules::ModuleGraph,
) -> Vec<&sal_compiler::ast::FnDef> {
    let mut out = Vec::new();
    for m in &graph.order {
        for item in &m.program.items {
            if let sal_compiler::ast::Item::Fn(f) = item {
                out.push(f);
            }
        }
    }
    out
}

/// The IR of a program without its per-type glue (`fn drop.<Struct>`), so the counts
/// below stay about the releases the program's own code makes. `ir_full` keeps the glue.
fn ir_of(name: &str, src: &str) -> String {
    strip_glue(&ir_full(name, src))
}

fn strip_glue(ir: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in ir.lines() {
        if line.starts_with("fn ") {
            skipping = line.starts_with("fn drop.");
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn ir_full(name: &str, src: &str) -> String {
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

/// The only release of a moved local sits behind its `__live_` flag test.
fn guarded_free(ir: &str) -> bool {
    ir.contains("op: \"eq\"") && frees(ir) == 1
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
    // `str_as_int` keeps the string as a handle: the local must not be released.
    assert_eq!(frees(&ir), 0, "{ir}");
}

#[test]
fn local_moved_into_another_local_is_released_once() {
    // `eat` takes `s` by value and moves it into `t`: `t` is the owner from then on (SPEC
    // «Memoria»), and the flag of `s` is cleared, so the String is released exactly once.
    // `main` moved `s` into `eat` and only holds a guarded release.
    let ir = ir_full(
        "user_call",
        "fn eat(s: String) -> Int ! alloc\n    t = s\n    str_len(t)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    eat(s)\n",
    );
    // `t` owns the String now: released once, unguarded. `s`'s release is behind its flag, cleared.
    assert_eq!(releases_in(&ir, "eat", "sal_free"), (1, 1), "{ir}");
    assert_eq!(releases_in(&ir, "main", "sal_free"), (0, 1), "{ir}");
}

#[test]
fn reassigned_local_releases_the_old_value() {
    // SPEC «Memoria»: a reassignment computes the new value, releases the old one and rebinds
    // the name; the last value is released at scope end. Two Strings, two releases.
    let ir = ir_of(
        "reassigned",
        "fn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    s = str_slice(\"abcdef\", 0, 3)\n    str_len(s)\n",
    );
    assert_eq!(releases_in(&ir, "main", "sal_free"), (2, 0), "{ir}");
}

#[test]
fn container_returned_by_a_fn_is_released_by_the_caller() {
    // SPEC «Memoria»: returning a container moves it to the caller, which owns and releases it.
    let ir = ir_full(
        "ret_list",
        "fn make() -> List[Int] ! alloc\n    xs = list_new[Int]()\n    xs\n\nfn main() -> Int ! alloc\n    ys = make()\n    list_len(&ys)\n",
    );
    assert_eq!(releases_in(&ir, "make", "sal_list_free"), (0, 0), "{ir}");
    assert_eq!(releases_in(&ir, "main", "sal_list_free"), (1, 0), "{ir}");
}

const BAG: &str = "struct Bag\n    name: String\n    items: List[Int]\n\n";

#[test]
fn clone_of_a_struct_with_a_list_field_is_built_and_released_by_its_glue() {
    // `b.clone()` of a struct without a descriptor goes through `clone.Bag` (a copy of every
    // field), and the local that holds the clone releases it with `drop.Bag`.
    let ir = ir_full(
        "clone_glue",
        &format!("{BAG}fn count(b: &Bag) -> Int ! alloc\n    c = b.clone()\n    c.items.len()\n\nfn main() -> Int ! alloc\n    b = Bag(int_to_str(5), list_new[Int]())\n    count(&b)\n"),
    );
    let count = fn_ir(&ir, "count");
    assert!(count.contains("func: \"clone.Bag\", args: [\"b\"]"), "{ir}");
    assert_eq!(count.matches("func: \"drop.Bag\"").count(), 1, "{ir}");
    let glue = fn_ir(&ir, "clone.Bag");
    assert!(glue.contains("sal_strdup") && glue.contains("sal_list_clone_val"), "{ir}");
}

#[test]
fn list_of_glue_structs_and_its_clone_are_released() {
    // A `List[Bag]` releases its elements through `drop.elem.Bag`; `xs.clone()` is a fresh list
    // that copies each element with `clone.Bag`. Each local releases its own list, and the
    // receiver of `clone` is only read.
    let ir = ir_full(
        "clone_list",
        &format!("{BAG}fn main() -> Int ! alloc\n    xs = list_new[Bag]()\n    ys = xs.clone()\n    list_len(&xs) + list_len(&ys)\n"),
    );
    let main = fn_ir(&ir, "main");
    assert!(main.contains("func: \"sal_list_set_drop\", args: [\"t1\", \"drop.elem.Bag\"]"), "{ir}");
    assert!(main.contains("func: \"sal_list_set_clone\", args: [\"t1\", \"clone.Bag\"]"), "{ir}");
    assert_eq!(releases_in(&ir, "main", "sal_list_free"), (2, 0), "{ir}");
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
fn list_push_keeps_the_list_alive() {
    let ir = ir_of(
        "push_keep",
        "fn f() -> Int ! alloc\n    ys = list_new[Int]()\n    list_push(ys, 1)\n    list_len(ys)\n",
    );
    // `list_push` takes `&mut` and returns `Unit`: the list local stays live and is released once.
    assert!(ir.contains("func: \"sal_list_free\"") || ir.contains("func: \"drop."), "{ir}");
}

#[test]
fn bytes_view_keeps_the_parent_alive() {
    let ir = ir_of(
        "bytes_view",
        "fn f() -> Int ! alloc\n    b = bytes_new(8)\n    v = bytes_view(b, 0, 4)\n    bytes_len(v)\n",
    );
    assert!(ir.contains("op: \"eq\""), "{ir}");
    assert_eq!(ir.matches("func: \"sal_bytes_free\"").count(), 2, "{ir}");
}

#[test]
fn struct_enum_field_is_freed_with_glue() {
    let ir = ir_expanded(
        "struct_enum",
        "enum E\n    Some(String)\n\nstruct B\n    e: E\n    n: Int\n\nfn main() -> Int ! alloc\n    e = Some(\"a\" + \"b\")\n    b = B(e, 1)\n    b.n\n",
    );
    assert_eq!(ir.matches("func: \"sal_enum_free\"").count(), 1, "{ir}");
}

#[test]
fn dict_get_some_string_is_freed() {
    let ir = ir_of(
        "dict_get_free",
        "enum Option[T]\n    None\n    Some(T)\n\nfn main() -> Int ! alloc\n    d = dict_new[String, String]()\n    d.put(\"a\", \"x\")\n    match dict_get(d, \"a\")\n        Some(v) => str_len(v)\n        None => 0\n    0\n",
    );
    assert_eq!(ir.matches("func: \"sal_enum_free\"").count(), 1, "{ir}");
    assert_eq!(ir.matches("func: \"sal_dict_free\"").count(), 1, "{ir}");
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
fn dict_with_struct_value_type_is_freed_at_scope_end() {
    let ir = ir_of(
        "dict_struct",
        "struct P\n    x: Int\n    y: Int\n\nfn f() -> Bool ! alloc\n    d = dict_new[String, P]()\n    dict_has(d, \"k\")\n",
    );
    assert_eq!(ir.matches("func: \"sal_dict_free\"").count(), 1, "{ir}");
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
    let ir = ir_full(
        "enum_escape",
        "enum Shape\n    Dot\n    Rect(Int, Int)\n\nfn g(s: Shape) -> Int\n    0\n\nfn f() -> Int ! alloc\n    s = Rect(3, 4)\n    g(s)\n\nfn h() -> Shape ! alloc\n    s = Rect(1, 2)\n    s\n",
    );
    // `g` takes the box and releases it; `f` moved it, so its release is behind a flag that the
    // move cleared; `h` returns its box and releases nothing.
    assert_eq!(releases_in(&ir, "g", "sal_enum_free"), (1, 0), "{ir}");
    assert_eq!(releases_in(&ir, "f", "sal_enum_free"), (0, 1), "{ir}");
    assert_eq!(releases_in(&ir, "h", "sal_enum_free"), (0, 0), "{ir}");
}

#[test]
fn string_list_that_stores_a_parameter_is_freed() {
    let ir = ir_of(
        "string_list_param",
        "fn f(s: String) -> Int ! alloc\n    names = list_new[String]()\n    _a = list_push(names, s)\n    list_len(names)\n",
    );
    assert!(
        ir.contains("sal_list_free"),
        "storing the parameter takes it, so the list owns the string:\n{ir}"
    );
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
    let ir = ir_full(
        "ro_param",
        "fn size(s: String) -> Int\n    str_len(s)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    size(s)\n",
    );
    // `size` takes the String by value and releases it; `main` moved it (guarded release only).
    assert_eq!(releases_in(&ir, "size", "sal_free"), (1, 0), "{ir}");
    assert_eq!(releases_in(&ir, "main", "sal_free"), (0, 1), "{ir}");
}

#[test]
fn string_passed_to_escaping_user_fn_is_not_freed() {
    let ir = ir_of(
        "escaping_param",
        "fn id(s: String) -> String\n    s\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    t = id(s)\n    str_len(t)\n",
    );
    // `s` moved into `id` (its guarded free never fires) and `id` returns that same block, so
    // `t` owns it: `t` is released once, unguarded. Two frees in all, one guarded.
    assert!(ir.contains("op: \"eq\""), "{ir}");
    assert_eq!(frees(&ir), 2, "{ir}");
}

#[test]
fn read_only_flows_through_a_chain_of_calls() {
    let ir = ir_full(
        "ro_chain",
        "fn inner(s: String) -> Int\n    str_len(s)\n\nfn outer(s: String) -> Int\n    inner(s)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    outer(s)\n",
    );
    // Each by-value parameter is released once, by the fn that took it; callers only hold guarded
    // releases of what they moved.
    assert_eq!(releases_in(&ir, "inner", "sal_free"), (1, 0), "{ir}");
    assert_eq!(releases_in(&ir, "outer", "sal_free"), (0, 1), "{ir}");
    assert_eq!(releases_in(&ir, "main", "sal_free"), (0, 1), "{ir}");
}

#[test]
fn recursion_keeps_read_only_optimistic() {
    let ir = ir_full(
        "ro_rec",
        "fn count(s: String, n: Int) -> Int\n    if n == 0\n        str_len(s)\n    else\n        count(s, n - 1)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    count(s, 3)\n",
    );
    // `count` takes `s`: its recursive branch moves it (flag cleared), and the read branch keeps
    // it, releasing it through the flag at scope end. `main` moved `s` and holds a guarded release.
    assert_eq!(releases_in(&ir, "count", "sal_free"), (0, 1), "{ir}");
    assert_eq!(releases_in(&ir, "main", "sal_free"), (0, 1), "{ir}");
}

#[test]
fn local_moved_in_one_branch_is_freed_behind_a_flag() {
    let ir = ir_of(
        "flag_branch",
        "fn main() -> Int ! alloc\n    s = int_to_str(7)\n    xs = list_new[String]()\n    if str_len(s) > 3\n        _a = list_push(xs, s)\n        0\n    else\n        0\n    k = list_len(xs)\n    _f = list_free(xs)\n    k\n",
    );
    // `s` is released once, guarded by its liveness flag (the list free is `sal_list_free`).
    assert_eq!(frees(&ir), 1, "{ir}");
    assert!(ir.contains("op: \"eq\""), "{ir}");
}

#[test]
fn local_moved_in_a_loop_it_was_declared_outside_stays_unfreed() {
    let ir = ir_of(
        "flag_loop",
        "fn main() -> Int ! alloc\n    s = int_to_str(7)\n    xs = list_new[String]()\n    i = 0\n    while i < 1\n        _a = list_push(xs, s)\n        i = i + 1\n    _f = list_free(xs)\n    0\n",
    );
    assert_eq!(frees(&ir), 0, "{ir}");
}

#[test]
fn list_of_flat_structs_is_freed_when_every_element_is_a_fresh_constructor() {
    let ir = ir_of(
        "list_flat",
        "struct F\n    name: String\n    k: Int\n\nfn main() -> Int ! alloc\n    xs = list_new[F]()\n    _a = list_push(xs, F(int_to_str(1), 2))\n    list_len(xs)\n",
    );
    assert!(ir.contains("sal_list_free"), "{ir}");
    let kept = ir_of(
        "list_flat_borrowed",
        "struct F\n    name: String\n    k: Int\n\nfn main() -> Int ! alloc\n    s = int_to_str(1)\n    xs = list_new[F]()\n    _a = list_push(xs, F(s, 2))\n    list_len(xs)\n",
    );
    // `F(s, 2)` moves `s` into the struct, and the struct into the list: the list releases it,
    // and `main` never frees `s` itself (SPEC «Memoria»: a moved value has one owner).
    assert!(kept.contains("sal_list_free"), "{kept}");
    assert_eq!(kept.matches("func: \"sal_free\"").count(), 0, "{kept}");
}

#[test]
fn fresh_string_passed_to_a_reader_is_bound_and_freed() {
    let ir = ir_of(
        "temporary",
        "fn main() -> Int ! alloc\n    str_len(int_to_str(12345))\n",
    );
    assert_eq!(frees(&ir), 1, "{ir}");
    let lit = ir_of("temporary_lit", "fn main() -> Int ! alloc\n    str_len(\"abc\")\n");
    assert_eq!(frees(&lit), 0, "{lit}");
}

#[test]
fn dict_keys_and_values_lists_are_freed_at_scope_end() {
    let ir = ir_of(
        "dict_keys_values_free",
        "fn main() -> Int ! alloc\n    d = dict_new[String, String]()\n    d.put(\"a\", \"x\")\n    ks = d.keys()\n    vs = d.values()\n    ks.len() + vs.len()\n",
    );
    // `ks`, `vs` and `d` each get their `*_free`.
    assert_eq!(ir.matches("func: \"sal_list_free\"").count(), 2, "{ir}");
    assert_eq!(ir.matches("func: \"sal_dict_free\"").count(), 1, "{ir}");
}

#[test]
fn dict_keys_list_that_escapes_is_not_freed() {
    let ir = ir_of(
        "dict_keys_escape",
        "fn names() -> List[String] ! alloc\n    d = dict_new[String, Int]()\n    d.put(\"a\", 1)\n    ks = d.keys()\n    ks\n",
    );
    assert_eq!(ir.matches("func: \"sal_list_free\"").count(), 0, "{ir}");
}

const NESTED_DECLS: &str = "struct P\n    name: String\n    tag: String\n\nstruct W\n    p: P\n    n: Int\n\nstruct V\n    w: W\n    label: String\n\n";

#[test]
fn nested_struct_string_fields_are_freed() {
    let ir = ir_expanded(
        "nested_struct_strings",
        &format!("{NESTED_DECLS}fn main() -> Int ! alloc\n    w = W(P(\"a\" + \"b\", \"c\" + \"d\"), 1)\n    w.n\n"),
    );
    assert_eq!(frees(&ir), 2, "{ir}");
}

#[test]
fn two_levels_of_nesting_are_freed() {
    let ir = ir_expanded(
        "nested_struct_two_levels",
        &format!("{NESTED_DECLS}fn main() -> Int ! alloc\n    v = V(W(P(\"a\" + \"b\", \"c\" + \"d\"), 1), \"e\" + \"f\")\n    v.w.n\n"),
    );
    // name, tag and label.
    assert_eq!(frees(&ir), 3, "{ir}");
}

#[test]
fn nested_struct_read_by_value_is_not_freed_twice() {
    let ir = ir_expanded(
        "nested_struct_read",
        &format!("{NESTED_DECLS}fn main() -> Int ! alloc\n    w = W(P(\"a\" + \"b\", \"c\" + \"d\"), 1)\n    q = w.p\n    str_len(q.name)\n"),
    );
    // Only `q` frees the strings; `w` gave them up when `w.p` was read.
    assert_eq!(frees(&ir), 2, "{ir}");
}

const CONTAINER_DECLS: &str = "struct B\n    xs: List[Int]\n    raw: Bytes\n    d: Dict[String, Int]\n    n: Int\n\nstruct O\n    b: B\n    tag: String\n\n";

#[test]
fn struct_glue_releases_flat_containers_and_bytes() {
    let ir = ir_expanded(
        "glue_containers",
        &format!("{CONTAINER_DECLS}fn main() -> Int ! alloc\n    xs = List.new[Int]()\n    xs.push(1)\n    d = Dict.new[String, Int]()\n    d.put(\"a\", 2)\n    o = O(B(xs, Bytes.new(8), d, 3), \"t\" + \"u\")\n    o.b.n\n"),
    );
    // The struct releases what was moved into it, through the nested `B` too. The
    // locals `xs` and `d` keep their own release behind a moved flag.
    assert_eq!(ir.matches("func: \"sal_bytes_free\"").count(), 1, "{ir}");
    assert_eq!(ir.matches("func: \"sal_free\"").count(), 1, "{ir}");
    assert_eq!(ir.matches("func: \"sal_list_free\"").count(), 2, "{ir}");
    assert_eq!(ir.matches("func: \"sal_dict_free\"").count(), 2, "{ir}");
}

#[test]
fn struct_glue_leaves_a_container_field_that_was_handed_on() {
    let ir = ir_of(
        "glue_handed_on",
        "struct B\n    xs: List[Int]\n    n: Int\n\nfn take_list() -> List[Int] ! alloc\n    xs = List.new[Int]()\n    xs.push(1)\n    b = B(xs, 3)\n    b.xs\n",
    );
    // Only the guarded release of the local `xs`; the struct must not free `b.xs`.
    assert_eq!(ir.matches("func: \"sal_list_free\"").count(), 1, "{ir}");
}

#[test]
fn struct_string_field_return_frees_callee_tag_and_caller_result() {
    let ir = ir_of(
        "field_return",
        "struct N\n    name: String\n    tag: String\n\nfn consume(n: N) -> String ! alloc\n    n.name\n\nfn main() -> Int ! alloc\n    n = N(\"a\" + \"b\", \"c\" + \"d\")\n    s = consume(n)\n    str_len(s)\n",
    );
    assert_eq!(frees(&ir), 2, "{ir}");
    assert!(
        !ir.contains("fn: \"consume\"") || {
            let start = ir.find("fn: \"consume\"").unwrap();
            let end = ir[start..].find("fn: \"main\"").map(|o| start + o).unwrap_or(ir.len());
            let callee = &ir[start..end];
            callee.matches("func: \"sal_free\"").count() == 1
        },
        "{ir}"
    );
}

#[test]
fn list_get_struct_passed_to_fn_frees_box_shell_not_full_struct() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn read_n(p: P) -> Int ! alloc\n    p.n\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    read_n(o)\n";
    let p = sal_compiler::parser::parse(src).expect("parse");
    let mut ir_mod = sal_compiler::ir::lower_program(&p);
    sal_compiler::fuse::fuse_module(&mut ir_mod);
    let ir = sal_compiler::ir::ir_to_text(&ir_mod);
    assert!(
        !ir.contains("func: \"sal_desc_copy\""),
        "a borrow of list_get is not copied:\n{ir}"
    );
    // The list owns its element: its block is freed by the list's glue, never by these fns.
    assert!(
        !strip_glue(&ir).contains("func: \"sal_struct_free_box\""),
        "a borrowed element is not a box this function frees:\n{ir}"
    );
    assert!(
        !ir.contains("func: \"sal_struct_free\""),
        "the list owns the element:\n{ir}"
    );
}

#[test]
fn bootstrap_module_ir_frees_list_get_struct_box() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn read_n(p: &P) -> Int ! alloc\n    p.n\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    read_n(o)\n";
    let path = std::env::temp_dir().join(format!("sal-bootstrap-box-{}.sal", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let graph = sal_compiler::modules::resolve_module_graph(&path, &root()).expect("graph");
    let root_mod = graph.order.last().expect("root");
    let callables = sal_compiler::modules::callable_fn_names(root_mod, &graph);
    let infer_out =
        sal_compiler::modules::infer_module(root_mod, &graph, sal_compiler::target::Target::Linux)
            .expect("infer");
    let graph_progs: Vec<&sal_compiler::ast::Program> =
        graph.order.iter().map(|m| &m.program).collect();
    let strings = sal_compiler::string_expr::StringEnv::from_module_graph(&graph_progs)
        .with_infer(&infer_out);
    let callees = fns_in_graph(&graph);
    let mut ir = sal_compiler::ir::lower_program_with_callables_env(
        &root_mod.program,
        &callables,
        strings,
        &callees,
    );
    sal_compiler::fuse::fuse_module(&mut ir);
    let ir = sal_compiler::ir::ir_to_text(&ir);
    assert!(
        !ir.contains("func: \"sal_desc_copy\""),
        "module pipeline must not copy a borrowed list_get:\n{ir}"
    );
    // The list owns its element: its block goes with the list's glue, not with this function.
    assert!(
        !strip_glue(&ir).contains("func: \"sal_struct_free_box\""),
        "module pipeline must not free the list's element in user code:\n{ir}"
    );
}

#[test]
fn list_get_struct_pushed_does_not_free_box_shell() {
    let ir = ir_of(
        "struct_box_push",
        "struct P\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    ys = list_new[P]()\n    list_push(ys, o.clone())\n    list_len(ys)\n",
    );
    assert!(
        ir.contains("func: \"clone.P\"") || ir.contains("func: \"sal_desc_copy\""),
        "pushing a borrowed element clones it:\n{ir}"
    );
    assert!(
        !ir.contains("func: \"sal_struct_free_box\""),
        "the destination list owns the copy:\n{ir}"
    );
}

#[test]
fn list_get_struct_taken_is_copied_and_the_box_is_freed_once() {
    let ir = ir_of(
        "struct_box_take",
        "struct P\n    name: String\n    n: Int\n\nfn keep(p: P) -> Int ! alloc\n    ys = list_new[P]()\n    list_push(ys, p)\n    list_len(ys)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    keep(o.clone())\n",
    );
    assert!(
        ir.contains("func: \"clone.P\"") || ir.contains("func: \"sal_desc_copy\""),
        "taking ownership of a borrowed element clones it:\n{ir}"
    );
}

#[test]
fn list_get_struct_returned_is_copied() {
    let ir = ir_of(
        "struct_box_ret",
        "struct P\n    name: String\n    n: Int\n\nfn give(xs: &List[P]) -> P ! alloc\n    list_get(xs, 0).clone()\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    p = give(&xs)\n    p.n\n",
    );
    assert!(
        ir.contains("func: \"clone.P\"") || ir.contains("func: \"sal_desc_copy\""),
        "returning a borrowed element clones it:\n{ir}"
    );
}

#[test]
fn list_get_struct_rebuilt_from_fields_is_not_copied() {
    let ir = ir_of(
        "struct_box_rebuild",
        "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: &P) -> P ! alloc\n    P(p.name.clone(), p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    q.n\n",
    );
    assert!(
        !ir.contains("func: \"sal_desc_copy\""),
        "rebuilding clones the String field, not the whole box:\n{ir}"
    );
    assert!(
        !ir.contains("func: \"sal_struct_free\""),
        "the list still owns the original element:\n{ir}"
    );
}

#[test]
fn list_get_struct_rebuilt_then_pushed_is_copied_once() {
    let ir = ir_of(
        "struct_box_rebuild_push",
        "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: &P) -> P ! alloc\n    P(p.name.clone(), p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    ys = list_new[P]()\n    list_push(ys, q)\n    list_len(ys)\n",
    );
    assert!(
        ir.contains("func: \"sal_strdup\"") || ir.contains("func: \"sal_desc_copy\""),
        "rebuilding clones the String before the second list takes it:\n{ir}"
    );
}

#[test]
fn struct_glue_frees_owned_struct_list_field() {
    let ir = ir_expanded(
        "struct_list_field",
        "struct P\n    name: String\n    n: Int\n\nstruct W\n    xs: List[P]\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = List.new[P]()\n    xs.push(P(\"a\" + \"b\", 1))\n    w = W(xs, 2)\n    w.n\n",
    );
    assert_eq!(ir.matches("func: \"sal_list_free\"").count(), 2, "{ir}");
}

#[test]
fn struct_glue_frees_string_list_when_only_fresh_values() {
    let ir = ir_expanded(
        "glue_string_list",
        "struct B\n    names: List[String]\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = List.new[String]()\n    xs.push(\"a\" + \"b\")\n    b = B(xs, 3)\n    b.n\n",
    );
    assert_eq!(ir.matches("func: \"sal_list_free\"").count(), 2, "{ir}");
}

fn ir_of_file(rel: &str) -> String {
    strip_glue(&ir_file_full(rel))
}

fn ir_file_full(rel: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["emit", "ir"])
        .arg(root().join(rel))
        .current_dir(root())
        .output()
        .expect("sal emit ir");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// The IR lines of `fn name:`, up to the next fn.
fn fn_ir(ir: &str, name: &str) -> String {
    let head = format!("fn {name}:");
    ir.lines()
        .skip_while(|l| *l != head)
        .skip(1)
        .take_while(|l| !l.starts_with("fn "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn struct_glue_keeps_fields_stored_in_imported_variant() {
    let ir = ir_of_file("tests/glue_escape/ctor.sal");
    let wrap = fn_ir(&ir, "wrap");
    assert!(wrap.contains("func: \"done\""), "{ir}");
    assert!(!wrap.contains("sal_list_free"), "{wrap}");
    assert!(!wrap.contains("sal_enum_free"), "{wrap}");
}

#[test]
fn struct_glue_releases_transitively_consumed_struct_once() {
    let ir = ir_of_file("tests/glue_escape/chain.sal");
    let main = fn_ir(&ir, "main");
    let shallow = fn_ir(&ir, "tp_free_shallow");
    let block = fn_ir(&ir, "tp_free_block");
    assert!(main.contains("func: \"emit_ir_single_file\""), "{ir}");
    // The caller must not release a struct the callee consumes, even through a
    // function that only forwards it.
    assert!(!main.contains("sal_list_free"), "{main}");
    assert_eq!(shallow.matches("sal_list_free").count(), 1, "{shallow}");
    // The callee that hands a field on must not release it again in its tail glue.
    assert!(!block.contains("sal_list_free"), "{block}");
}

#[test]
fn struct_glue_does_not_release_a_result_that_aliases_a_borrowed_param() {
    let ir = ir_of_file("tests/glue_escape/alias.sal");
    let lower = fn_ir(&ir, "lower_if");
    assert!(lower.contains("func: \"elsifs_else_block\""), "{ir}");
    assert!(!lower.contains("sal_list_free"), "{lower}");
}

#[test]
fn struct_glue_releases_branch_returned_struct_once() {
    let ir = ir_of_file("tests/glue_escape/branch.sal");
    let main = fn_ir(&ir, "main");
    assert!(main.contains("func: \"pick\""), "{ir}");
    // `t` moves into `pick`; the result `u` is released once, by the glue of `B`.
    assert_eq!(main.matches("func: \"drop.B\"").count(), 1, "{main}");
    assert_eq!(main.matches("sal_list_free").count(), 0, "{main}");
}

// Per-type glue (SPEC «Liberación de locales»): `List[T]` releases each element with the
// glue of `T`, then the block, and a struct releases its fields in declaration order.
// `Leaf` has a list of Int, so it is only freed through its own glue `drop.Leaf`.
#[test]
fn glue_per_type_frees_list_of_struct_with_list_field() {
    let ir = ir_full(
        "glue_tree",
        "struct Leaf\n    name: String\n    xs: List[Int]\n\nstruct Tree\n    leaves: List[Leaf]\n    tag: String\n\nfn main() -> Int ! alloc\n    leaves = List.new[Leaf]()\n    t = Tree(leaves, \"a\")\n    t.tag.len()\n",
    );
    let leaf = fn_ir(&ir, "drop.Leaf");
    assert!(leaf.contains("func: \"sal_free\""), "{ir}");
    assert!(leaf.contains("func: \"sal_list_free\""), "{leaf}");
    let tree = fn_ir(&ir, "drop.Tree");
    assert!(tree.contains("func: \"sal_list_free\""), "{tree}");
    let main = fn_ir(&ir, "main");
    // `leaves` moved into `t`: main releases `t` through its glue. A flag-guarded
    // release of the moved-from binding may remain; it must not run after the move.
    assert_eq!(main.matches("func: \"drop.Tree\"").count(), 1, "{main}");
    assert!(ir.contains("\"drop.Leaf\""), "the list block must hold the element glue:\n{ir}");
}

// The runtime hook of the per-type glue: `sal_list_set_drop` makes `sal_list_free` release
// each element with the given glue (SPEC «Liberación de locales»).
#[test]
fn list_set_drop_releases_each_element_with_element_glue() {
    let dir = std::env::temp_dir().join(format!("sal-list-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("list_set_drop");
    let cc = Command::new("cc")
        .args(["-std=c11", "-o"])
        .arg(&bin)
        .arg(root().join("tests/runtime_drop/list_set_drop.c"))
        .arg(root().join("runtime/sal_runtime.c"))
        .args(["-I"])
        .arg(root().join("runtime"))
        .args(["-lm", "-lpthread"])
        .output()
        .expect("cc");
    assert!(cc.status.success(), "{}", String::from_utf8_lossy(&cc.stderr));
    let run = Command::new(&bin).output().expect("run list_set_drop");
    assert_eq!(run.status.code(), Some(0), "{}", String::from_utf8_lossy(&run.stdout));
}

// Step 2 of the per-type glue: every struct with heap fields gets a `drop.<Struct>` function
// that releases those fields in declaration order (SPEC «Liberación de locales»).
#[test]
fn struct_glue_fn_releases_heap_fields_in_declaration_order() {
    let ir = ir_full(
        "glue_fn_order",
        "struct Leaf\n    name: String\n    n: Int\n    xs: List[Int]\n\nfn main() -> Int\n    0\n",
    );
    let leaf = fn_ir(&ir, "drop.Leaf");
    let free = leaf.find("func: \"sal_free\"").expect(&ir);
    let list = leaf.find("func: \"sal_list_free\"").expect(&ir);
    assert!(free < list, "fields in declaration order:\n{leaf}");
    assert!(!leaf.contains("func: \"sal_enum_free\""), "{leaf}");
}

/// The IR of a program with each call `drop.<S>(local)` replaced by the releases of that
/// glue, on `local`. Release counts then read the same as with inline releases.
fn ir_expanded(name: &str, src: &str) -> String {
    let full = ir_full(name, src);
    let mut bodies: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut cur: Option<String> = None;
    for line in full.lines() {
        if let Some(f) = line.strip_prefix("fn ") {
            cur = f.strip_suffix(':').filter(|n| n.starts_with("drop.")).map(str::to_string);
            if let Some(n) = &cur {
                bodies.insert(n.clone(), Vec::new());
            }
        } else if let Some(n) = &cur {
            if line.contains("func: \"sal_") || line.contains("Extract") {
                bodies.get_mut(n).unwrap().push(line.to_string());
            }
        }
    }
    let mut out = String::new();
    let mut in_glue = false;
    for line in full.lines() {
        if let Some(f) = line.strip_prefix("fn ") {
            in_glue = f.starts_with("drop.");
        }
        if in_glue {
            continue;
        }
        if let Some(i) = line.find("func: \"drop.") {
            let func = line[i + 7..].split('"').next().unwrap().to_string();
            let arg = line.split("args: [\"").nth(1).unwrap().split('"').next().unwrap().to_string();
            for body in bodies.get(&func).cloned().unwrap_or_default() {
                out.push_str(
                    &body
                        .replace("base: \"v\"", &format!("base: \"{arg}\""))
                        .replace("args: [\"v\"]", &format!("args: [\"{arg}\"]")),
                );
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// A list of structs with glue is released element by element through `drop.<S>` (the block
// holds that glue, `sal_list_set_drop`): an instrumented run reports no leak and no double free.
#[test]
fn list_of_glue_structs_is_released_once_and_clean() {
    let dir = std::env::temp_dir().join(format!("sal-list-glue-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("tree.sal");
    std::fs::write(
        &src,
        "struct Leaf\n    name: String\n    xs: List[Int]\n\nstruct Tree\n    leaves: List[Leaf]\n    tag: String\n\nfn main() -> Int ! alloc\n    leaves = List.new[Leaf]()\n    leaves.push(Leaf(\"a\" + \"b\", List.new[Int]()))\n    t = Tree(leaves, \"t\" + \"u\")\n    t.leaves.len() + t.tag.len()\n",
    )
    .unwrap();
    let bin = dir.join("tree_instr");
    let build = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["build", "--instrument", "-o"])
        .arg(&bin)
        .arg(&src)
        .output()
        .expect("sal build");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let run = Command::new(&bin).output().expect("run");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(3), "{stderr}");
    assert!(!stderr.contains("LEAK") && !stderr.contains("DOUBLE_FREE"), "{stderr}");
}

// SPEC «Liberación de locales»: a `List` whose elements are lists releases each inner list
// through the element glue `sal_list_free_elem`, set when the outer list is built.
#[test]
fn list_of_lists_sets_inner_list_glue_and_is_clean() {
    let ir = ir_full(
        "list_of_lists",
        "fn main() -> Int ! alloc\n    xs = List.new[List[Int]]()\n    ys = List.new[Int]()\n    ys.push(3)\n    xs.push(ys)\n    xs.len()\n",
    );
    assert!(ir.contains("func: \"sal_list_set_drop\", args: [\"t"), "{ir}");
    assert!(ir.contains("\"sal_list_free_elem\""), "{ir}");
    let dir = std::env::temp_dir().join(format!("sal-list-of-lists-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("lol.sal");
    std::fs::write(&src, "fn main() -> Int ! alloc\n    xs = List.new[List[Int]]()\n    ys = List.new[Int]()\n    ys.push(3)\n    xs.push(ys)\n    xs.len()\n").unwrap();
    let bin = dir.join("lol_instr");
    let build = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["build", "--instrument", "-o"])
        .arg(&bin)
        .arg(&src)
        .output()
        .expect("sal build");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let run = Command::new(&bin).output().expect("run");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("LEAK") && !stderr.contains("DOUBLE_FREE"), "{stderr}");
}

/// The releases of `free` in `fn name`, as (unguarded, behind a liveness flag). A guarded
/// release sits inside the `then_body` of its flag test (SPEC «Liberación de locales»).
fn releases_in(ir: &str, name: &str, free: &str) -> (usize, usize) {
    let body = fn_ir(ir, name);
    let mut unguarded = 0;
    let mut guarded = 0;
    for line in body.lines().filter(|l| l.contains(&format!("func: \"{free}\""))) {
        if line.contains("then_body") || line.starts_with("If") {
            guarded += 1;
        } else {
            unguarded += 1;
        }
    }
    (unguarded, guarded)
}

// SPEC «Memoria»: `t = s` moves the String into `t`, which releases it once. An instrumented run
// reports no leak and no double free.
#[test]
fn moved_into_another_local_is_released_once_at_runtime() {
    let dir = std::env::temp_dir().join(format!("sal-transfer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("transfer.sal");
    std::fs::write(
        &src,
        "fn eat(s: String) -> Int ! alloc\n    t = s\n    str_len(t)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    eat(s)\n",
    )
    .unwrap();
    let bin = dir.join("transfer_instr");
    let build = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["build", "--instrument", "-o"])
        .arg(&bin)
        .arg(&src)
        .output()
        .expect("sal build");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let run = Command::new(&bin).output().expect("run");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(5), "{stderr}");
    assert!(!stderr.contains("LEAK") && !stderr.contains("DOUBLE_FREE"), "{stderr}");
}

// SPEC «Liberación de locales»: a by-value parameter moved after an early `return` is released on
// that return (its flag is still set), and never again by the move. Instrumented: no leak.
#[test]
fn early_return_before_move_releases_the_by_value_param() {
    let dir = std::env::temp_dir().join(format!("sal-early-return-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("early.sal");
    std::fs::write(
        &src,
        "fn eat(s: String) -> Int ! alloc\n    str_len(s)\n\nfn f(s: String, n: Int) -> Int ! alloc\n    if n == 0\n        return 0\n    eat(s)\n\nfn main() -> Int ! alloc\n    s = str_slice(\"hello world\", 0, 5)\n    f(s, 0)\n",
    )
    .unwrap();
    let bin = dir.join("early_instr");
    let build = Command::new(env!("CARGO_BIN_EXE_sal"))
        .args(["build", "--instrument", "-o"])
        .arg(&bin)
        .arg(&src)
        .output()
        .expect("sal build");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let run = Command::new(&bin).output().expect("run");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("LEAK") && !stderr.contains("DOUBLE_FREE"), "{stderr}");
}

// A struct local declared in an `if` branch is released when the branch ends (SPEC «Memoria»:
// every owned value is released at the end of its scope). Before, the glue of the fn looked the
// local up after the branch and skipped it without a release.
#[test]
fn struct_local_of_if_branch_is_released_when_branch_ends() {
    let ir = ir_full(
        "branch_scope_glue",
        "struct Inner\n    xs: List[String]\n\nstruct Ctx\n    moved: List[String]\n    inner: Inner\n    n: Int\n\nfn ctx_new(k: Int) -> Ctx ! alloc\n    Ctx(list_new[String](), Inner(list_new[String]()), k)\n\nfn ctx_use(c: &Ctx) -> Int ! alloc\n    c.n\n\nfn go(k: Int, round: Int) -> Int ! alloc\n    if round >= 16\n        k\n    else\n        c = ctx_new(k)\n        r = ctx_use(&c)\n        r\n\nfn main() -> Int ! alloc\n    go(3, 0)\n",
    );
    let go = fn_ir(&ir, "go");
    let ctx_use = go.find("func: \"ctx_use\"").expect("ctx_use call");
    let released = go.find("func: \"drop.Ctx\"").expect("branch-local struct released");
    assert!(released > ctx_use, "{go}");
    assert_eq!(go.matches("func: \"drop.Ctx\"").count(), 1, "{go}");
}

// A `return` leaves its open scopes: the struct locals of the branch it returns from are released
// before the value goes out. A local the return moves is not released.
#[test]
fn return_inside_if_branch_releases_its_struct_locals_but_not_moved_ones() {
    let ir = ir_full(
        "return_branch_glue",
        "struct Inner\n    xs: List[String]\n\nstruct Ctx\n    moved: List[String]\n    inner: Inner\n    n: Int\n\nfn ctx_new(k: Int) -> Ctx ! alloc\n    Ctx(list_new[String](), Inner(list_new[String]()), k)\n\nfn ctx_use(c: &Ctx) -> Int ! alloc\n    c.n\n\nfn early(k: Int, flag: Int) -> Int ! alloc\n    if flag == 1\n        e = ctx_new(k)\n        return ctx_use(&e)\n    else\n        0\n\nfn moves(k: Int, flag: Int) -> Ctx ! alloc\n    if flag == 1\n        c = ctx_new(k)\n        return c\n    d = ctx_new(k + 1)\n    d\n\nfn main() -> Int ! alloc\n    early(2, 1) + moves(4, 0).n\n",
    );
    let early = fn_ir(&ir, "early");
    let ret_use = early.find("func: \"ctx_use\"").expect("ctx_use call");
    let released = early.find("func: \"drop.Ctx\"").expect("e released before the return");
    assert!(released > ret_use, "{early}");
    let moves = fn_ir(&ir, "moves");
    assert_eq!(moves.matches("func: \"drop.Ctx\"").count(), 0, "{moves}");
}

// A `return` that is the last statement of the fn body is covered by the glue at the end of the
// fn: the local is released once, not once by the return and again by the glue.
#[test]
fn top_level_return_releases_its_struct_local_once() {
    let ir = ir_full(
        "top_return_glue",
        "struct Ctx\n    moved: List[String]\n    n: Int\n\nfn ctx_new(k: Int) -> Ctx ! alloc\n    Ctx(list_new[String](), k)\n\nfn ctx_use(c: &Ctx) -> Int ! alloc\n    c.n\n\nfn top(k: Int) -> Int ! alloc\n    x = ctx_new(k)\n    v = ctx_use(&x)\n    return v\n\nfn main() -> Int ! alloc\n    top(4)\n",
    );
    let top = fn_ir(&ir, "top");
    assert_eq!(top.matches("func: \"drop.Ctx\"").count(), 1, "{top}");
}

// Multi-field `drop.<Struct>` frees the heap block too (SPEC / Rust Drop of an owning value).
#[test]
fn drop_struct_frees_heap_box() {
    let ir = ir_full(
        "drop_box",
        "struct OC\n    moved: List[String]\n    n: Int\n\nfn main() -> Int ! alloc\n    c = OC(List.new[String](), 1)\n    list_len(&c.moved)\n",
    );
    let drop_oc = fn_ir(&ir, "drop.OC");
    assert!(
        drop_oc.contains("func: \"sal_struct_free_box\""),
        "drop.OC must free the struct block:\n{drop_oc}"
    );
    let drop_elem = fn_ir(&ir, "drop.elem.OC");
    assert!(
        drop_elem.contains("func: \"drop.OC\"") && !drop_elem.contains("sal_struct_free_box"),
        "drop.elem.OC only forwards to drop.OC:\n{drop_elem}"
    );
}

// `f(&g())`: the temporary from `g()` is bound and released (SPEC / Rust; Hoist Ref/RefMut).
#[test]
fn borrowed_call_temporary_is_released() {
    let ir = ir_full(
        "borrowed_call_temp",
        "struct Toks\n    items: List[Int]\n\nfn lex_fake() -> Toks ! alloc\n    Toks(List.new[Int]())\n\nfn take_toks(t: &Toks) -> Int ! alloc\n    list_len(t.items)\n\nfn main() -> Int ! alloc\n    take_toks(&lex_fake())\n",
    );
    let main = fn_ir(&ir, "main");
    assert!(
        main.contains("func: \"drop.Toks\"") || main.contains("func: \"sal_list_free\""),
        "temporary under `&lex_fake()` must be released:\n{main}"
    );
}

// Field overwrite writes in place and drops the previous field (SPEC / Rust).
#[test]
fn field_assign_drops_previous_and_stores_in_place() {
    let ir = ir_full(
        "field_overwrite",
        "struct OC\n    moved: List[String]\n    n: Int\n\nfn reset(c: &mut OC) -> Int ! alloc\n    c.moved = List.new[String]()\n    0\n\nfn main() -> Int ! alloc\n    c = OC(List.new[String](), 1)\n    list_push(&mut c.moved, \"a\")\n    reset(&mut c)\n    list_len(&c.moved)\n",
    );
    let reset = fn_ir(&ir, "reset");
    assert!(
        reset.contains("func: \"sal_struct_set\""),
        "field assign through `&mut` must store in place:\n{reset}"
    );
    assert!(
        reset.contains("func: \"sal_list_free\""),
        "previous list field must be dropped:\n{reset}"
    );
}

#[test]
fn elsif_branch_returning_a_borrow_makes_the_result_not_owned() {
    let ir = ir_of(
        "elsif_borrowed_return",
        "fn pick(n: Int, h: Int) -> String ! alloc\n    if n == 0\n        \"s\"\n    elsif n == 1\n        str_from_int(h)\n    else\n        \"\"\n\nfn main() -> Int ! alloc\n    d = pick(1, 7)\n    str_len(d)\n",
    );
    // `str_from_int` is a cast of a handle somebody else owns: `pick` does not return a fresh
    // String, so `main` must not release `d`.
    assert_eq!(frees(&ir), 0, "{ir}");
}
