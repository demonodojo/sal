use crate::diag::{Diagnostic, ErrorCode};
use crate::span::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Int(i64),
    Float(f64),
    String(String),
    /// `d"…"` — azúcar de `strdup("…")`; el parser desazucara a `Call`.
    DupString(String),
    Fn,
    Let,
    If,
    Elsif,
    Else,
    Match,
    On,
    To,
    Kernel,
    Return,
    True,
    False,
    Import,
    Try,
    While,
    Struct,
    Enum,
    /// Reservada. El parser la rechaza: no está en el arranque.
    Parallel,
    Cpu,
    Gpu,
    Tpu,
    Arrow,    // ->
    Bang,     // !
    Comma,
    Colon,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Question,
    Dot,
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    FatArrow, // =>
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub fn lex(source: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0usize;
    let mut line = 1u32;
    let mut col = 1u32;
    let mut indent_stack = vec![0usize];

    while i < bytes.len() {
        // Skip comment
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        if bytes[i] == b'\t' {
            return Err(Diagnostic::new(
                ErrorCode::EParse,
                "tab is not allowed",
                Span {
                    start: i as u32,
                    end: i as u32 + 1,
                    line,
                    col,
                },
            ));
        }

        if bytes[i] == b'@' {
            return Err(Diagnostic::new(
                ErrorCode::EParse,
                "attributes are not in this version",
                Span {
                    start: i as u32,
                    end: i as u32 + 1,
                    line,
                    col,
                },
            ));
        }

        if bytes[i] == b'\n' {
            let start = i;
            i += 1;
            line += 1;
            col = 1;
            tokens.push(Token {
                kind: TokenKind::Newline,
                span: Span {
                    start: start as u32,
                    end: start as u32 + 1,
                    line: line - 1,
                    col,
                },
            });
            // Measure indent on next line
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'\n' {
                continue;
            }
            if j >= bytes.len() {
                break;
            }
            let indent = j - i;
            if indent > *indent_stack.last().unwrap() {
                indent_stack.push(indent);
                tokens.push(Token {
                    kind: TokenKind::Indent,
                    span: Span {
                        start: i as u32,
                        end: j as u32,
                        line,
                        col: 1,
                    },
                });
            } else {
                while indent_stack.len() > 1 && indent < *indent_stack.last().unwrap() {
                    indent_stack.pop();
                    tokens.push(Token {
                        kind: TokenKind::Dedent,
                        span: Span {
                            start: i as u32,
                            end: i as u32,
                            line,
                            col: 1,
                        },
                    });
                }
                if indent != *indent_stack.last().unwrap() {
                    return Err(Diagnostic::new(
                        ErrorCode::EParse,
                        "inconsistent indentation",
                        Span {
                            start: i as u32,
                            end: j as u32,
                            line,
                            col: 1,
                        },
                    ));
                }
            }
            i = j;
            col = (indent + 1) as u32;
            continue;
        }

        if bytes[i].is_ascii_whitespace() {
            i += 1;
            col += 1;
            continue;
        }

        let start = i;
        let start_line = line;
        let start_col = col;

        // Two-char ops
        if i + 1 < bytes.len() {
            let two = &source[i..i + 2];
            let kind = match two {
                "->" => Some(TokenKind::Arrow),
                "=>" => Some(TokenKind::FatArrow),
                "==" => Some(TokenKind::EqEq),
                "!=" => Some(TokenKind::Ne),
                "<=" => Some(TokenKind::Le),
                ">=" => Some(TokenKind::Ge),
                _ => None,
            };
            if let Some(k) = kind {
                tokens.push(Token {
                    kind: k,
                    span: Span {
                        start: start as u32,
                        end: (start + 2) as u32,
                        line: start_line,
                        col: start_col,
                    },
                });
                i += 2;
                col += 2;
                continue;
            }
        }

        let ch = bytes[i];
        let single = match ch {
            b'!' => TokenKind::Bang,
            b',' => TokenKind::Comma,
            b':' => TokenKind::Colon,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b'?' => TokenKind::Question,
            b'.' => TokenKind::Dot,
            b'=' => TokenKind::Eq,
            b'+' => TokenKind::Plus,
            b'-' => TokenKind::Minus,
            b'*' => TokenKind::Star,
            b'/' => TokenKind::Slash,
            b'<' => TokenKind::Lt,
            b'>' => TokenKind::Gt,
            b'"' => {
                let s = read_quoted_string(
                    bytes,
                    source,
                    &mut i,
                    &mut col,
                    start,
                    start_line,
                    start_col,
                )?;
                TokenKind::String(s)
            }
            _ if ch.is_ascii_digit() => {
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                    col += 1;
                }
                let txt = &source[start..i];
                if txt.contains('.') {
                    TokenKind::Float(txt.parse().unwrap_or(0.0))
                } else {
                    TokenKind::Int(txt.parse().unwrap_or(0))
                }
            }
            _ if ch.is_ascii_alphabetic() || ch == b'_' => {
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                    col += 1;
                }
                let txt = &source[start..i];
                if txt == "d" && i < bytes.len() && bytes[i] == b'"' {
                    let s = read_quoted_string(
                        bytes,
                        source,
                        &mut i,
                        &mut col,
                        start,
                        start_line,
                        start_col,
                    )?;
                    tokens.push(Token {
                        kind: TokenKind::DupString(s),
                        span: Span {
                            start: start as u32,
                            end: i as u32,
                            line: start_line,
                            col: start_col,
                        },
                    });
                    continue;
                }
                TokenKind::Ident(txt.to_string())
            }
            _ => {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    format!("unexpected character '{ch}'"),
                    Span {
                        start: start as u32,
                        end: start as u32 + 1,
                        line: start_line,
                        col: start_col,
                    },
                ));
            }
        };

        if !matches!(
            single,
            TokenKind::String(_) | TokenKind::DupString(_) | TokenKind::Int(_) | TokenKind::Float(_)
        ) {
            if let TokenKind::Ident(ref name) = single {
                let kw = match name.as_str() {
                    "fn" => TokenKind::Fn,
                    "let" => TokenKind::Let,
                    "if" => TokenKind::If,
                    "elsif" => TokenKind::Elsif,
                    "else" => TokenKind::Else,
                    "match" => TokenKind::Match,
                    "on" => TokenKind::On,
                    "to" => TokenKind::To,
                    "kernel" => TokenKind::Kernel,
                    "return" => TokenKind::Return,
                    "true" => TokenKind::True,
                    "false" => TokenKind::False,
                    "import" => TokenKind::Import,
                    "try" => TokenKind::Try,
                    "while" => TokenKind::While,
                    "struct" => TokenKind::Struct,
                    "enum" => TokenKind::Enum,
                    "parallel" => TokenKind::Parallel,
                    "cpu" => TokenKind::Cpu,
                    "gpu" => TokenKind::Gpu,
                    "tpu" => TokenKind::Tpu,
                    _ => single,
                };
                tokens.push(Token {
                    kind: kw,
                    span: Span {
                        start: start as u32,
                        end: i as u32,
                        line: start_line,
                        col: start_col,
                    },
                });
                continue;
            }
            tokens.push(Token {
                kind: single,
                span: Span {
                    start: start as u32,
                    end: i as u32,
                    line: start_line,
                    col: start_col,
                },
            });
            if !matches!(
                ch,
                b'"' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'_'
            ) {
                i += 1;
                col += 1;
            }
            continue;
        }

        tokens.push(Token {
            kind: single,
            span: Span {
                start: start as u32,
                end: i as u32,
                line: start_line,
                col: start_col,
            },
        });
    }

    while indent_stack.len() > 1 {
        indent_stack.pop();
        tokens.push(Token {
            kind: TokenKind::Dedent,
            span: Span {
                start: source.len() as u32,
                end: source.len() as u32,
                line,
                col,
            },
        });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span {
            start: source.len() as u32,
            end: source.len() as u32,
            line,
            col,
        },
    });
    Ok(tokens)
}

fn read_quoted_string(
    bytes: &[u8],
    source: &str,
    i: &mut usize,
    col: &mut u32,
    err_start: usize,
    start_line: u32,
    start_col: u32,
) -> Result<String, Diagnostic> {
    *i += 1;
    *col += 1;
    let str_start = *i;
    while *i < bytes.len() && bytes[*i] != b'"' {
        if bytes[*i] == b'\\' && *i + 1 < bytes.len() {
            *i += 2;
            *col += 2;
        } else {
            if bytes[*i] == b'\n' {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "unclosed string",
                    Span {
                        start: err_start as u32,
                        end: *i as u32,
                        line: start_line,
                        col: start_col,
                    },
                ));
            }
            *i += 1;
            *col += 1;
        }
    }
    if *i >= bytes.len() {
        return Err(Diagnostic::new(
            ErrorCode::EParse,
            "unclosed string",
            Span {
                start: err_start as u32,
                end: *i as u32,
                line: start_line,
                col: start_col,
            },
        ));
    }
    let s = source[str_start..*i].to_string();
    let s = unescape_string(&s);
    *i += 1;
    *col += 1;
    Ok(s)
}

fn unescape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('0') => out.push('\0'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
