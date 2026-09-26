#!/usr/bin/env python3
"""Generate selfhost/main.sal — native SAL compiler (no embedded C).

Algorithm mirrors tools/recovered_selfhost.c.ref for corpus IR, plus if/cmp
and AST→C emit for stage2.
"""
from __future__ import annotations
import os, sys, textwrap, subprocess, ctypes

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "selfhost", "main.sal")

# ---------- shared tag constants (C-compatible) ----------
TK_EOF, TK_NL, TK_INDENT, TK_DEDENT, TK_IDENT, TK_INT, TK_FLOAT, TK_STRING = range(8)
TK_FN, TK_ON, TK_TO, TK_ARROW, TK_BANG, TK_LPAREN, TK_RPAREN = range(8, 15)
TK_LBRACK, TK_RBRACK, TK_COMMA, TK_COLON, TK_EQ, TK_PLUS, TK_MINUS = range(15, 22)
TK_STAR, TK_SLASH, TK_EQEQ, TK_NE, TK_LT, TK_LE, TK_GT, TK_GE = range(22, 30)
TK_IF, TK_ELSE = 30, 31

EX_INT, EX_FLOAT, EX_STRING, EX_IDENT, EX_BINARY, EX_CALL = range(6)
EX_TO, EX_ON, EX_TENSOR, EX_IF = range(6, 10)

II_CONST_INT, II_CONST_STR, II_BINARY, II_CALL, II_PLACE, II_RETURN, II_DROP, II_IF = range(8)
FO_MATMUL, FO_EPILOGUE, FO_SOFTMAX = 0, 1, 2

