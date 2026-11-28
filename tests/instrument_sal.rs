use std::path::{Path, PathBuf};
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}


fn instrument_opts() -> CompileOptions {
    CompileOptions {
        toolchain: Default::default(),
        output: None,
        release: false,
        instrument: true,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    }
}

fn link_lock() {
    // Each link gets its own `a.out.<pid>.<n>`, so the tests need no lock to run in parallel.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe { std::env::set_var("SAL_OUT_UNIQUE", "1") });
}

fn compile_instrumented(file: &Path) -> (String, PathBuf) {
    link_lock();
    let art = compile_file(file, &instrument_opts()).expect("compile instrumented");
    let bin = art.binary.expect("binary");
    (art.llvm, bin)
}

/// A String handed to a user fn that returns it keeps unknown provenance: the caller
/// does not free it, so instrumented shutdown reports LEAK. Also wires `--instrument-out`.
#[test]
fn instrument_sal_leak() {
    let file = PathBuf::from(format!("/tmp/sal-inst-leak-{}.sal", std::process::id()));
    std::fs::write(
        &file,
        "fn main() -> Int ! alloc\n    s = int_to_str(12345)\n    h = str_as_int(&s)\n    h - h\n",
    )
    .expect("write leak source");
    let (llvm, bin) = compile_instrumented(&file);
    assert!(
        llvm.contains("sal_instrument_malloc")
            && llvm.contains("sal_instrument_shutdown"),
        "expected instrument probes:\n{llvm}"
    );
    let out = Command::new(&bin)
        .env("SAL_INSTRUMENT_STRICT", "1")
        .output()
        .expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit on LEAK");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LEAK"),
        "stderr missing LEAK: {stderr}"
    );

    // Also exercise CLI `--instrument-out` via `sal run`.
    let out_path = PathBuf::from("/tmp/sal-instrument-out-wiring.json");
    let sal = env!("CARGO_BIN_EXE_sal");
    link_lock();
    let status = Command::new(sal)
        .arg("run")
        .arg(&file)
        .arg("--instrument")
        .arg("--instrument-out")
        .arg(&out_path)
        .env("SAL_INSTRUMENT_STRICT", "1")
        .current_dir(root())
        .output()
        .expect("sal run");
    assert_ne!(status.status.code(), Some(0));
    let saved = std::fs::read_to_string(&out_path).expect("read instrument-out");
    assert!(
        saved.contains("LEAK") || String::from_utf8_lossy(&status.stderr).contains("LEAK"),
        "instrument-out={saved} stderr={}",
        String::from_utf8_lossy(&status.stderr)
    );
}

/// `index(collection, i)` with `--instrument` must call `sal_instrument_check_index`
/// and exit non-zero with OOB (IR has no List lengths; LLVM uses a small bound).
#[test]
fn instrument_sal_oob() {
    let file = root().join("examples/instrument_oob.sal");
    let (llvm, bin) = compile_instrumented(&file);
    assert!(
        llvm.contains("sal_instrument_check_index"),
        "expected check_index probe:\n{llvm}"
    );
    let out = Command::new(&bin)
        .current_dir(root())
        .output()
        .expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit on OOB");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("OOB"),
        "stderr missing OOB: {stderr}"
    );
}

/// `load` of a .salt with f32 NaN bits must hit `sal_instrument_check_f32` and emit NAN.
#[test]
fn instrument_sal_nan() {
    let file = root().join("examples/instrument_nan.sal");
    let (llvm, bin) = compile_instrumented(&file);
    assert!(
        llvm.contains("sal_instrument_check_f32")
            && llvm.contains("sal_load_f32"),
        "expected load + check_f32:\n{llvm}"
    );
    let out = Command::new(&bin)
        .current_dir(root())
        .output()
        .expect("run");
    assert_ne!(out.status.code(), Some(0), "expected non-zero exit on NAN");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("NAN"),
        "stderr missing NAN: {stderr}"
    );
}

fn run_strict(src: &str, tag: &str) -> std::process::Output {
    let file = PathBuf::from(format!("/tmp/sal-strict-{}-{tag}.sal", std::process::id()));
    std::fs::write(&file, src).expect("write source");
    let (_llvm, bin) = compile_instrumented(&file);
    Command::new(&bin)
        .env("SAL_INSTRUMENT_STRICT", "1")
        .current_dir(root())
        .output()
        .expect("run")
}

/// SPEC «Memoria»: the constructor of a `frame` does not move its columns, so the column
/// local is still released at scope end (the frame owns nothing).
#[test]
fn instrument_strict_frame_column_is_freed() {
    let out = run_strict(
        "frame Rows\n    val: F32\n\nfn main() -> Int ! alloc\n    v = tensor[[1.0, 2.0, 3.0]]\n    r = Rows(v)\n    mask = v != 2.0\n    _ = where(r, mask)\n    0\n",
        "frame_column",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "frame column: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
}

/// `xg = to gpu x` owns the tensor `x` moved into it; a tensor a user fn returns is the
/// caller's, bound or discarded; the fn releases its own temporaries (`v * 2.0`).
#[test]
fn instrument_strict_tensor_moves_and_results_are_freed() {
    let out = run_strict(
        "fn scale(v: &Tensor[F32, 4] on gpu) -> Tensor[F32, 4] on gpu ! gpu\n    on gpu\n        v * 2.0 + v\n\nfn main() -> Int ! alloc, gpu\n    x = tensor[[1.0, 2.0, 3.0, 4.0]]\n    xg = to gpu x\n    _ = scale(&xg)\n    y = scale(&xg)\n    _ = scale(&y)\n    0\n",
        "tensor_moves",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "tensor moves: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
}

/// A tensor fn that goes through `emit_host_tensor_fn` (`relu`) returns mid-program: only the
/// end of `main` checks for leaks, and every buffer moved through `to` is released once.
#[test]
fn instrument_strict_tensor_fn_through_places_is_clean() {
    let src = std::fs::read_to_string(root().join("corpus/places.sal")).expect("read");
    let out = run_strict(&src, "places");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "places: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
}

/// `to cpu f(&g)`: the tensor `f` returns moves to the cpu (the runtime copies it by the size
/// its heap recorded and frees the gpu one), so neither block outlives `main`.
#[test]
fn instrument_strict_to_of_fn_result_moves_it() {
    let file = PathBuf::from(format!("/tmp/sal-strict-{}-to_fn_result.sal", std::process::id()));
    std::fs::write(
        &file,
        "fn scale(v: &Tensor[F32, 4] on p) -> Tensor[F32, 4] on p\n    on p\n        v * 2.0\n\nfn main() -> Int ! alloc, gpu\n    x = tensor[[1.0, 2.0, 3.0, 4.0]]\n    g = to gpu x\n    y = to cpu scale(&g)\n    z = to cpu scale(&g)\n    _ = to gpu z\n    0\n",
    )
    .expect("write source");
    let (llvm, bin) = compile_instrumented(&file);
    assert!(llvm.contains("call ptr @sal_place_move("), "`to` of a fn result must move it:\n{llvm}");
    let out = Command::new(&bin)
        .env("SAL_INSTRUMENT_STRICT", "1")
        .current_dir(root())
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "to of a fn result: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
}

/// SPEC «Liberación de locales»: the body of a `match` arm is a temporary scope, as in Rust: the
/// `String` it builds and only reads is released at the end of the arm (expression arm and tail of a
/// block arm), with the pattern binder still alive.
#[test]
fn instrument_strict_match_arm_temporaries_are_freed() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_match_arm_temporaries.sal")).expect("read");
    let out = run_strict(&src, "match_arm_temps");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(23), "match arm temporaries: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
}

/// SPEC «Memoria»: a tensor passed by value moves into the callee, which releases it at scope
/// end, or hands it back when it returns it (then the caller releases it once).
#[test]
fn instrument_strict_tensor_param_by_value_is_freed() {
    let out = run_strict(
        "fn total(v: Tensor[F32, 4] on cpu) -> Int ! alloc\n    on cpu\n        w = v * 2.0\n    0\n\nfn keep(v: Tensor[F32, 4] on cpu) -> Tensor[F32, 4] on cpu\n    v\n\nfn main() -> Int ! alloc\n    x = tensor[[1.0, 2.0, 3.0, 4.0]]\n    y = keep(x)\n    _ = keep(y)\n    a = tensor[[5.0, 6.0, 7.0, 8.0]]\n    total(a)\n",
        "tensor_param",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "tensor param: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
}

/// `to gpu load(..)` moves the loaded block to the gpu heap (the compiler does not know its
/// size; the runtime does), and an elementwise op (`relu`) keeps its result on the place of
/// its input: every bounds check of the gpu matmuls finds its block on the gpu.
#[test]
fn instrument_strict_transformer_gpu_keeps_places() {
    let src = std::fs::read_to_string(root().join("examples/transformer_gpu.sal")).expect("read");
    let out = run_strict(&src, "transformer_gpu");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stderr.contains("BAD_PLACE"), "bad place: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(stdout.contains("argmax=1 gpu=8"), "stdout={stdout} stderr={stderr}");
}

/// A body generic over `on p` with a `?` axis runs where its arguments are (the gpu) and reads
/// the row count from the buffer it is handed; its result leaves the fused region before the
/// region ends.
#[test]
fn instrument_strict_place_generic_fn_runs_where_its_args_are() {
    let path = root().join("tests/lang/tensors/tensor_place_generic_dynamic_rows.sal");
    let src = std::fs::read_to_string(path).expect("read");
    let out = run_strict(&src, "place_generic");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("BAD_PLACE"), "bad place: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(17), "place generic: {stderr}");
}

/// `corpus/load_salt.sal`: two `.salt` files loaded on the cpu, moved to the gpu and handed to a
/// fn generic over its place and its rows; the result comes back to the cpu.
#[test]
fn instrument_strict_corpus_load_salt_runs() {
    fn salt(path: &Path, dims: &[u64], payload: &[f32]) {
        let mut b = b"SALT".to_vec();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&[0u8, dims.len() as u8]);
        for d in dims {
            b.extend_from_slice(&d.to_le_bytes());
        }
        for v in payload {
            b.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, b).expect("write salt");
    }
    let tmp = tempfile::tempdir().expect("tmpdir");
    let eye: Vec<f32> = (0..16).map(|i| if i % 5 == 0 { 1.0 } else { 0.0 }).collect();
    salt(&tmp.path().join("w.salt"), &[4, 4], &eye);
    salt(&tmp.path().join("x.salt"), &[1, 4], &[1.0, -2.0, 3.0, 4.0]);
    let (_llvm, bin) = compile_instrumented(&root().join("corpus/load_salt.sal"));
    let out = Command::new(&bin)
        .env("SAL_INSTRUMENT_STRICT", "1")
        .current_dir(tmp.path())
        .output()
        .expect("run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "load_salt: {stderr}");
    assert!(!stderr.contains("\"code\""), "instrument event: {stderr}");
}

/// Locals the compiler frees at scope end leave no live block, even in strict mode.
#[test]
fn instrument_strict_freed_locals_are_clean() {
    let out = run_strict(
        "enum Shape\n    Dot\n    Rect(Int, Int)\n    Tag(String, Int)\n\nfn main() -> Int ! alloc\n    s = int_to_str(12345) + \"x\"\n    xs = list_new[String]()\n    _a = list_push(xs, int_to_str(7))\n    d = dict_new[String, String]()\n    _b = dict_put(d, \"k\", int_to_str(9))\n    e = Tag(int_to_str(3), 4)\n    n = match e\n        Tag(t, k) => str_len(t) + k\n        Dot => 0\n        Rect(w, h) => w\n    str_len(s) + list_len(xs) + n\n",
        "clean",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
}

/// Pages of an `@arena` call are freed as one block, so strict mode reports no LEAK.
#[test]
fn instrument_strict_arena_pages_are_clean() {
    let out = run_strict(
        "@arena\nfn work(req: &String) -> Int ! alloc\n    s = req.dup() + \"!\"\n    xs = list_new[Int]()\n    _a = list_push(xs, str_len(s))\n    str_len(s)\n\nfn main() -> Int ! alloc\n    work(&\"ab\")\n",
        "arena",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.code().is_some(), "arena crashed: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "arena double free: {stderr}");
}

/// A second `@arena` call can map its page at the address the first one unmapped: the probe
/// takes it as a new block, not a free of the old one (`sal_instrument_alloc`).
#[test]
fn instrument_strict_arena_page_reused_is_clean() {
    let out = run_strict(
        "@arena\nfn f(early: Int) -> Int\n    a = 1\n    r = &mut a\n    *r = 5\n    if early == 1\n        return *r\n    a\n\nfn main() -> Int\n    f(1) + f(0) + f(1)\n",
        "arena_reuse",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(15), "arena reuse: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "arena double free: {stderr}");
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
}

/// A string whose handle escapes (`str_as_int(&s)`) is never freed by the pass, and strict
/// mode reports it. (A string handed to a function that returns it is now freed by the caller
/// that receives it: `t = keep(s)` owns the block, as in Rust.)
#[test]
fn instrument_strict_reports_unfreed_string() {
    let out = run_strict(
        "fn main() -> Int ! alloc\n    s = int_to_str(12345)\n    h = str_as_int(&s)\n    h - h\n",
        "leak",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("LEAK"), "expected a leak report: {stderr}");
}

/// Lists of enums own their boxes (`list_free` releases them, `list_set` replaces
/// one). `list_get` borrows the element, so the local is not released again.
#[test]
fn instrument_strict_enum_lists_are_clean() {
    for (tag, file) in [
        ("enum_owned", "drops/drop_list_enum_owned.sal"),
        ("enum_nested", "drops/drop_list_enum_nested.sal"),
    ] {
        let src = std::fs::read_to_string(root().join("tests/lang").join(file)).expect("read");
        let out = run_strict(&src, tag);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("LEAK"), "{file}: unexpected leak: {stderr}");
    }
}

/// A value popped from an owned list is a fresh box the local releases. A `list_get` read
/// straight into a `match` borrows the element: the list releases it, and strict mode is clean.
#[test]
fn instrument_strict_list_pop_and_match_temps_are_clean() {
    for (tag, file) in [
        ("pop_enum", "drops/drop_list_pop_enum.sal"),
        ("match_temp", "drops/drop_match_list_get_temp.sal"),
    ] {
        let src = std::fs::read_to_string(root().join("tests/lang").join(file)).expect("read");
        let out = run_strict(&src, tag);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("LEAK"), "{file}: unexpected leak: {stderr}");
    }
}

/// An owned struct list frees its boxes. `list_get` borrows, so a read passed to a function
/// that only looks at a field is not copied and not freed again.
#[test]
fn instrument_strict_struct_list_box_pass_fn_is_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_list_struct_box_pass_fn.sal"))
        .expect("read");
    let out = run_strict(&src, "struct_box_fn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("struct_copy"), "struct_copy leak: {stderr}");
    assert_eq!(out.status.code(), Some(8), "stderr: {stderr}");
}

#[test]
fn instrument_strict_struct_list_boxes_are_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_list_struct_fields_copy.sal"))
        .expect("read");
    let out = run_strict(&src, "struct_list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(8), "stderr: {stderr}");
}

/// A struct popped from an owned struct list is a box the local releases: strict mode is clean.
#[test]
fn instrument_strict_list_pop_struct_is_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_list_pop_struct.sal")).expect("read");
    let out = run_strict(&src, "pop_struct");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
}

/// A discarded `pop` is a fresh value the statement releases. A discarded `get` only borrows.
#[test]
fn instrument_strict_discarded_fresh_values_are_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_discard_fresh.sal")).expect("read");
    let out = run_strict(&src, "discard_fresh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(4), "stderr: {stderr}");
}

/// Pushing a borrowed element clones it. Both lists free their own box: no leak, no double free.
#[test]
fn instrument_strict_list_get_pushed_copies_once() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    ys = list_new[P]()\n    list_push(ys, o.clone())\n    list_len(xs) + list_len(ys)\n";
    let out = run_strict(src, "struct_push");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
}

/// Returning a borrowed element by value needs `clone`. Moving that copy into another list
/// frees it once; the source list keeps its element.
#[test]
fn instrument_strict_list_get_returned_is_freed_once() {
    // `list_get` lends the element (SPEC «Memoria»): returning it by value is `E_BORROW`, so the
    // callee clones out of the borrow and the list keeps its element.
    let src = "struct P\n    name: String\n    n: Int\n\nfn give(xs: &List[P]) -> P ! alloc\n    list_get(xs, 0).clone()\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    p = give(&xs)\n    ys = list_new[P]()\n    list_push(ys, p)\n    list_get(ys, 0).n\n";
    let out = run_strict(src, "struct_ret");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// Reading a field of a borrowed struct does not copy it.
#[test]
fn instrument_strict_list_get_field_read_is_clean() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn read_n(p: &P) -> Int ! alloc\n    p.n\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    read_n(o)\n";
    let out = run_strict(src, "struct_read");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// Rebuilding a borrowed struct clones its owned fields. Moving the result into a list frees
/// that copy; the source list still owns the original element.
#[test]
fn instrument_strict_list_get_rebuilt_is_clean() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: &P) -> P ! alloc\n    P(p.name.clone(), p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    ys = list_new[P]()\n    list_push(ys, q)\n    list_get(ys, 0).n\n";
    let out = run_strict(src, "struct_rebuild");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// Pushing the rebuilt value moves that owned copy. Both lists free their own strings.
#[test]
fn instrument_strict_list_get_rebuilt_then_pushed_is_clean() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn rebuild(p: &P) -> P ! alloc\n    P(p.name.clone(), p.n)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    q = rebuild(o)\n    ys = list_new[P]()\n    list_push(ys, q)\n    list_len(xs) + list_len(ys)\n";
    let out = run_strict(src, "struct_rebuild_push");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
}

/// `o.n = v` needs `&mut` of the list element (`xs.get_mut(i)`).
#[test]
fn instrument_strict_field_write_updates_the_list_element() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = List.new[P]()\n    xs.push(P(\"a\", 1))\n    o = xs.get_mut(0)\n    o.n = 9\n    xs.get(0).n\n";
    let out = run_strict(src, "struct_field_write");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(9), "stderr: {stderr}");
}

/// Overwriting an owned field drops the previous value and stores in place (Rust).
#[test]
fn instrument_strict_field_overwrite_drops_old() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn main() -> Int ! alloc\n    xs = List.new[P]()\n    xs.push(P(strdup(\"a\"), 1))\n    o = xs.get_mut(0)\n    o.name = strdup(\"bb\")\n    str_len(xs.get(0).name)\n";
    let out = run_strict(src, "struct_field_overwrite");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
}

/// `c.field = v` through `&mut` writes back; the local's box is freed by `drop.OC`.
#[test]
fn instrument_strict_mut_param_field_assign_is_clean() {
    let src = "struct OC\n    moved: List[String]\n    n: Int\n\nfn reset(c: &mut OC) -> Int ! alloc\n    c.moved = List.new[String]()\n    0\n\nfn main() -> Int ! alloc\n    c = OC(List.new[String](), 1)\n    list_push(&mut c.moved, \"a\")\n    reset(&mut c)\n    list_len(&c.moved)\n";
    let out = run_strict(src, "mut_field_assign");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
}

/// A multi-field local frees its `sal_struct_new` block with `drop.<Struct>` (Rust Drop).
#[test]
fn instrument_strict_struct_local_box_is_clean() {
    let src = "struct OC\n    moved: List[String]\n    n: Int\n\nfn main() -> Int ! alloc\n    c = OC(List.new[String](), 1)\n    list_len(&c.moved)\n";
    let out = run_strict(src, "struct_local_box");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
}

/// A callee that stores the struct takes it: the caller clones out of the borrow.
#[test]
fn instrument_strict_list_get_taken_is_freed_once() {
    let src = "struct P\n    name: String\n    n: Int\n\nfn keep(p: P) -> Int ! alloc\n    ys = list_new[P]()\n    list_push(ys, p)\n    list_len(ys)\n\nfn main() -> Int ! alloc\n    xs = list_new[P]()\n    list_push(xs, P(\"a\", 1))\n    o = list_get(xs, 0)\n    keep(o.clone())\n";
    let out = run_strict(src, "struct_take");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert!(!stderr.contains("DOUBLE_FREE"), "double free: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}

/// A user fn whose every result is a fresh String hands its caller an owned value: the caller
/// releases it, however it is used.
#[test]
fn instrument_strict_user_fn_string_is_clean() {
    let src = std::fs::read_to_string(root().join("tests/lang/drops/drop_user_fn_string_owned.sal")).expect("read");
    let out = run_strict(&src, "user_fn_string");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("LEAK"), "unexpected leak: {stderr}");
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
}
