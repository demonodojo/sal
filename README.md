# sal

[Español](LEEME.md) | English

**sal** is a compiled systems language targeting native code, with indentation-based syntax, garbage-collector-free memory management (Rust's ownership model), **explicit effects in signatures**, and **CPU/GPU/TPU as part of the type**.

The compiler emits LLVM IR and links it with Clang. Two implementations coexist: the Rust **bootstrap** (`src/`, the `sal` binary) and the sal **self-hosted** compiler (`selfhost/`), used to validate that the language compiles itself. There is also a style linter written in sal (`standard/`).

```sal
fn main() -> Int
    42
```

```bash
sal run examples/hello.sal   # exits with 42
```

## Table of contents

- [Complete example](#complete-example)
- [Language features](#language-features)
- [Installation](#installation)
- [Compiler usage](#compiler-usage)
- [Project structure](#project-structure)
- [Status and documentation](#status-and-documentation)

## Complete example

```sal
import "std/prelude.sal"

struct Vec2
    x: Float
    y: Float

method Vec2.norm2(&self) -> Float
    self.x * self.x + self.y * self.y

fn apply(xs: &List[Int], f: fn(Int) -> Int) -> List[Int] ! alloc
    out = List.new[Int]()
    i = 0
    while i < xs.len()
        out.push(f(xs.get(i)))
        i = i + 1
    out

fn main() -> Int ! io, alloc
    v = Vec2(3.0, 4.0)

    xs = List.new[Int]()
    xs.push(1)
    xs.push(2)
    xs.push(3)

    doubled = apply(&xs) x => x * 2
    total = 0
    i = 0
    while i < doubled.len()
        total = total + doubled.get(i)
        i = i + 1

    label = total.to_str()
    print(f"sal: #{label}")     # sal: 12

    if v.norm2() == 25.0
        total - 12
    else
        1
```

## Language features

### Indentation-based syntax

Blocks are delimited with spaces; there are no braces or semicolons. Comments start with `#`, and a tab is a syntax error (`E_PARSE`). The last expression of a block is its value; `return` ends the function at that point.

```sal
fn classify(n: Int) -> String
    if n < 0
        "negative"
    elsif n == 0
        "zero"
    else
        "positive"
```

### Functions with checked effects

The signature declares the effects a function may perform: `io`, `alloc`, `panic`, `gpu`, and `tpu`. Calling an allocating function from one that does not declare `alloc` is a compile error (`E_EFFECT`), and the compiler propagates the check through the entire call chain.

```sal
fn read_name() -> String ! io, alloc
    read_file(&d"name.txt")

fn length() -> Int ! io, alloc
    read_name().len()
```

### Ownership and borrowing, like Rust

Every owned value has a single owner and **moves by default**; it is borrowed with `&T` / `&mut T` (with lifetimes and elision as in Rust) and deep-copied only with `x.clone()`. There is no garbage collector, no implicit copies, and no inferred parameter modes. Drops are inserted at scope exit and on reassignment.

```sal
fn consume(s: String) -> Int
    s.len()

fn main() -> Int ! alloc
    s = d"hello"       # d"..." is strdup: an owned String
    n = consume(s)     # s moves
    # s.len()          # error E_MOVED
    n
```

```sal
fn add_one(xs: &mut List[Int]) -> Unit
    xs.push(1)

fn main() -> Int ! alloc
    xs = List.new[Int]()
    add_one(&mut xs)
    xs.len()
```

`Copy` values (`Int`, `Float`, `Bool`, `Unit`, and structs whose fields are all `Copy`) are copied rather than moved. Borrow rules are checked statically: two `&` borrows may coexist, a `&mut` does not overlap any other borrow, and mutating a borrowed place or returning a reference to a local is `E_BORROW`.

`@arena` is the only extension to the model: the call's heap comes from a region that is discarded all at once on return, and the return value must be stack-allocated.

```sal
@arena
fn sum_squares(n: Int) -> Int ! alloc
    xs = List.new[Int]()
    i = 0
    while i < n
        xs.push(i * i)
        i = i + 1
    total = 0
    i = 0
    while i < xs.len()
        total = total + xs.get(i)
        i = i + 1
    total
```

### Structs, enums, methods and constructors

A struct is constructed with its name and fields in order. A single-field struct is a transparent *newtype* with no representation overhead. An enum stores its payload in a box and is consumed with `match`.

```sal
struct Point
    x: Int
    y: Int

method Point.sum(&self) -> Int
    self.x + self.y

static Point.origin() -> Point
    Point(0, 0)

init Point(x: Int, y: Int)
    Point(x, y)

enum Shape
    Circle(Float)
    Square(Float)

fn area(s: &Shape) -> Float
    match s
        Circle(r) => 3.14159 * r * r
        Square(a) => a * a
```

Basic types also have methods (`xs.push(v)`, `s.len()`, `List.new[Int]()`, `Dict.new[String, Int]()`). They are the same builtins with the receiver as the first argument and the same ownership rules.

### Generics with monomorphization

Generic functions are checked once with erased types and also specialized per call site: each concrete combination of types produces its own copy. Generic structs are substituted the same way, and generic enums share a representation.

```sal
fn id[T](x: T) -> T
    x

struct Box[T]
    value: T

fn main() -> Int
    id(7)
```

### Protocols and dynamic dispatch

A `protocol` declares required and provided methods. A `struct` or `enum` claims the protocol in its header (`impl P`) or with an external `impl Type P` (orphan rule), and `any[P]` gives dynamic dispatch through a vtable.

```sal
protocol Show
    method show(self) -> String ! alloc

struct Point impl Show
    x: Int

method Point.show(self) -> String ! alloc
    self.x.to_str()

fn log(x: &any[Show]) -> String ! alloc
    x.show()
```

### Patterns, regex and lambdas

`match` picks the first arm that fits. It supports enum variants with payloads, `Int` and `String` literals, `_`, and regex patterns `r"..."`.

```sal
fn kind(line: String) -> Int
    match line
        r"^GET " => 1
        r"^POST " => 2
        _ => 0
```

Lambdas are passed to `fn(...)` parameters and support the trailing form without parentheses. The indented block after `=>` also works as a body.

```sal
fn fold(xs: &List[Int], initial: Int, f: fn(Int, Int) -> Int) -> Int
    acc = initial
    i = 0
    while i < xs.len()
        acc = f(acc, xs.get(i))
        i = i + 1
    acc

total = fold(&xs, 0) (a, x) => a + x
```

### Strings and collections

`String` is immutable; `+` concatenates (with the `alloc` effect) and `x = x + y` reuses the accumulator's block. Strings have four special forms:

| Literal | Meaning |
|---------|---------|
| `"text"` | Static literal in rodata (unowned, shared) |
| `d"text"` | `strdup`: an owned `String` |
| `f"hello #{name}"` | Interpolation of `String` variables |
| `r"^x+"` | POSIX ERE regex, only as a `match` pattern |

`List[T]`, `Dict[K, V]`, and `Bytes` are owned containers. `Dict.get` returns `Option[V]`, `List.get` borrows the element, and `pop` moves it to the caller. `Option` and `Result` live in the prelude, which is imported explicitly (`import "std/prelude.sal"`).

### Tensors and devices in the type

Place is part of the type: `Tensor[F32, 4, 4] on gpu`. `to gpu x` is an explicit transfer, and mixing places without `to` is `E_PLACE`. Tensor operations (`matmul`, `softmax`, `map`, `reduce`, `relu`, `attention`, `qmatmul`, …) run inside an `on` block:

```sal
fn scale(x: &Tensor[F32, 2, 2] on p) -> Tensor[F32, 2, 2] on p
    on p
        relu(x * 2.0)

fn main() -> Int ! gpu, alloc
    host = tensor[[1.0, 2.0], [3.0, 4.0]]
    dev = to gpu host
    _back = to cpu scale(&dev)
    0
```

Arithmetic and comparison operators are **columnar** when one operand is a tensor: same element type, same place, and shape broadcasting from the right. Inside `on`, operator chains in the same place are fused into a single region. A `frame` groups numeric or `String` columns around a row axis and is filtered with `where(table, mask)`.

### Modules and packages

Each file is a module. `import path` flattens the module's items (and its imports); `import path as alias` requires qualifying with `alias.name`.

```sal
import "std/io/fs.sal"
import "my/project/math.sal" as mat
```

Dependencies are declared in `Sal.toml` and resolved with `sal fetch` / `sal install` (which link each package into `sal_modules/`). `path` dependencies, `git` dependencies with a semver version, and GitHub releases are supported (`{ github = "owner/repo", version = "0.1" }`). The `Sal.lock` lockfile pins the revision.

### Attributes

| Attribute | Applies to | Effect |
|-----------|------------|--------|
| `@frozen` | `let` | The binding is not reassigned |
| `@stack` | `let` | The value is stack-allocated and constructing it does not allocate |
| `@no_heap` | `fn` | The function does not touch the heap |
| `@arena` | `fn`, `method`, `static`, `init` | Region heap, stack return |
| `@layout(c)` | `struct` | C representation (`Int`, `Float`, `Bool`) |
| `@deprecated("text")` | item | `W_DEPRECATED` warning on each use |

### Diagnostics with a closed set of codes

Errors carry a stable code, and `--error-format json` emits them for tools:

| Code | Meaning |
|------|---------|
| `E_PARSE` | Syntax (including constructs reserved for future phases) |
| `E_TYPE` | Types, arity, names or imports |
| `E_MOVED` | Use of a value after it was moved |
| `E_BORROW` | Overlapping borrow, reference outliving its referent, lifetime with no source |
| `E_EFFECT` | The function performs an effect it does not declare |
| `E_PLACE` | Mixing places (`cpu`/`gpu`/`tpu`) without `to` |
| `E_SHAPE` | Incompatible tensor shapes |
| `E_TENSOR_ELEM` | Unsupported tensor element type |
| `E_DEVICE` | Invalid operation on that device |
| `E_DEVICE_MISSING` | Device unavailable |
| `E_OOM` | Out of memory |
| `E_TARGET` | The construct does not exist for the target (`--target`) |

`E_INTERNAL` is reserved for compiler failures. `W_DEPRECATED` is a warning and does not interrupt compilation.

### Instrumentation and memory checking

With `--instrument`, the binary includes probes for `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, and `INF`. With `--sanitize`, the program is compiled and linked with ASan, LSan, and UBSan to detect leaks, double frees, and use-after-free.

## Installation

Requirements: Rust (2021 edition) and Clang.

```bash
cargo build --release
```

The following end up in `target/release/`:

- `sal` — compiler (bootstrap)
- `standard` — style linter written in sal
- `som2salt` — tensor converter

## Compiler usage

```bash
sal check examples/hello.sal      # check only
sal build examples/hello.sal      # compile and link (target/sal-out/a.out)
sal run examples/hello.sal        # compile and run
sal fmt examples/hello.sal        # format
sal emit ast examples/hello.sal   # AST as JSON
sal emit ir examples/forward.sal  # IR
sal test                          # tests/sal/test_*.sal
sal selfhost                      # self-hosted stage1 and stage2
sal parity examples/forward.sal   # compare bootstrap IR with self-hosted IR
```

Global and per-command flags:

| Flag | Effect |
|------|--------|
| `-o, --output FILE` | Path of the generated binary |
| `--release` | Optimization and full LTO (modules and runtime as bitcode) |
| `--device cpu\|gpu\|tpu` | Default device for compilation |
| `--target linux\|soso` | Compilation target |
| `--instrument`, `--instrument-out FILE` | Instrumentation probes |
| `--sanitize` | ASan + LSan + UBSan (not available for `soso`) |
| `--error-format human\|json` | Diagnostic format |

Dependencies:

```bash
sal fetch            # download dependencies and write Sal.lock
sal install          # link dependencies into sal_modules/
sal add name --git URL --version 0.1
```

Style linter:

```bash
standard check examples/hello.sal
standard fix examples/hello.sal   # rewrites the file in place
```

## Project structure

| Path | Contents |
|------|----------|
| `src/` | Rust bootstrap compiler (lexer, parser, inference, ownership, effects, drops, IR, fusion, LLVM, packages) |
| `runtime/` | C runtime (kernels, instrumentation, `soso` target) |
| `selfhost/` | Compiler written in sal (mirror of the bootstrap passes) |
| `std/` | Prelude and standard library in sal (`std/io`, `http`, `json`, `text`) |
| `standard/` | Style linter, written in sal |
| `corpus/`, `examples/` | Parity programs and examples |
| `tests/` | Rust tests, conformance tests (`tests/lang/`), and `std` tests (`tests/sal/`) |
| `editors/vscode/` | VS Code extension |

## Status and documentation

The language specification — grammar, types, memory, modules, tensors — is **[SPEC.md](SPEC.md)** and takes precedence over the implementation. Per-pass subplans live in [docs/subplans/](docs/subplans/).

The bootstrap compiler currently runs the legacy ownership and drops passes while subplan 13 is being closed; the self-hosted compiler validates the language but does not replace the bootstrap. A construct marked `# xfail` in `tests/lang/` is a known gap, not a supported feature.

Tests:

```bash
cargo test --locked                # full release gate
cargo test --test lang             # language conformance
cargo test --test selfhost_ir -- --test-threads=1
cargo test --test standard -- --test-threads=1
```
