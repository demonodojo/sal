#!/usr/bin/env python3
"""Pure-Python port of recovered_selfhost.c for corpus IR + extended if/cmp + AST emit_c."""
from __future__ import annotations
import os, sys, re, subprocess, copy
from dataclasses import dataclass, field
from typing import Any, Optional, List, Dict, Tuple

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# ---- tokens ----
(TK_EOF, TK_NL, TK_INDENT, TK_DEDENT, TK_IDENT, TK_INT, TK_FLOAT, TK_STRING,
 TK_FN, TK_ON, TK_TO, TK_ARROW, TK_BANG, TK_LPAREN, TK_RPAREN, TK_LBRACK, TK_RBRACK,
 TK_COMMA, TK_COLON, TK_EQ, TK_PLUS, TK_MINUS, TK_STAR, TK_SLASH,
 TK_EQEQ, TK_NE, TK_LT, TK_LE, TK_GT, TK_GE, TK_IF, TK_ELSE) = range(32)

@dataclass
class Tok:
    kind: int
    text: Optional[str] = None
    ival: int = 0
    fval: float = 0.0

KEYWORDS = {
    "fn": TK_FN, "on": TK_ON, "to": TK_TO, "if": TK_IF, "else": TK_ELSE,
}

def is_ident_start(c: int) -> bool:
    return (65 <= c <= 90) or (97 <= c <= 122) or c == 95

def is_ident(c: int) -> bool:
    return is_ident_start(c) or (48 <= c <= 57)

def is_digit(c: int) -> bool:
    return 48 <= c <= 57

def lex(src: str) -> List[Tok]:
    out: List[Tok] = []
    indent_stack = [0]
    i, n = 0, len(src)
    at_line_start = True
    while i < n:
        if src[i] == '#':
            while i < n and src[i] != '\n':
                i += 1
            continue
        if at_line_start:
            spaces = 0
            while i < n and src[i] == ' ':
                spaces += 1; i += 1
            if i < n and src[i] == '\t':
                spaces += 4; i += 1
            if i < n and (src[i] == '\n' or src[i] == '#'):
                at_line_start = True
                if i < n and src[i] == '#':
                    while i < n and src[i] != '\n':
                        i += 1
                if i < n and src[i] == '\n':
                    i += 1
                continue
            col = spaces
            if col > indent_stack[-1]:
                indent_stack.append(col)
                out.append(Tok(TK_INDENT))
            else:
                while len(indent_stack) > 1 and col < indent_stack[-1]:
                    indent_stack.pop()
                    out.append(Tok(TK_DEDENT))
            at_line_start = False
            continue
        c = src[i]
        if c == '\n':
            out.append(Tok(TK_NL)); i += 1; at_line_start = True; continue
        if c in ' \t\r':
            i += 1; continue
        if c == '-' and i + 1 < n and src[i+1] == '>':
            out.append(Tok(TK_ARROW)); i += 2; continue
        if c == '(' : out.append(Tok(TK_LPAREN)); i += 1; continue
        if c == ')' : out.append(Tok(TK_RPAREN)); i += 1; continue
        if c == '[' : out.append(Tok(TK_LBRACK)); i += 1; continue
        if c == ']' : out.append(Tok(TK_RBRACK)); i += 1; continue
        if c == ',' : out.append(Tok(TK_COMMA)); i += 1; continue
        if c == ':' : out.append(Tok(TK_COLON)); i += 1; continue
        if c == '=' and i + 1 < n and src[i+1] == '=':
            out.append(Tok(TK_EQEQ)); i += 2; continue
        if c == '!' and i + 1 < n and src[i+1] == '=':
            out.append(Tok(TK_NE)); i += 2; continue
        if c == '<' and i + 1 < n and src[i+1] == '=':
            out.append(Tok(TK_LE)); i += 2; continue
        if c == '>' and i + 1 < n and src[i+1] == '=':
            out.append(Tok(TK_GE)); i += 2; continue
        if c == '<' : out.append(Tok(TK_LT)); i += 1; continue
        if c == '>' : out.append(Tok(TK_GT)); i += 1; continue
        if c == '=' : out.append(Tok(TK_EQ)); i += 1; continue
        if c == '!' : out.append(Tok(TK_BANG)); i += 1; continue
        if c == '+' : out.append(Tok(TK_PLUS)); i += 1; continue
        if c == '*' : out.append(Tok(TK_STAR)); i += 1; continue
        if c == '/' : out.append(Tok(TK_SLASH)); i += 1; continue
        if c == '-' and (i + 1 >= n or not is_digit(ord(src[i+1]))):
            out.append(Tok(TK_MINUS)); i += 1; continue
        if c == '"':
            i += 1
            buf = []
            while i < n and src[i] != '"':
                if src[i] == '\\' and i + 1 < n:
                    e = src[i+1]
                    if e == 'n': buf.append('\n')
                    elif e == 't': buf.append('\t')
                    elif e == 'r': buf.append('\r')
                    elif e in '\\"': buf.append(e)
                    else: buf.append('\\'); buf.append(e)
                    i += 2
                else:
                    buf.append(src[i]); i += 1
            if i < n and src[i] == '"': i += 1
            # unescape doubled braces
            s = ''.join(buf)
            s2 = []
            j = 0
            while j < len(s):
                if s[j] == '{' and j+1 < len(s) and s[j+1] == '{':
                    s2.append('{'); j += 2
                elif s[j] == '}' and j+1 < len(s) and s[j+1] == '}':
                    s2.append('}'); j += 2
                else:
                    s2.append(s[j]); j += 1
            out.append(Tok(TK_STRING, ''.join(s2))); continue
        if is_digit(ord(c)) or (c == '-' and i+1 < n and is_digit(ord(src[i+1]))):
            start = i
            if src[i] == '-': i += 1
            while i < n and is_digit(ord(src[i])): i += 1
            is_float = False
            if i < n and src[i] == '.':
                is_float = True; i += 1
                while i < n and is_digit(ord(src[i])): i += 1
            tmp = src[start:i]
            if is_float:
                out.append(Tok(TK_FLOAT, fval=float(tmp)))
            else:
                out.append(Tok(TK_INT, ival=int(tmp)))
            continue
        if is_ident_start(ord(c)):
            start = i; i += 1
            while i < n and is_ident(ord(src[i])): i += 1
            text = src[start:i]
            kind = KEYWORDS.get(text, TK_IDENT)
            out.append(Tok(kind, text))
            continue
        i += 1  # skip unknown
    while len(indent_stack) > 1:
        indent_stack.pop()
        out.append(Tok(TK_DEDENT))
    out.append(Tok(TK_EOF))
    return out


# ---- AST ----
(EX_INT, EX_FLOAT, EX_STRING, EX_IDENT, EX_BINARY, EX_CALL,
 EX_TO, EX_ON, EX_TENSOR, EX_IF) = range(10)

@dataclass
class Expr:
    kind: int
    ival: int = 0
    fval: float = 0.0
    text: Optional[str] = None
    place: Optional[str] = None
    args: List["Expr"] = field(default_factory=list)
    left: Optional["Expr"] = None
    right: Optional["Expr"] = None
    inner: Optional["Expr"] = None
    body: Optional["Block"] = None
    then_block: Optional["Block"] = None
    else_block: Optional["Block"] = None
    cells: List[float] = field(default_factory=list)
    nrows: int = 0
    ncols: int = 0

@dataclass
class Stmt:
    is_assign: bool
    name: Optional[str]
    expr: Expr

@dataclass
class Block:
    stmts: List[Stmt] = field(default_factory=list)
    tail: Optional[Expr] = None

@dataclass
class Param:
    name: str
    ty_raw: str

@dataclass
class FnDef:
    name: str
    params: List[Param]
    body: Block

@dataclass
class Program:
    fns: List[FnDef] = field(default_factory=list)

class Parser:
    def __init__(self, toks: List[Tok]):
        self.toks = toks
        self.i = 0
    def peek(self) -> Tok:
        return self.toks[self.i]
    def advance(self) -> Tok:
        t = self.peek()
        if t.kind != TK_EOF:
            self.i += 1
        return t
    def accept(self, k: int) -> bool:
        if self.peek().kind == k:
            self.advance(); return True
        return False
    def skip_nls(self):
        while self.peek().kind == TK_NL:
            self.advance()

def parse_type_text(p: Parser) -> str:
    parts = []
    depth = 0
    while p.peek().kind != TK_EOF:
        t = p.peek()
        if depth == 0 and t.kind in (TK_NL, TK_BANG, TK_INDENT, TK_DEDENT, TK_EQ):
            break
        if depth == 0 and t.kind in (TK_COMMA, TK_RPAREN):
            break
        if t.kind == TK_IDENT:
            parts.append(t.text or ""); p.advance(); parts.append(" "); continue
        if t.kind == TK_ON: parts.append("on "); p.advance(); continue
        if t.kind == TK_TO: parts.append("to "); p.advance(); continue
        if t.kind == TK_FN: parts.append("fn "); p.advance(); continue
        if t.kind == TK_INT: parts.append(str(t.ival)+" "); p.advance(); continue
        if t.kind == TK_LBRACK: depth += 1; parts.append("["); p.advance(); continue
        if t.kind == TK_RBRACK: depth -= 1; parts.append("]"); p.advance(); continue
        if t.kind == TK_COMMA: parts.append(","); p.advance(); continue
        break
    return "".join(parts).rstrip() or "Int"

def parse_expr(p: Parser) -> Expr:
    return parse_binary(p, 0)

def parse_binary(p: Parser, min_prec: int) -> Expr:
    left = parse_primary(p)
    while True:
        t = p.peek()
        opmap = {
            TK_EQEQ: ("eq", 1), TK_NE: ("ne", 1),
            TK_LT: ("lt", 2), TK_LE: ("le", 2), TK_GT: ("gt", 2), TK_GE: ("ge", 2),
            TK_PLUS: ("add", 3), TK_MINUS: ("sub", 3),
            TK_STAR: ("mul", 4), TK_SLASH: ("div", 4),
        }
        if t.kind not in opmap: break
        op, prec = opmap[t.kind]
        if prec < min_prec: break
        p.advance()
        right = parse_binary(p, prec + 1)
        left = Expr(EX_BINARY, text=op, left=left, right=right)
    return left

def parse_primary(p: Parser) -> Expr:
    t = p.peek()
    if t.kind == TK_INT:
        p.advance(); return Expr(EX_INT, ival=t.ival)
    if t.kind == TK_FLOAT:
        p.advance(); return Expr(EX_FLOAT, fval=t.fval)
    if t.kind == TK_STRING:
        p.advance(); return Expr(EX_STRING, text=t.text)
    if t.kind == TK_IF:
        p.advance()
        cond = parse_expr(p)
        p.skip_nls(); p.accept(TK_INDENT)
        then_b = parse_block_after_indent(p)
        p.skip_nls()
        p.accept(TK_ELSE)
        p.skip_nls(); p.accept(TK_INDENT)
        else_b = parse_block_after_indent(p)
        return Expr(EX_IF, left=cond, then_block=then_b, else_block=else_b)
    if t.kind == TK_TO:
        p.advance()
        pl = p.advance()
        place = pl.text or "cpu"
        inner = parse_expr(p)
        return Expr(EX_TO, place=place, inner=inner)
    if t.kind == TK_ON:
        p.advance()
        pl = p.advance()
        place = pl.text or "p"
        p.skip_nls(); p.accept(TK_INDENT)
        body = parse_block_after_indent(p)
        return Expr(EX_ON, place=place, body=body)
    if t.kind in (TK_IDENT, TK_FN):
        name = t.text or "fn"
        p.advance()
        if name == "tensor" and p.peek().kind == TK_LBRACK:
            p.advance()
            cells = []; ncols = 0; nrows = 0
            if p.peek().kind == TK_LBRACK:
                while p.peek().kind == TK_LBRACK:
                    p.advance(); rowc = 0
                    while p.peek().kind not in (TK_RBRACK, TK_EOF):
                        if p.peek().kind in (TK_FLOAT, TK_INT):
                            v = p.peek().fval if p.peek().kind == TK_FLOAT else float(p.peek().ival)
                            cells.append(v); rowc += 1; p.advance()
                        elif p.peek().kind == TK_COMMA: p.advance()
                        else: p.advance()
                    p.accept(TK_RBRACK)
                    if ncols == 0: ncols = rowc
                    nrows += 1
                    p.accept(TK_COMMA)
            p.accept(TK_RBRACK)
            return Expr(EX_TENSOR, cells=cells, nrows=nrows, ncols=ncols)
        if p.peek().kind == TK_LBRACK:
            d = 0
            while True:
                tt = p.advance()
                if tt.kind == TK_LBRACK: d += 1
                elif tt.kind == TK_RBRACK: d -= 1
                if d <= 0: break
                if p.peek().kind == TK_EOF: break
        if p.peek().kind == TK_LPAREN:
            p.advance()
            args = []
            if p.peek().kind != TK_RPAREN:
                while True:
                    args.append(parse_expr(p))
                    if not p.accept(TK_COMMA): break
            p.accept(TK_RPAREN)
            return Expr(EX_CALL, text=name, args=args)
        return Expr(EX_IDENT, text=name)
    if t.kind == TK_LPAREN:
        p.advance(); e = parse_expr(p); p.accept(TK_RPAREN); return e
    p.advance()
    return Expr(EX_INT, ival=0)

def at_block_end(p: Parser) -> bool:
    return p.peek().kind in (TK_DEDENT, TK_EOF, TK_FN, TK_ELSE)

def parse_block_after_indent(p: Parser) -> Block:
    b = Block()
    p.skip_nls()
    while not at_block_end(p):
        p.skip_nls()
        if at_block_end(p): break
        if p.peek().kind == TK_IDENT:
            save = p.i
            idt = p.advance()
            if p.peek().kind == TK_EQ:
                p.advance()
                ex = parse_expr(p)
                b.stmts.append(Stmt(True, idt.text, ex))
                p.skip_nls(); continue
            p.i = save
        ex = parse_expr(p)
        p.skip_nls()
        if at_block_end(p):
            b.tail = ex; break
        b.stmts.append(Stmt(False, None, ex))
    p.accept(TK_DEDENT)
    return b

def skip_effects(p: Parser):
    if not p.accept(TK_BANG): return
    while p.peek().kind in (TK_IDENT, TK_COMMA):
        p.advance()

def parse_fn(p: Parser) -> FnDef:
    p.accept(TK_FN)
    name = p.advance().text or "fn"
    p.accept(TK_LPAREN)
    params = []
    if p.peek().kind != TK_RPAREN:
        while True:
            pn = p.advance().text or "x"
            if p.accept(TK_COLON):
                ty = parse_type_text(p)
            else:
                ty = "Int"
            params.append(Param(pn, ty))
            if not p.accept(TK_COMMA): break
    p.accept(TK_RPAREN)
    if p.accept(TK_ARROW):
        parse_type_text(p)
    skip_effects(p)
    p.skip_nls(); p.accept(TK_INDENT)
    body = parse_block_after_indent(p)
    return FnDef(name, params, body)

def preprocess_src(src: str) -> str:
    out = []; in_str = False; i = 0
    while i < len(src):
        ch = src[i]
        if in_str:
            out.append(ch)
            if ch == '\\' and i+1 < len(src):
                out.append(src[i+1]); i += 2; continue
            if ch == '"': in_str = False
            i += 1; continue
        if ch == '"':
            in_str = True; out.append(ch); i += 1; continue
        if ch == '?' and (i == 0 or not is_ident(ord(src[i-1]))):
            out.append("QDYN"); i += 1; continue
        out.append(ch); i += 1
    return "".join(out)

def parse_program(src: str) -> Program:
    toks = lex(src)
    p = Parser(toks)
    prog = Program()
    p.skip_nls()
    while p.peek().kind != TK_EOF:
        p.skip_nls()
        if p.peek().kind == TK_FN:
            prog.fns.append(parse_fn(p))
        elif p.peek().kind == TK_EOF:
            break
        else:
            p.advance()
        p.skip_nls()
    return prog


# ---- IR ----
(II_CONST_INT, II_CONST_STR, II_BINARY, II_CALL, II_PLACE, II_RETURN, II_DROP, II_IF) = range(8)
(FO_MATMUL, FO_EPILOGUE, FO_SOFTMAX) = range(3)

@dataclass
class IrInst:
    kind: int
    dest: str = ""
    a: str = ""
    b: str = ""
    c: str = ""
    ival: int = 0
    func: Optional[str] = None
    args: List[str] = field(default_factory=list)
    has_dest: int = 1
    then_body: List["IrInst"] = field(default_factory=list)
    else_body: List["IrInst"] = field(default_factory=list)
    then_val: str = ""
    else_val: str = ""

@dataclass
class FusedOp:
    kind: int
    a: str = ""
    b: str = ""
    c: str = ""

@dataclass
class Region:
    place: str = "p"
    ops: List[FusedOp] = field(default_factory=list)
    fused: int = 0
    has_peak: int = 0
    has_sym: int = 0
    peak: int = 0
    peak_sym: str = ""

@dataclass
class Bind:
    name: str
    ssa: str
    is_tensor: int = 0
    ndims: int = 0
    dims: List[int] = field(default_factory=lambda: [-1]*4)
    concrete: List[int] = field(default_factory=lambda: [-1]*4)
    elem_bytes: int = 4

@dataclass
class LowerCtx:
    counter: int = 0
    insts: List[IrInst] = field(default_factory=list)
    binds: List[Bind] = field(default_factory=list)
    fn_names: List[str] = field(default_factory=list)
    dim_params: List[str] = field(default_factory=list)
    regions: List[Region] = field(default_factory=list)

@dataclass
class IrFn:
    name: str
    dim_params: List[str] = field(default_factory=list)
    insts: List[IrInst] = field(default_factory=list)
    regions: List[Region] = field(default_factory=list)

@dataclass
class IrMod:
    fns: List[IrFn] = field(default_factory=list)

def fresh(cx: LowerCtx) -> str:
    cx.counter += 1
    return f"t{cx.counter}"

def ir_push(cx: LowerCtx, inst: IrInst):
    cx.insts.append(inst)

def cx_bind(cx: LowerCtx, name: str, ssa: str):
    for b in cx.binds:
        if b.name == name:
            b.ssa = ssa; return
    cx.binds.append(Bind(name, ssa))

def cx_find(cx: LowerCtx, name: str) -> Optional[Bind]:
    for b in cx.binds:
        if b.name == name: return b
    return None

def cx_lookup(cx: LowerCtx, name: str) -> str:
    b = cx_find(cx, name)
    return b.ssa if b else name

def is_user_fn(cx: LowerCtx, name: str) -> bool:
    return name in cx.fn_names

def runtime_name(cx: LowerCtx, fname: str) -> str:
    if is_user_fn(cx, fname):
        return fname
    if fname == "matmul":
        return "sal_matmul_f32"
    return f"sal_{fname}"

def push_const_int(cx: LowerCtx, dest: str, v: int):
    ir_push(cx, IrInst(II_CONST_INT, dest=dest, ival=v))

def parse_tensor_type(ty: str, b: Bind):
    b.is_tensor = 0; b.ndims = 0; b.elem_bytes = 4
    if not ty or "Tensor" not in ty: return
    b.is_tensor = 1
    lb = ty.find("[")
    if lb < 0: return
    rb = ty.find("]", lb)
    inner = ty[lb+1: rb if rb>=0 else len(ty)]
    toks = [t.strip() for t in inner.replace(",", " ").split() if t.strip()]
    first = True
    for tok in toks:
        if first:
            first = False
            if tok in ("F16", "BF16"): b.elem_bytes = 2
            elif tok == "I8": b.elem_bytes = 1
            continue
        if b.ndims >= 4: break
        if tok == "QDYN" or tok == "?":
            b.dims[b.ndims] = -1; b.concrete[b.ndims] = -1
        else:
            try:
                v = int(tok); b.dims[b.ndims] = v; b.concrete[b.ndims] = v
            except ValueError:
                b.dims[b.ndims] = -1; b.concrete[b.ndims] = -1
        b.ndims += 1

def collect_fused(e: Optional[Expr], cx: LowerCtx, ops: List[FusedOp]):
    if not e: return
    if e.kind == EX_CALL:
        for a in e.args: collect_fused(a, cx, ops)
        fname = e.text or ""
        if (fname in ("relu", "map")) and ops and ops[-1].kind == FO_MATMUL:
            ops.append(FusedOp(FO_EPILOGUE, a=fname, b=ops[-1].c, c=ops[-1].c+"_epilogue"))
        elif fname == "matmul" and len(e.args) == 2:
            lhs = cx_lookup(cx, e.args[0].text) if e.args[0].kind == EX_IDENT else "tmp"
            rhs = cx_lookup(cx, e.args[1].text) if e.args[1].kind == EX_IDENT else "tmp"
            ops.append(FusedOp(FO_MATMUL, a=lhs, b=rhs, c="mm"))
        elif fname == "softmax":
            inp = cx_lookup(cx, e.args[0].text) if e.args and e.args[0].kind == EX_IDENT else "tmp"
            ops.append(FusedOp(FO_SOFTMAX, a=inp, c="sm"))
    elif e.kind == EX_BINARY:
        collect_fused(e.left, cx, ops); collect_fused(e.right, cx, ops)

def axis_len(cx: LowerCtx, tensor: str, take_row: int) -> str:
    b = cx_find(cx, tensor)
    if not b or not b.is_tensor or b.ndims < 2:
        return f"{tensor}_d{0 if take_row else 1}"
    idx = b.ndims - 2 if take_row else b.ndims - 1
    if b.dims[idx] >= 0:
        d = fresh(cx); push_const_int(cx, d, b.dims[idx]); return d
    if b.concrete[idx] >= 0:
        d = fresh(cx); push_const_int(cx, d, b.concrete[idx]); return d
    return f"{tensor}_d{idx}"

def estimate_peak(r: Region, cx: LowerCtx):
    static_total = 0; sym = ""; saw = 0
    for op in r.ops:
        if op.kind != FO_MATMUL: continue
        l = cx_find(cx, op.a); w = cx_find(cx, op.b)
        if not (l and w and l.is_tensor and w.is_tensor): continue
        saw = 1
        for label, tens in (("act", l), ("w", w)):
            parts = ""; dyn = 0; prod = 1
            for d in range(tens.ndims):
                if tens.dims[d] < 0 and tens.concrete[d] < 0:
                    dyn = 1; parts += ("*?" if parts else "?")
                else:
                    v = tens.dims[d] if tens.dims[d] >= 0 else tens.concrete[d]
                    prod *= v; parts += (("*" if parts else "") + str(v))
            if dyn:
                piece = f"{label}:{parts}*{tens.elem_bytes}"
                sym = sym + ("+" if sym else "") + piece
            else:
                static_total += prod * tens.elem_bytes
        # out
        od = [l.dims[d] for d in range(l.ndims - 1)] + [w.dims[w.ndims - 1]]
        parts = ""; dyn = 0; prod = 1
        for d, dimv in enumerate(od):
            conc = l.concrete[d] if d < l.ndims - 1 else w.concrete[w.ndims - 1]
            if dimv < 0 and conc < 0:
                dyn = 1; parts += ("*?" if parts else "?")
            else:
                v = dimv if dimv >= 0 else conc
                prod *= v; parts += (("*" if parts else "") + str(v))
        if dyn:
            piece = f"out:{parts}*{l.elem_bytes}"
            sym = sym + ("+" if sym else "") + piece
        else:
            static_total += prod * l.elem_bytes
    if sym:
        r.has_sym = 1
        r.peak_sym = f"{static_total}+{sym}" if static_total > 0 else sym
        r.has_peak = 1; r.peak = static_total
    elif saw or static_total > 0:
        r.has_peak = 1; r.peak = static_total

def lower_expr(cx: LowerCtx, e: Optional[Expr]) -> str:
    if not e: return "0"
    if e.kind == EX_INT:
        d = fresh(cx); push_const_int(cx, d, e.ival); return d
    if e.kind == EX_FLOAT:
        d = fresh(cx); push_const_int(cx, d, 0); return d
    if e.kind == EX_STRING:
        d = fresh(cx)
        ir_push(cx, IrInst(II_CONST_STR, dest=d, a=e.text or "")); return d
    if e.kind == EX_IDENT:
        return cx_lookup(cx, e.text or "")
    if e.kind == EX_BINARY:
        l = lower_expr(cx, e.left); r = lower_expr(cx, e.right); d = fresh(cx)
        ir_push(cx, IrInst(II_BINARY, dest=d, a=e.text or "add", b=l, c=r)); return d
    if e.kind == EX_TENSOR:
        return fresh(cx)
    if e.kind == EX_TO:
        src = lower_expr(cx, e.inner); d = fresh(cx)
        ir_push(cx, IrInst(II_PLACE, dest=d, a=src, b=e.place or "cpu"))
        dropn = e.inner.text if e.inner and e.inner.kind == EX_IDENT else "tmp"
        ir_push(cx, IrInst(II_DROP, a=dropn)); return d
    if e.kind == EX_ON:
        ops: List[FusedOp] = []
        if e.body and e.body.tail: collect_fused(e.body.tail, cx, ops)
        has_ep = any(o.kind == FO_EPILOGUE for o in ops)
        has_mm = any(o.kind == FO_MATMUL for o in ops)
        fused = 1 if (has_ep or len(ops) > 1 or has_mm) else 0
        reg = Region(place=e.place or "p", ops=list(ops), fused=fused)
        estimate_peak(reg, cx)
        d = fresh(cx)
        for op in ops:
            if op.kind == FO_MATMUL:
                m = axis_len(cx, op.a, 1); k = axis_len(cx, op.a, 0); n = axis_len(cx, op.b, 0)
                ir_push(cx, IrInst(II_CALL, dest=d, func="sal_matmul_f32", args=[op.a, op.b, m, k, n], has_dest=1))
                break
        cx.regions.append(reg)
        return d
    if e.kind == EX_CALL:
        fname = e.text or ""
        arg_names = [lower_expr(cx, a) for a in e.args]
        d = fresh(cx)
        if fname == "load" and e.args and e.args[0].kind == EX_STRING:
            quoted = f"\"{e.args[0].text or ''}\""
            ir_push(cx, IrInst(II_CALL, dest=d, func="sal_load", args=[quoted], has_dest=1))
            return d
        rname = runtime_name(cx, fname)
        ir_push(cx, IrInst(II_CALL, dest=d, func=rname, args=arg_names, has_dest=1))
        return d
    if e.kind == EX_IF:
        # For corpus we don't need If in IR text matching; for emit_c we handle via AST.
        # Lower If into nested bodies for completeness (bootstrap style).
        c = lower_expr(cx, e.left)
        # Simplified: don't emit II_IF for corpus path — evaluate structure for emit_c separately
        # Actually for IR of selfhost we might print If; for corpus never hit.
        then_cx_insts_start = len(cx.insts)
        # Inline then/else into II_IF
        then_body: List[IrInst] = []; else_body: List[IrInst] = []
        # lower then
        saved = cx.insts; cx.insts = []
        then_env_binds = list(cx.binds)
        if e.then_block:
            for st in e.then_block.stmts:
                if st.is_assign:
                    v = lower_expr(cx, st.expr); cx_bind(cx, st.name or "", v)
                else:
                    lower_expr(cx, st.expr)
            then_val = lower_expr(cx, e.then_block.tail) if e.then_block.tail else (push_const_int(cx, fresh(cx), 0) or cx.insts[-1].dest)
        else:
            then_val = fresh(cx); push_const_int(cx, then_val, 0)
        then_body = cx.insts
        cx.insts = []
        cx.binds = then_env_binds
        if e.else_block:
            for st in e.else_block.stmts:
                if st.is_assign:
                    v = lower_expr(cx, st.expr); cx_bind(cx, st.name or "", v)
                else:
                    lower_expr(cx, st.expr)
            else_val = lower_expr(cx, e.else_block.tail) if e.else_block.tail else fresh(cx)
            if not e.else_block.tail:
                push_const_int(cx, else_val, 0)
        else:
            else_val = fresh(cx); push_const_int(cx, else_val, 0)
        else_body = cx.insts
        cx.insts = saved
        d = fresh(cx)
        ir_push(cx, IrInst(II_IF, dest=d, a=c, then_body=then_body, else_body=else_body, then_val=then_val if isinstance(then_val,str) else str(then_val), else_val=else_val if isinstance(else_val,str) else str(else_val)))
        return d
    return fresh(cx)

def lower_fn(f: FnDef, fn_names: List[str]) -> IrFn:
    cx = LowerCtx(fn_names=list(fn_names))
    for pm in f.params:
        b = Bind(pm.name, pm.name)
        parse_tensor_type(pm.ty_raw, b)
        if b.is_tensor:
            for d in range(b.ndims):
                if b.dims[d] < 0:
                    cx.dim_params.append(f"{b.name}_d{d}")
        cx.binds.append(b)
    for st in f.body.stmts:
        if st.is_assign:
            v = lower_expr(cx, st.expr)
            cx_bind(cx, st.name or "", v)
            if st.expr and st.expr.kind == EX_TENSOR:
                b = cx_find(cx, st.name or "")
                if b:
                    b.is_tensor = 1; b.ndims = 2
                    b.dims[0] = st.expr.nrows; b.dims[1] = st.expr.ncols
                    b.concrete[0] = b.dims[0]; b.concrete[1] = b.dims[1]
        else:
            lower_expr(cx, st.expr)
    if f.body.tail:
        v = lower_expr(cx, f.body.tail)
        ir_push(cx, IrInst(II_RETURN, a=v))
    return IrFn(f.name, list(cx.dim_params), list(cx.insts), list(cx.regions))

def lower_program(prog: Program) -> IrMod:
    names = [f.name for f in prog.fns]
    return IrMod([lower_fn(f, names) for f in prog.fns])

def emit_inst(in_: IrInst) -> str:
    if in_.kind == II_CONST_INT:
        return f'  ConstInt {{ dest: "{in_.dest}", value: {in_.ival} }}\n'
    if in_.kind == II_CONST_STR:
        return f'  ConstString {{ dest: "{in_.dest}", value: "{in_.a}" }}\n'
    if in_.kind == II_BINARY:
        return f'  Binary {{ dest: "{in_.dest}", op: "{in_.a}", left: "{in_.b}", right: "{in_.c}" }}\n'
    if in_.kind == II_CALL:
        dest = f'Some("{in_.dest}")' if in_.has_dest else "None"
        args = ", ".join('"' + a.replace("\\\\","\\\\\\\\").replace('"','\\"') + '"' for a in in_.args)
        # match C/Rust: escape " and \ in args
        arg_parts = []
        for a in in_.args:
            esc = ""
            for ch in a:
                if ch in '"\\': esc += "\\" + ch
                else: esc += ch
            arg_parts.append('"' + esc + '"')
        return f'  Call {{ dest: {dest}, func: "{in_.func or ""}", args: [{", ".join(arg_parts)}] }}\n'
    if in_.kind == II_PLACE:
        return f'  PlaceCopy {{ dest: "{in_.dest}", from: "{in_.a}", to_place: "{in_.b}" }}\n'
    if in_.kind == II_RETURN:
        return f'  Return {{ value: "{in_.a}" }}\n'
    if in_.kind == II_DROP:
        return f'  Drop {{ name: "{in_.a}" }}\n'
    if in_.kind == II_IF:
        # Debug-like format matching bootstrap
        def body(bs):
            return "[" + ", ".join(emit_inst(x).strip() for x in bs) + "]"
        return (f'  If {{ cond: "{in_.a}", then_body: {body(in_.then_body)}, '
                f'else_body: {body(in_.else_body)}, then_val: "{in_.then_val}", '
                f'else_val: "{in_.else_val}", dest: "{in_.dest}" }}\n')
    return ""

def emit_region(r: Region) -> str:
    s = f'  region on {r.place} fused={"true" if r.fused else "false"} peak_bytes='
    if r.has_peak or r.has_sym:
        s += f'{{"{r.place}": {r.peak}}}' if r.has_peak else "{}"
    else:
        s += "{}"
    if r.has_sym:
        s += f' peak_symbolic={{"{r.place}": "{r.peak_sym}"}}'
    s += "\n"
    for op in r.ops:
        if op.kind == FO_MATMUL:
            s += f'    Matmul {{ lhs: "{op.a}", rhs: "{op.b}", dest: "{op.c}" }}\n'
        elif op.kind == FO_EPILOGUE:
            s += (f'    MapEpilogue {{ op: "{op.a}", input: "{op.b}", dest: "{op.c}" }}'
                  f'  # fused epilogue, no intermediate buffer\n')
        elif op.kind == FO_SOFTMAX:
            s += f'    Softmax {{ input: "{op.a}", dest: "{op.c}" }}\n'
    return s

def ir_to_text(m: IrMod) -> str:
    parts = []
    for f in m.fns:
        parts.append(f"fn {f.name}:\n")
        if f.dim_params:
            parts.append("  dim_params=[" + ", ".join(f'"{d}"' for d in f.dim_params) + "]\n")
        for inst in f.insts:
            parts.append(emit_inst(inst))
        for r in f.regions:
            parts.append(emit_region(r))
    return "".join(parts)

def emit_ir(src: str) -> str:
    pre = preprocess_src(src or "")
    prog = parse_program(pre)
    ir = lower_program(prog)
    return ir_to_text(ir)

def main_validate():
    import subprocess
    ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    files = [
        "examples/hello.sal","corpus/moves.sal","corpus/effects.sal","corpus/places.sal",
        "examples/forward.sal","corpus/forward_fused.sal","corpus/load_salt.sal",
        "corpus/instrument_oob.sal","corpus/instrument_leak.sal",
    ]
    ok = True
    for rel in files:
        src = open(os.path.join(ROOT, rel)).read()
        got = emit_ir(src)
        boot = subprocess.check_output(
            ["cargo","run","--offline","--quiet","--","emit","ir",rel],
            cwd=ROOT, env={**os.environ, "CARGO_TARGET_DIR":"/tmp/sal-probe-build"},
        ).decode()
        if got != boot:
            print("DIFF", rel)
            for i,(a,b) in enumerate(zip(got.splitlines(), boot.splitlines())):
                if a!=b:
                    print(" ", i+1, "got:", a); print(" ", i+1, "boot:", b); break
            else:
                print(" len", len(got.splitlines()), len(boot.splitlines()))
            ok = False
        else:
            print("OK", rel)
    print("PASS" if ok else "FAIL")
    return ok

# ---- AST → C (for -o / stage2) ----
RUNTIME_DECLS = r"""#include <stdint.h>
#include <stdlib.h>
void sal_runtime_init(int64_t argc, const char **argv);
char *sal_argv(int64_t i);
char *sal_read_file(const char *path);
int64_t sal_write_file(const char *path, const char *data);
int64_t sal_print_str(const char *data);
int64_t sal_str_eq(const char *a, const char *b);
int64_t sal_str_contains(const char *a, const char *b);
int64_t sal_str_len(const char *s);
char *sal_str_concat(const char *a, const char *b);
char *sal_str_append(char *a, const char *b);
char *sal_strdup(const char *s);
char *sal_select_str(int64_t cond, char *a, char *b);
void sal_free(void *p);
int64_t sal_not(int64_t x);
int64_t sal_gated_print_str(int64_t cond, const char *data);
char *sal_tmp_path(const char *suffix);
int64_t sal_clang(const char *c_path, const char *out_path);
char *sal_char_to_str(int64_t c);
char *sal_int_to_str(int64_t v);
char *sal_str_slice(const char *s, int64_t start, int64_t end);
int64_t sal_str_char(const char *s, int64_t i);
void *sal_vec_new(void);
int64_t sal_vec_push(void *v, int64_t x);
int64_t sal_vec_get(void *v, int64_t i);
int64_t sal_vec_set(void *v, int64_t i, int64_t x);
int64_t sal_vec_len(void *v);
void sal_vec_free(void *v);
int64_t sal_print_i64(int64_t v);

"""

def c_escape_str(s: str) -> str:
    out = ['"']
    for ch in s or "":
        o = ord(ch)
        if ch in '\\"': out.append('\\'+ch)
        elif ch == '\n': out.append('\\n')
        elif ch == '\t': out.append('\\t')
        elif o < 32 or o > 126: out.append(f'\\x{o:02x}')
        else: out.append(ch)
    out.append('"')
    return "".join(out)

def mangled(name: str) -> str:
    return "fn_" + name

# String-returning builtins
STR_BUILTINS = {
    "argv","read_file","str_concat","str_append","str_slice","int_to_str",
    "char_to_str","strdup","select_str","tmp_path",
}
INT_BUILTINS = {
    "print","print_str","argc","str_eq","str_contains","str_len","str_char","not",
    "free","write_file","gated_print_str","vec_push","vec_get","vec_set","vec_len",
    "vec_free","clang","vec_new",
}

def emit_c_expr(e: Expr, user_fns: set, tmp: list, indent: str, declared: set | None = None) -> str:
    if declared is None:
        declared = set()
    """Return C expression string; may append stmts to tmp."""
    if e.kind == EX_INT:
        return str(e.ival)
    if e.kind == EX_FLOAT:
        return "0"
    if e.kind == EX_STRING:
        return f"((int64_t)(uintptr_t)(const char *){c_escape_str(e.text or '')})"
    if e.kind == EX_IDENT:
        return e.text or "0"
    if e.kind == EX_BINARY:
        l = emit_c_expr(e.left, user_fns, tmp, indent, declared)
        r = emit_c_expr(e.right, user_fns, tmp, indent, declared)
        op = e.text or "add"
        cmap = {"add":"+","sub":"-","mul":"*","div":"/","eq":"==","ne":"!=","lt":"<","le":"<=","gt":">","ge":">="}
        return f"({l} {cmap.get(op,'+')} {r})"
    if e.kind == EX_CALL:
        fname = e.text or ""
        args = [emit_c_expr(a, user_fns, tmp, indent, declared) for a in e.args]
        if fname in user_fns:
            return f"{mangled(fname)}({', '.join(args)})"
        if fname == "vec_new":
            return "((int64_t)(uintptr_t)sal_vec_new())"
        if fname in STR_BUILTINS:
            # cast args appropriately
            if fname == "argv":
                return f"((int64_t)(uintptr_t)sal_argv({args[0] if args else 0}))"
            if fname in ("read_file","strdup","tmp_path","int_to_str","char_to_str"):
                a0 = args[0] if args else "0"
                cast = f"(const char *)(uintptr_t){a0}" if fname != "int_to_str" and fname != "char_to_str" else a0
                if fname == "int_to_str":
                    return f"((int64_t)(uintptr_t)sal_int_to_str({a0}))"
                if fname == "char_to_str":
                    return f"((int64_t)(uintptr_t)sal_char_to_str({a0}))"
                return f"((int64_t)(uintptr_t)sal_{fname}({cast}))"
            if fname in ("str_concat","str_append","select_str"):
                a=args[0] if args else "0"; b=args[1] if len(args)>1 else "0"
                if fname == "select_str":
                    c=args[0] if args else "0"; a=args[1] if len(args)>1 else "0"; b=args[2] if len(args)>2 else "0"
                    return f"((int64_t)(uintptr_t)sal_select_str({c},(char*)(uintptr_t){a},(char*)(uintptr_t){b}))"
                if fname == "str_append":
                    return f"((int64_t)(uintptr_t)sal_str_append((char*)(uintptr_t){a},(const char*)(uintptr_t){b}))"
                return f"((int64_t)(uintptr_t)sal_str_concat((const char*)(uintptr_t){a},(const char*)(uintptr_t){b}))"
            if fname == "str_slice":
                a,b,c = (args+[0,0,0])[:3]
                return f"((int64_t)(uintptr_t)sal_str_slice((const char*)(uintptr_t){a},{b},{c}))"
        if fname in INT_BUILTINS or fname.startswith("sal_"):
            rn = fname if fname.startswith("sal_") else f"sal_{fname}"
            if fname == "free":
                tmp.append(f"{indent}sal_free((void*)(uintptr_t){args[0] if args else 0});")
                return "0"
            if fname == "clang":
                a=args[0] if args else "0"; b=args[1] if len(args)>1 else "0"
                return f"sal_clang((const char*)(uintptr_t){a},(const char*)(uintptr_t){b})"
            if fname == "gated_print_str":
                a=args[0] if args else "0"; b=args[1] if len(args)>1 else "0"
                return f"sal_gated_print_str({a},(const char*)(uintptr_t){b})"
            if fname == "write_file":
                a=args[0] if args else "0"; b=args[1] if len(args)>1 else "0"
                return f"sal_write_file((const char*)(uintptr_t){a},(const char*)(uintptr_t){b})"
            if fname in ("str_eq","str_contains"):
                a=args[0] if args else "0"; b=args[1] if len(args)>1 else "0"
                return f"{rn}((const char*)(uintptr_t){a},(const char*)(uintptr_t){b})"
            if fname in ("str_len","str_char"):
                if fname == "str_char":
                    return f"sal_str_char((const char*)(uintptr_t){args[0]},{args[1] if len(args)>1 else 0})"
                return f"sal_str_len((const char*)(uintptr_t){args[0]})"
            if fname == "vec_push":
                return f"sal_vec_push((void*)(uintptr_t){args[0]},{args[1] if len(args)>1 else 0})"
            if fname == "vec_get":
                return f"sal_vec_get((void*)(uintptr_t){args[0]},{args[1] if len(args)>1 else 0})"
            if fname == "vec_set":
                return f"sal_vec_set((void*)(uintptr_t){args[0]},{args[1] if len(args)>1 else 0},{args[2] if len(args)>2 else 0})"
            if fname == "vec_len":
                return f"sal_vec_len((void*)(uintptr_t){args[0]})"
            if fname == "vec_free":
                tmp.append(f"{indent}sal_vec_free((void*)(uintptr_t){args[0]});"); return "0"
            if fname == "not":
                return f"sal_not({args[0] if args else 0})"
            if fname in ("print","print_str"):
                if fname == "print_str":
                    return f"sal_print_str((const char*)(uintptr_t){args[0] if args else 0})"
                return f"sal_print_i64({args[0] if args else 0})"
            return f"{rn}({', '.join(args)})"
        return "0"
    if e.kind == EX_IF:
        c = emit_c_expr(e.left, user_fns, tmp, indent, declared)
        tname = f"__t{len(tmp)}"
        tmp.append(f"{indent}int64_t {tname};")
        tmp.append(f"{indent}if ({c}) {{")
        then_stmts = []
        then_val = emit_c_block(e.then_block, user_fns, then_stmts, indent+"  ", declared)
        tmp.extend(then_stmts)
        tmp.append(f"{indent}  {tname} = {then_val};")
        tmp.append(f"{indent}}} else {{")
        else_stmts = []
        else_val = emit_c_block(e.else_block, user_fns, else_stmts, indent+"  ", declared)
        tmp.extend(else_stmts)
        tmp.append(f"{indent}  {tname} = {else_val};")
        tmp.append(f"{indent}}}")
        return tname
    return "0"

def emit_c_block(b: Optional[Block], user_fns: set, tmp: list, indent: str, declared: set | None = None) -> str:
    if declared is None:
        declared = set()
    if not b:
        return "0"
    for st in b.stmts:
        if st.is_assign:
            val = emit_c_expr(st.expr, user_fns, tmp, indent, declared)
            if st.name in declared:
                tmp.append(f"{indent}{st.name} = {val};")
            else:
                declared.add(st.name)
                tmp.append(f"{indent}int64_t {st.name} = {val};")
        else:
            emit_c_expr(st.expr, user_fns, tmp, indent, declared)
    if b.tail:
        return emit_c_expr(b.tail, user_fns, tmp, indent)
    return "0"

def emit_c_from_ast(prog: Program) -> str:
    user = {f.name for f in prog.fns}
    parts = [RUNTIME_DECLS, "#include <stdint.h>\n"]
    # forward decls
    for f in prog.fns:
        if f.name == "main": continue
        params = ", ".join(f"int64_t {p.name}" for p in f.params) or "void"
        parts.append(f"static int64_t {mangled(f.name)}({params});\n")
    parts.append("\n")
    for f in prog.fns:
        is_main = f.name == "main"
        if is_main:
            parts.append("int main(int argc, char **argv) {\n")
            parts.append("  sal_runtime_init((int64_t)argc, (const char **)argv);\n")
        else:
            params = ", ".join(f"int64_t {p.name}" for p in f.params)
            parts.append(f"static int64_t {mangled(f.name)}({params}) {{\n")
        tmp = []
        val = emit_c_block(f.body, user, tmp, "  ", set())
        parts.extend(s+"\n" for s in tmp)
        if is_main:
            parts.append(f"  return (int)({val});\n}}\n\n")
        else:
            parts.append(f"  return {val};\n}}\n\n")
    return "".join(parts)

def emit_c(src: str) -> str:
    pre = preprocess_src(src or "")
    prog = parse_program(pre)
    return emit_c_from_ast(prog)

def compile_sal_to_bin(src_path: str, out_path: str) -> int:
    src = open(src_path).read()
    ccode = emit_c(src)
    cpath = f"/tmp/sal-py-emit-{os.getpid()}.c"
    open(cpath,"w").write(ccode)
    rdir = os.path.join(ROOT, "runtime")
    cmd = f"clang -O0 -g -o {out_path} {cpath} {rdir}/sal_runtime.c {rdir}/kernels.c {rdir}/instrument.c -lm"
    rc = os.system(cmd + " 2>/tmp/sal-py-clang.err")
    return 0 if rc == 0 else 1



if __name__ == "__main__":
    main_validate()
