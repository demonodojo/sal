//! `struct T impl Reflect` (SPEC «Reflexión»): the methods of `Reflect` that `T` does not
//! define, written as sal text and appended to the module before it is parsed. The selfhost
//! mirror is `selfhost/front/reflect.sal`, with the same text, so both compilers lower the same
//! tree.

use crate::diag::{Diagnostic, ErrorCode};
use crate::span::Span;

/// A field type that `Reflect` converts to and from `Value`.
#[derive(Clone, Debug, PartialEq)]
enum Ty {
    Int,
    Float,
    Bool,
    Str,
    /// A struct that also claims `Reflect` (or an imported one, trusted to): `VDict`.
    Struct(String),
    List(Box<Ty>),
    Dict(Box<Ty>),
}

struct RStruct {
    name: String,
    fields: Vec<(String, Ty)>,
}

/// The module's top-level `struct` / `enum` headers: name, line, whether it claims `Reflect`,
/// whether it has type parameters.
struct Header {
    name: String,
    line: usize,
    reflect: bool,
    generic: bool,
    is_struct: bool,
}

pub fn with_reflect(src: &str) -> Result<String, Diagnostic> {
    if !src.contains("Reflect") {
        return Ok(src.to_string());
    }
    let lines: Vec<&str> = src.split('\n').collect();
    let offsets: Vec<usize> = {
        let mut o = Vec::with_capacity(lines.len());
        let mut at = 0;
        for l in &lines {
            o.push(at);
            at += l.len() + 1;
        }
        o
    };
    let headers: Vec<Header> = lines.iter().enumerate().filter_map(|(i, l)| header(l, i)).collect();
    if !headers.iter().any(|h| h.reflect) {
        return Ok(src.to_string());
    }
    let mut structs = Vec::new();
    for h in headers.iter().filter(|h| h.reflect) {
        let span_at = |line: usize, col: usize, len: usize| Span {
            start: (offsets[line] + col) as u32,
            end: (offsets[line] + col + len) as u32,
            line: line as u32 + 1,
            col: col as u32 + 1,
        };
        if h.generic || !h.is_struct {
            let what = if h.is_struct { "a generic struct" } else { "an enum" };
            return Err(Diagnostic::new(
                ErrorCode::EType,
                format!("impl Reflect on {what} ({}) is not supported", h.name),
                span_at(h.line, 0, lines[h.line].len()),
            ));
        }
        let mut fields = Vec::new();
        for (i, l) in lines.iter().enumerate().skip(h.line + 1) {
            let t = l.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if !l.starts_with(' ') {
                break;
            }
            let body = match t.find('#') {
                Some(c) => t[..c].trim(),
                None => t,
            };
            let Some((fname, fty)) = body.split_once(':') else { break };
            let fname = fname.trim().to_string();
            let fty = fty.trim();
            let col = l.len() - l.trim_start().len() + body.find(':').map_or(0, |c| c + 1);
            match field_ty(fty, &headers) {
                Some(ty) => fields.push((fname, ty)),
                None => {
                    return Err(Diagnostic::new(
                        ErrorCode::EType,
                        format!("impl Reflect: field {fname} of {} has type {fty}, which Reflect does not convert", h.name),
                        span_at(i, col, fty.len() + 1),
                    ))
                }
            }
        }
        structs.push(RStruct { name: h.name.clone(), fields });
    }
    let mut out = src.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    for s in &structs {
        out.push_str(&generate(s, src));
    }
    Ok(out)
}

fn header(line: &str, i: usize) -> Option<Header> {
    let (is_struct, rest) = if let Some(r) = line.strip_prefix("struct ") {
        (true, r)
    } else if let Some(r) = line.strip_prefix("enum ") {
        (false, r)
    } else {
        return None;
    };
    let rest = match rest.find('#') {
        Some(c) => &rest[..c],
        None => rest,
    };
    let (head, claims) = match rest.find(" impl ") {
        Some(at) => (&rest[..at], &rest[at + 6..]),
        None => (rest, ""),
    };
    let head = head.trim();
    let name: String = head.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if name.is_empty() {
        return None;
    }
    let generic = head[name.len()..].trim_start().starts_with('[');
    let reflect = claims.split(',').any(|c| c.trim() == "Reflect");
    Some(Header { name, line: i, reflect, generic, is_struct })
}

/// `Int`, `List[String]`, `Dict[String, Punto]`… A container holds a scalar or a struct.
fn field_ty(text: &str, headers: &[Header]) -> Option<Ty> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    parse_ty(&t, headers, true)
}

fn parse_ty(t: &str, headers: &[Header], outer: bool) -> Option<Ty> {
    match t {
        "Int" => return Some(Ty::Int),
        "Float" => return Some(Ty::Float),
        "Bool" => return Some(Ty::Bool),
        "String" => return Some(Ty::Str),
        _ => {}
    }
    if let Some(inner) = t.strip_prefix("List[").and_then(|r| r.strip_suffix(']')) {
        if !outer {
            return None;
        }
        return parse_ty(inner, headers, false).map(|e| Ty::List(Box::new(e)));
    }
    if let Some(inner) = t.strip_prefix("Dict[String,").and_then(|r| r.strip_suffix(']')) {
        if !outer {
            return None;
        }
        return parse_ty(inner, headers, false).map(|e| Ty::Dict(Box::new(e)));
    }
    if !t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) || !t.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let builtin = matches!(
        t,
        "Bytes" | "Unit" | "Tensor" | "Thread" | "Semaphore" | "Mutex" | "Channel" | "Value" | "Field" | "List" | "Dict"
    );
    if builtin {
        return None;
    }
    match headers.iter().find(|h| h.name == t) {
        // A struct of this module has to claim `Reflect` itself; an enum is not converted.
        Some(h) if h.is_struct && h.reflect && !h.generic => Some(Ty::Struct(t.to_string())),
        Some(_) => None,
        // A type of another module: its `to_dict` / `from_dict` are checked where they are called.
        None => Some(Ty::Struct(t.to_string())),
    }
}

fn ty_text(t: &Ty) -> String {
    match t {
        Ty::Int => "Int".into(),
        Ty::Float => "Float".into(),
        Ty::Bool => "Bool".into(),
        Ty::Str => "String".into(),
        Ty::Struct(n) => n.clone(),
        Ty::List(e) => format!("List[{}]", ty_text(e)),
        Ty::Dict(e) => format!("Dict[String, {}]", ty_text(e)),
    }
}

/// The `Value` of a scalar or struct at `place` (a field read or a container element).
fn value_of(t: &Ty, place: &str) -> String {
    match t {
        Ty::Int => format!("VInt({place})"),
        Ty::Float => format!("VFloat({place})"),
        Ty::Bool => format!("VBool({place})"),
        Ty::Str => format!("VStr({place}.clone())"),
        Ty::Struct(_) => format!("VDict({place}.to_dict())"),
        Ty::List(_) | Ty::Dict(_) => String::new(),
    }
}

/// The `Result[T, String]` of converting the `&Value` `arg` (scalars and structs).
fn conv(t: &Ty, arg: &str, label: &str, item_fn: &str) -> String {
    match t {
        Ty::Int => format!("value_int({arg}, &\"{label}\")"),
        Ty::Float => format!("value_float({arg}, &\"{label}\")"),
        Ty::Bool => format!("value_bool({arg}, &\"{label}\")"),
        Ty::Str => format!("value_str({arg}, &\"{label}\")"),
        _ => format!("{item_fn}({arg})"),
    }
}

/// An owned copy of `x`, a `&T` bound by `match &r`.
fn copy_of(t: &Ty, x: &str) -> String {
    match t {
        Ty::Int | Ty::Float | Ty::Bool => format!("*{x}"),
        _ => format!("{x}.clone()"),
    }
}

fn has_own(src: &str, kw: &str, s: &str, m: &str) -> bool {
    src.contains(&format!("{kw} {s}.{m}("))
}

fn generate(s: &RStruct, src: &str) -> String {
    let n = &s.name;
    let mut o = String::new();
    let struct_fields: Vec<&(String, Ty)> = s.fields.iter().filter(|(_, t)| matches!(t, Ty::Struct(_))).collect();
    if !has_own(src, "method", n, "type_name") {
        o.push_str(&format!("\nmethod {n}.type_name(&self) -> String ! alloc\n    d\"{n}\"\n"));
    }
    if !has_own(src, "method", n, "fields") {
        o.push_str(&format!("\nmethod {n}.fields(&self) -> List[Field] ! alloc\n    out = List.new[Field]()\n"));
        for (f, t) in &s.fields {
            o.push_str(&format!("    out.push(Field(d\"{f}\", d\"{}\"))\n", ty_text(t)));
        }
        o.push_str("    out\n");
    }
    // `get_field` / `to_dict`: the `Value` of each field.
    let field_value = |f: &str, t: &Ty| match t {
        Ty::List(_) | Ty::Dict(_) => format!("reflect_{n}_{f}_value(&self.{f})"),
        _ => value_of(t, &format!("self.{f}")),
    };
    // `name.slice` of a dotted name: `head` is the field, `rest` goes to its own method.
    let dotted = |o: &mut String, call: &str, ok: &str, miss: &str| {
        o.push_str("    dot = name.index_of(\".\")\n    if dot >= 0\n");
        o.push_str("        head = name.slice(0, dot)\n        rest = name.slice(dot + 1, name.len())\n");
        for (k, (f, _)) in struct_fields.iter().enumerate() {
            let kw = if k == 0 { "if" } else { "elsif" };
            o.push_str(&format!("        {kw} head.eq(\"{f}\") == 1\n"));
            o.push_str(&format!("            r = self.{f}.{call}\n            match &r\n"));
            o.push_str(ok);
        }
        o.push_str("        else\n");
        o.push_str(miss);
    };
    if !has_own(src, "method", n, "get_field") {
        o.push_str(&format!("\nmethod {n}.get_field(&self, name: &String) -> Option[Value] ! alloc\n"));
        let mut kw = "if";
        if !struct_fields.is_empty() {
            dotted(
                &mut o,
                "get_field(&rest)",
                "                Some(x) =>\n                    c = x.clone()\n                    Some(c)\n                None => None\n",
                "            None\n",
            );
            kw = "elsif";
        }
        for (f, t) in &s.fields {
            o.push_str(&format!("    {kw} name.eq(\"{f}\") == 1\n        v = {}\n        Some(v)\n", field_value(f, t)));
            kw = "elsif";
        }
        if kw == "if" {
            o.push_str("    None\n");
        } else {
            o.push_str("    else\n        None\n");
        }
    }
    if !has_own(src, "method", n, "set_field") {
        o.push_str(&format!(
            "\nmethod {n}.set_field(&mut self, name: &String, v: Value) -> Result[Unit, String] ! alloc\n"
        ));
        let mut kw = "if";
        if !struct_fields.is_empty() {
            dotted(
                &mut o,
                "set_field(&rest, v.clone())",
                "                Ok(_) => Ok(())\n                Err(e) =>\n                    m = e.clone()\n                    Err(m)\n",
                &format!("            e = d\"no field \" + head + \" in {n}\"\n            Err(e)\n"),
            );
            kw = "elsif";
        }
        for (f, t) in &s.fields {
            let call = conv(t, "&v", f, &format!("reflect_{n}_{f}_from"));
            o.push_str(&format!("    {kw} name.eq(\"{f}\") == 1\n        r = {call}\n        match &r\n"));
            o.push_str(&format!(
                "            Ok(x) =>\n                self.{f} = {}\n                Ok(())\n",
                copy_of(t, "x")
            ));
            o.push_str("            Err(e) =>\n                m = e.clone()\n                Err(m)\n");
            kw = "elsif";
        }
        let miss = format!("e = d\"no field \" + name + \" in {n}\"\n");
        if kw == "if" {
            o.push_str(&format!("    {miss}    Err(e)\n"));
        } else {
            o.push_str(&format!("    else\n        {miss}        Err(e)\n"));
        }
    }
    if !has_own(src, "method", n, "to_dict") {
        o.push_str(&format!(
            "\nmethod {n}.to_dict(&self) -> Dict[String, Value] ! alloc\n    out = Dict.new[String, Value]()\n"
        ));
        for (f, t) in &s.fields {
            o.push_str(&format!("    out.put(d\"{f}\", {})\n", field_value(f, t)));
        }
        o.push_str("    out\n");
    }
    if !has_own(src, "static", n, "from_dict") {
        o.push_str(&format!(
            "\nstatic {n}.from_dict(src: &Dict[String, Value]) -> Result[{n}, String] ! alloc\n"
        ));
        from_dict_body(&mut o, s, 0, 1);
    }
    for (f, t) in &s.fields {
        helpers(&mut o, n, f, t);
    }
    o
}

/// Field `k` of `from_dict`, nested one `match` per step so every `Result` is matched by
/// reference and released whole: the value under `f`, then its conversion.
fn from_dict_body(o: &mut String, s: &RStruct, k: usize, depth: usize) {
    let pad = "    ".repeat(depth);
    if k == s.fields.len() {
        let args: Vec<String> = s.fields.iter().enumerate().map(|(i, (_, t))| copy_of(t, &format!("x{i}"))).collect();
        o.push_str(&format!("{pad}o = {}({})\n{pad}Ok(o)\n", s.name, args.join(", ")));
        return;
    }
    let (f, t) = &s.fields[k];
    let err = |o: &mut String, pad: &str| {
        o.push_str(&format!("{pad}    Err(e{k}) =>\n{pad}        m{k} = e{k}.clone()\n{pad}        Err(m{k})\n"));
    };
    o.push_str(&format!("{pad}r{k} = value_field(src, &\"{f}\")\n{pad}match &r{k}\n{pad}    Ok(v{k}) =>\n"));
    let pad2 = "    ".repeat(depth + 2);
    let call = conv(t, &format!("&v{k}"), f, &format!("reflect_{}_{f}_from", s.name));
    o.push_str(&format!("{pad2}q{k} = {call}\n{pad2}match &q{k}\n{pad2}    Ok(x{k}) =>\n"));
    from_dict_body(o, s, k + 1, depth + 4);
    err(o, &pad2);
    err(o, &pad);
}

/// The fns a container or struct field needs: its `Value` (`_value`) and its conversion back
/// (`_from`, with `_check` / `_build` over the elements and `_item` for a struct element).
fn helpers(o: &mut String, n: &str, f: &str, t: &Ty) {
    let mismatch = |o: &mut String, want: &str| {
        o.push_str(&format!("        _ =>\n            e = value_mismatch(&\"{f}\", &\"{want}\", v)\n            Err(e)\n"));
    };
    let struct_from = |o: &mut String, name: &str, s: &str| {
        o.push_str(&format!("\nfn {name}(v: &Value) -> Result[{s}, String] ! alloc\n    match v\n"));
        o.push_str(&format!("        VDict(dd) =>\n            r = {s}.from_dict(&dd)\n            match &r\n"));
        o.push_str("                Ok(x) =>\n                    c = x.clone()\n                    Ok(c)\n");
        o.push_str("                Err(e) =>\n                    m = e.clone()\n                    Err(m)\n");
        mismatch(o, s);
    };
    let item = format!("reflect_{n}_{f}_item");
    match t {
        Ty::Struct(s) => struct_from(o, &format!("reflect_{n}_{f}_from"), s),
        Ty::List(e) | Ty::Dict(e) => {
            let is_list = matches!(t, Ty::List(_));
            let (vk, coll) = if is_list { ("VList", "items") } else { ("VDict", "entries") };
            let et = ty_text(e);
            let tt = ty_text(t);
            // `_value`: the field as a `VList` / `VDict`.
            o.push_str(&format!("\nfn reflect_{n}_{f}_value(xs: &{tt}) -> Value ! alloc\n"));
            if is_list {
                o.push_str("    out = List.new[Value]()\n    i = 0\n    while i < xs.len()\n");
                o.push_str(&format!("        out.push({})\n        i = i + 1\n    VList(out)\n", value_of(e, "xs.get(i)")));
            } else {
                o.push_str("    out = Dict.new[String, Value]()\n    ks = xs.keys()\n    i = 0\n    while i < ks.len()\n");
                o.push_str("        k = ks.get(i)\n        o = xs.get(k)\n");
                o.push_str(&format!("        out.put(k, reflect_{n}_{f}_entry(&o))\n        i = i + 1\n    VDict(out)\n"));
                // `_entry`: the `Value` of an entry, through a typed `&Option[E]`.
                o.push_str(&format!("\nfn reflect_{n}_{f}_entry(o: &Option[{et}]) -> Value ! alloc\n    match o\n"));
                o.push_str(&format!("        Some(x) => {}\n        None => VInt(0)\n", value_of(e, "x")));
            }
            // `_from`: every element converts (`_check`), then the copy is built (`_build`).
            let src_ty = if is_list { "List[Value]" } else { "Dict[String, Value]" };
            o.push_str(&format!("\nfn reflect_{n}_{f}_from(v: &Value) -> Result[{tt}, String] ! alloc\n    match v\n"));
            o.push_str(&format!("        {vk}({coll}) =>\n            errs = reflect_{n}_{f}_check(&{coll})\n"));
            o.push_str(&format!(
                "            if errs.len() == 0\n                out = reflect_{n}_{f}_build(&{coll})\n                Ok(out)\n"
            ));
            o.push_str("            else\n                m = errs.get(0).clone()\n                Err(m)\n");
            mismatch(o, &tt);
            let call = conv(e, "&y", f, &item);
            let walk_head = |o: &mut String, cond: &str| {
                if is_list {
                    o.push_str(&format!("    i = 0\n    while {cond}\n        y = items.get(i)\n"));
                    o.push_str(&format!("        r = {call}\n        match &r\n"));
                } else {
                    o.push_str(&format!("    ks = entries.keys()\n    i = 0\n    while {cond}\n"));
                    o.push_str("        k = ks.get(i)\n        match entries.get(k)\n            Some(y) =>\n");
                    o.push_str(&format!("                r = {call}\n                match &r\n"));
                }
            };
            let pad = if is_list { "            " } else { "                    " };
            let bound = if is_list { "items.len()" } else { "ks.len()" };
            // `_check`: the message of the first element that does not convert, if any.
            o.push_str(&format!(
                "\nfn reflect_{n}_{f}_check({coll}: &{src_ty}) -> List[String] ! alloc\n    errs = List.new[String]()\n"
            ));
            walk_head(o, &format!("i < {bound} && errs.len() == 0"));
            o.push_str(&format!("{pad}Ok(_) => ()\n{pad}Err(e) => errs.push(e.clone())\n"));
            if !is_list {
                o.push_str("            None => ()\n");
            }
            o.push_str("        i = i + 1\n    errs\n");
            let new = if is_list { format!("List.new[{et}]()") } else { format!("Dict.new[String, {et}]()") };
            o.push_str(&format!("\nfn reflect_{n}_{f}_build({coll}: &{src_ty}) -> {tt} ! alloc\n    out = {new}\n"));
            walk_head(o, &format!("i < {bound}"));
            let put = if is_list { format!("out.push({})", copy_of(e, "x")) } else { format!("out.put(k, {})", copy_of(e, "x")) };
            o.push_str(&format!("{pad}Ok(x) => {put}\n{pad}Err(_) => ()\n"));
            if !is_list {
                o.push_str("            None => ()\n");
            }
            o.push_str("        i = i + 1\n    out\n");
            if let Ty::Struct(s) = e.as_ref() {
                struct_from(o, &item, s);
            }
        }
        _ => {}
    }
}
