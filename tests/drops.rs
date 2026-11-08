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
    // The statement that stores `s` clears its flag, so the guarded free never runs.
    assert!(guarded_free(&ir), "{ir}");
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
    // `ys` moved into `zs`: its flag is cleared, so only the guarded free remains.
    assert!(ir.contains("op: \"eq\""), "{ir}");
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
    let ir = ir_of(
        "enum_escape",
        "enum Shape\n    Dot\n    Rect(Int, Int)\n\nfn g(s: Shape) -> Int\n    0\n\nfn f() -> Int ! alloc\n    s = Rect(3, 4)\n    g(s)\n\nfn h() -> Shape ! alloc\n    s = Rect(1, 2)\n    s\n",
    );
    // `f` keeps `s` (g only reads it); `h` returns its box.
    assert_eq!(ir.matches("sal_enum_free").count(), 1, "{ir}");
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
    // `t` is the result of a user call (unknown provenance); `s` moved into `id`, which
    // clears its flag before the guarded free.
    assert!(guarded_free(&ir), "{ir}");
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
    assert!(!kept.contains("sal_list_free"), "{kept}");
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
    assert!(
        !ir.contains("func: \"sal_struct_free_box\""),
        "a borrowed element is not a box this function frees:\n{ir}"
    );
    assert!(
        !ir.contains("func: \"sal_struct_free\""),
        "the list owns the element:\n{ir}"
    );
}

#[test]
fn bootstrap_module_ir_frees_list_get_struct_box() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn read_n(p: P) -> Int ! alloc\n    p.n\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    read_n(o)\n";
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
    assert!(
        !ir.contains("func: \"sal_struct_free_box\""),
        "module pipeline must not free the list's element:\n{ir}"
    );
}

#[test]
fn list_get_struct_pushed_does_not_free_box_shell() {
    let ir = ir_of(
        "struct_box_push",
        "struct P\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    ys = list_new[P]()\n    list_push(ys, o)\n    list_len(ys)\n",
    );
    assert!(
        ir.contains("func: \"sal_desc_copy\""),
        "pushing a borrowed element copies it:\n{ir}"
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
        "struct P\n    name: String\n    n: Int\n\nfn keep(p: P) -> Int ! alloc\n    ys = list_new[P]()\n    list_push(ys, p)\n    list_len(ys)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    keep(o)\n",
    );
    assert_eq!(ir.matches("func: \"sal_desc_copy\"").count(), 1, "{ir}");
    assert_eq!(ir.matches("func: \"sal_struct_free_box\"").count(), 1, "{ir}");
}

#[test]
fn list_get_struct_returned_is_copied() {
    let ir = ir_of(
        "struct_box_ret",
        "struct P\n    name: String\n    n: Int\n\nfn give(xs: List[P]) -> P ! alloc\n    list_get(xs, 0)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    p = give(xs)\n    p.n\n",
    );
    assert!(
        ir.contains("func: \"sal_desc_copy\""),
        "returning a borrowed element copies it:\n{ir}"
    );
}

#[test]
fn list_get_struct_rebuilt_from_fields_is_not_copied() {
    let ir = ir_of(
        "struct_box_rebuild",
        "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: P) -> P ! alloc\n    P(p.name, p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    q.n\n",
    );
    assert!(
        !ir.contains("func: \"sal_desc_copy\""),
        "rebuilding from borrowed fields shares them:\n{ir}"
    );
    assert!(
        !ir.contains("func: \"sal_struct_free\""),
        "the alias is not freed; the list owns the fields:\n{ir}"
    );
}

#[test]
fn list_get_struct_rebuilt_then_pushed_is_copied_once() {
    let ir = ir_of(
        "struct_box_rebuild_push",
        "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: P) -> P ! alloc\n    P(p.name, p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    ys = list_new[P]()\n    list_push(ys, q)\n    list_len(ys)\n",
    );
    assert_eq!(
        ir.matches("func: \"sal_desc_copy\"").count(),
        1,
        "the second list copies the alias once:\n{ir}"
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
    assert_eq!(main.matches("sal_list_free").count(), 1, "{main}");
    assert_eq!(main.matches("sal_enum_free").count(), 1, "{main}");
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
    // `leaves` moved into `t`: main releases `t` once through its glue and never the list itself.
    assert_eq!(main.matches("func: \"drop.Tree\"").count(), 1, "{main}");
    assert!(!main.contains("func: \"sal_list_free\""), "{main}");
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
                out.push_str(&body.replace("base: \"v\"", &format!("base: \"{arg}\"")));
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
