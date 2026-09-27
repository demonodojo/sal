use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode};
use crate::lexer::{Token, TokenKind};
use crate::span::Span;

pub fn parse(source: &str) -> Result<Program, Diagnostic> {
    let tokens = crate::lexer::lex(source)?;
    let mut p = Parser { tokens, pos: 0 };
    let mut items = Vec::new();
    p.skip_layout();
    while !p.at(TokenKind::Eof) {
        items.push(p.parse_item()?);
        p.skip_layout();
    }
    Ok(Program {
        schema_version: AST_SCHEMA_VERSION,
        items,
        type_defs: Vec::new(),
    })
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.pos += 1;
        t
    }

    fn at(&self, k: TokenKind) -> bool {
        token_eq(&self.peek().kind, &k)
    }

    fn at_ident(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Ident(_))
    }

    fn expect(&mut self, k: TokenKind, msg: &str) -> Result<Token, Diagnostic> {
        if self.at(k) {
            Ok(self.bump())
        } else {
            Err(Diagnostic::new(
                ErrorCode::EParse,
                msg,
                self.peek().span,
            ))
        }
    }

    fn skip_layout(&mut self) {
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            self.bump();
        }
    }

    fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        if self.at(TokenKind::Parallel) {
            return Err(Diagnostic::new(
                ErrorCode::EParse,
                "parallel is not in this version",
                self.peek().span,
            ));
        }
        if self.at(TokenKind::Import) {
            let sp = self.bump().span;
            let path = self.parse_import_path()?;
            Ok(Item::Import(Import { path, span: sp }))
        } else if self.at(TokenKind::Struct) {
            Ok(Item::Struct(self.parse_struct()?))
        } else if self.at(TokenKind::Enum) {
            Ok(Item::Enum(self.parse_enum()?))
        } else {
            self.expect(TokenKind::Fn, "expected fn, import, struct or enum")?;
            self.parse_fn_after_kw().map(Item::Fn)
        }
    }

    fn parse_type_params(&mut self) -> Result<Vec<String>, Diagnostic> {
        if !self.at(TokenKind::LBracket) {
            return Ok(Vec::new());
        }
        self.bump();
        let mut params = Vec::new();
        while !self.at(TokenKind::RBracket) {
            params.push(self.parse_ident()?);
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RBracket, "expected ] after type parameters")?;
        Ok(params)
    }

    fn parse_struct(&mut self) -> Result<StructDef, Diagnostic> {
        let sp = self.expect(TokenKind::Struct, "expected struct")?.span;
        let name = self.parse_ident()?;
        let type_params = self.parse_type_params()?;
        self.expect(TokenKind::Newline, "expected newline after struct header")?;
        self.expect(TokenKind::Indent, "expected indented struct fields")?;
        let mut fields = Vec::new();
        while !self.at(TokenKind::Dedent) && !self.at(TokenKind::Eof) {
            let fsp = self.peek().span;
            let fname = self.parse_ident()?;
            self.expect(TokenKind::Colon, "expected : in struct field")?;
            let ty = self.parse_type()?;
            fields.push(StructField {
                name: fname,
                ty,
                span: fsp,
            });
            self.skip_newlines();
        }
        if self.at(TokenKind::Dedent) {
            self.bump();
        }
        Ok(StructDef {
            name,
            type_params,
            fields,
            span: sp,
        })
    }

    fn parse_enum(&mut self) -> Result<EnumDef, Diagnostic> {
        let sp = self.expect(TokenKind::Enum, "expected enum")?.span;
        let name = self.parse_ident()?;
        let type_params = self.parse_type_params()?;
        self.expect(TokenKind::Newline, "expected newline after enum header")?;
        self.expect(TokenKind::Indent, "expected indented enum variants")?;
        let mut variants = Vec::new();
        while !self.at(TokenKind::Dedent) && !self.at(TokenKind::Eof) {
            let vsp = self.peek().span;
            let vname = self.parse_ident()?;
            let mut fields = Vec::new();
            if self.at(TokenKind::LParen) {
                self.bump();
                if !self.at(TokenKind::RParen) {
                    loop {
                        fields.push(self.parse_type()?);
                        if self.at(TokenKind::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RParen, "expected ) after variant fields")?;
            }
            variants.push(EnumVariant {
                name: vname,
                fields,
                span: vsp,
            });
            self.skip_newlines();
        }
        if self.at(TokenKind::Dedent) {
            self.bump();
        }
        Ok(EnumDef {
            name,
            type_params,
            variants,
            span: sp,
        })
    }

    fn parse_fn_after_kw(&mut self) -> Result<FnDef, Diagnostic> {
        let name = self.parse_ident()?;
        let type_params = self.parse_type_params()?;
        self.expect(TokenKind::LParen, "expected (")?;
        self.skip_soft_layout();
        let mut params = Vec::new();
        if !self.at(TokenKind::RParen) {
            loop {
                params.push(self.parse_param()?);
                self.skip_soft_layout();
                if self.at(TokenKind::Comma) {
                    self.bump();
                    self.skip_soft_layout();
                } else {
                    break;
                }
            }
        }
        self.skip_soft_layout();
        self.expect(TokenKind::RParen, "expected )")?;
        self.expect(TokenKind::Arrow, "expected ->")?;
        let ret = self.parse_type()?;
        let effects = self.parse_effects()?;
        self.expect(TokenKind::Newline, "expected newline before body")?;
        self.expect(TokenKind::Indent, "expected indented body")?;
        let body = self.parse_block_inner()?;
        self.skip_newlines();
        while self.at(TokenKind::Dedent) {
            self.bump();
        }
        let span = body.span;
        Ok(FnDef {
            name,
            type_params,
            params,
            ret,
            effects,
            body,
            span,
        })
    }

    fn parse_param(&mut self) -> Result<Param, Diagnostic> {
        let span = self.peek().span;
        let name = self.parse_ident()?;
        self.expect(TokenKind::Colon, "expected : in parameter")?;
        let ty = self.parse_type()?;
        self.skip_soft_layout();
        let mode = match &self.peek().kind {
            TokenKind::Ident(n) if n == "borrow" => {
                self.bump();
                ParamMode::Borrow
            }
            TokenKind::Ident(n) if n == "take" => {
                self.bump();
                ParamMode::Take
            }
            _ => ParamMode::Inferred,
        };
        Ok(Param {
            name,
            ty,
            mode,
            span,
        })
    }

    fn parse_effects(&mut self) -> Result<Vec<Effect>, Diagnostic> {
        if !self.at(TokenKind::Bang) {
            return Ok(Vec::new());
        }
        self.bump();
        let mut effects = Vec::new();
        loop {
            let eff = match self.peek().kind.clone() {
                TokenKind::Ident(ref n) => match n.as_str() {
                    "io" => Effect::Io,
                    "alloc" => Effect::Alloc,
                    "panic" => Effect::Panic,
                    "gpu" => Effect::Gpu,
                    "tpu" => Effect::Tpu,
                    other => {
                        return Err(Diagnostic::new(
                            ErrorCode::EParse,
                            format!("unknown effect `{other}`"),
                            self.peek().span,
                        ))
                    }
                },
                TokenKind::Gpu => Effect::Gpu,
                TokenKind::Tpu => Effect::Tpu,
                _ => {
                    return Err(Diagnostic::new(
                        ErrorCode::EParse,
                        "expected effect name",
                        self.peek().span,
                    ))
                }
            };
            self.bump();
            effects.push(eff);
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(effects)
    }

    fn parse_type(&mut self) -> Result<Type, Diagnostic> {
        let span = self.peek().span;
        if self.at_ident() {
            let name = self.parse_ident()?;
            if name == "Tensor" {
                return self.parse_tensor_type(span);
            }
            let mut args = Vec::new();
            if self.at(TokenKind::LBracket) {
                self.bump();
                while !self.at(TokenKind::RBracket) {
                    args.push(self.parse_type()?);
                    if self.at(TokenKind::Comma) {
                        self.bump();
                    }
                }
                self.expect(TokenKind::RBracket, "expected ]")?;
            }
            return Ok(Type::Named {
                name,
                args,
                span,
            });
        }
        Err(Diagnostic::new(ErrorCode::EParse, "expected type", span))
    }

    fn parse_tensor_type(&mut self, span: Span) -> Result<Type, Diagnostic> {
        self.expect(TokenKind::LBracket, "expected [ after Tensor")?;
        let elem = match self.parse_ident()?.as_str() {
            "F32" => TensorElem::F32,
            "F16" => TensorElem::F16,
            "BF16" => TensorElem::BF16,
            "I8" => TensorElem::I8,
            other => {
                return Err(Diagnostic::new(
                    ErrorCode::ETensorelem,
                    format!("`{other}` is not a tensor element type"),
                    span,
                ))
            }
        };
        let mut dims = Vec::new();
        if self.at(TokenKind::Comma) {
            self.bump();
        }
        loop {
            if self.at(TokenKind::RBracket) {
                break;
            }
            if self.at(TokenKind::Question) {
                self.bump();
                dims.push(Dim::Dynamic);
            } else if matches!(self.peek().kind, TokenKind::Int(_)) {
                if let TokenKind::Int(n) = self.bump().kind {
                    dims.push(Dim::Static(n as u64));
                }
            } else {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "expected dimension",
                    self.peek().span,
                ));
            }
            if self.at(TokenKind::Comma) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBracket, "expected ]")?;
        self.expect(TokenKind::On, "expected `on` in tensor type")?;
        let place = self.parse_place()?;
        Ok(Type::Tensor {
            elem,
            dims,
            place,
            span,
        })
    }

    fn parse_place(&mut self) -> Result<Place, Diagnostic> {
        match self.peek().kind {
            TokenKind::Cpu => {
                self.bump();
                Ok(Place::Cpu)
            }
            TokenKind::Gpu => {
                self.bump();
                Ok(Place::Gpu)
            }
            TokenKind::Tpu => {
                self.bump();
                Ok(Place::Tpu)
            }
            TokenKind::Ident(_) => Ok(Place::Param(self.parse_ident()?)),
            _ => Err(Diagnostic::new(
                ErrorCode::EParse,
                "expected place (cpu, gpu, tpu or parameter)",
                self.peek().span,
            )),
        }
    }

    fn parse_block_inner(&mut self) -> Result<Block, Diagnostic> {
        let start = self.peek().span;
        let mut stmts = Vec::new();
        let mut pending_expr: Option<Expr> = None;
        while !self.at(TokenKind::Dedent) && !self.at(TokenKind::Eof) {
            self.skip_newlines();
            if self.at(TokenKind::Dedent) || self.at(TokenKind::Eof) {
                break;
            }
            if self.at(TokenKind::Return) {
                if let Some(e) = pending_expr.take() {
                    stmts.push(Stmt::Expr(e));
                }
                let sp = self.bump().span;
                let value = if self.at(TokenKind::Newline) || self.at(TokenKind::Dedent) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                stmts.push(Stmt::Return { value, span: sp });
                self.skip_newlines();
                continue;
            }
            if self.at(TokenKind::While) {
                if let Some(e) = pending_expr.take() {
                    stmts.push(Stmt::Expr(e));
                }
                let sp = self.bump().span;
                let cond = self.parse_expr()?;
                self.expect(TokenKind::Newline, "expected newline after while")?;
                self.expect(TokenKind::Indent, "expected indented while body")?;
                let body = self.parse_block_inner()?;
                if self.at(TokenKind::Dedent) {
                    self.bump();
                }
                stmts.push(Stmt::While {
                    cond,
                    body,
                    span: sp,
                });
                self.skip_newlines();
                continue;
            }
            if self.at(TokenKind::Let) {
                if let Some(e) = pending_expr.take() {
                    stmts.push(Stmt::Expr(e));
                }
                let sp = self.bump().span;
                let name = self.parse_ident()?;
                let ty = if self.at(TokenKind::Colon) {
                    self.bump();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.expect(TokenKind::Eq, "expected = in let")?;
                let init = self.parse_expr()?;
                stmts.push(Stmt::Let {
                    name,
                    ty,
                    init,
                    span: sp,
                });
                self.skip_newlines();
                continue;
            }
            let expr = self.parse_expr()?;
            if self.at(TokenKind::Eq) && is_lvalue(&expr) {
                if let Some(e) = pending_expr.take() {
                    stmts.push(Stmt::Expr(e));
                }
                let sp = expr.span();
                self.bump();
                let value = self.parse_expr()?;
                stmts.push(Stmt::Assign {
                    target: expr,
                    value,
                    span: sp,
                });
                self.skip_newlines();
                continue;
            }
            self.skip_newlines();
            if let Some(prev) = pending_expr.take() {
                stmts.push(Stmt::Expr(prev));
            }
            pending_expr = Some(expr);
            if self.at(TokenKind::Dedent) || self.at(TokenKind::Eof) {
                break;
            }
        }
        let tail = pending_expr.map(Box::new);
        let end = self.peek().span;
        Ok(Block {
            stmts,
            tail,
            span: Span::merge(start, end),
        })
    }

    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.bump();
        }
    }

    /// Skip newlines and indent tokens inside flat lists (params, etc.).
    fn skip_soft_layout(&mut self) {
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            self.bump();
        }
    }

    /// `expr → "try" expr | lambda`
    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_try()
    }

    fn parse_try(&mut self) -> Result<Expr, Diagnostic> {
        if self.at(TokenKind::Try) {
            let sp = self.bump().span;
            let expr = self.parse_expr()?;
            let span = Span::merge(sp, expr.span());
            return Ok(Expr::Try {
                expr: Box::new(expr),
                span,
            });
        }
        self.parse_lambda()
    }

    /// `lambda → cmp "=>" expr | cmp`. El cuerpo es un `expr`, así que admite `try`.
    fn parse_lambda(&mut self) -> Result<Expr, Diagnostic> {
        let left = self.parse_cmp()?;
        if self.at(TokenKind::FatArrow) {
            let arrow = self.bump().span;
            let start = left.span();
            let param = match left {
                Expr::Ident { name, .. } => name,
                other => {
                    return Err(Diagnostic::new(
                        ErrorCode::EParse,
                        "lambda parameter must be an identifier",
                        other.span(),
                    ));
                }
            };
            let body = self.parse_expr()?;
            let span = Span::merge(Span::merge(start, arrow), body.span());
            return Ok(Expr::Lambda {
                params: vec![param],
                body: Box::new(body),
                span,
            });
        }
        Ok(left)
    }

    /// `cmp → cmp cmp_op add | add`. Todas las comparaciones asocian a la izquierda.
    fn parse_cmp(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::Ne => BinOp::Ne,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Ge => BinOp::Ge,
                _ => break,
            };
            let sp = self.bump().span;
            let right = self.parse_add()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `add → add "+" mul | add "-" mul | mul`
    fn parse_add(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            let sp = self.bump().span;
            let right = self.parse_mul()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `mul → mul "*" unary | mul "/" unary | unary`
    fn parse_mul(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            let sp = self.bump().span;
            let right = self.parse_unary()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        match self.peek().kind {
            TokenKind::Minus => {
                let sp = self.bump().span;
                let e = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(e),
                    span: sp,
                })
            }
            TokenKind::Bang => {
                let sp = self.bump().span;
                let e = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(e),
                    span: sp,
                })
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek().kind {
                TokenKind::LBracket => {
                    let sp = self.bump().span;
                    let mut type_args = Vec::new();
                    while !self.at(TokenKind::RBracket) {
                        type_args.push(self.parse_type_arg()?);
                        if self.at(TokenKind::Comma) {
                            self.bump();
                        }
                    }
                    self.expect(TokenKind::RBracket, "expected ]")?;
                    self.expect(TokenKind::LParen, "expected ( after type arguments")?;
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.at(TokenKind::Comma) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, "expected )")?;
                    e = Expr::Call {
                        func: Box::new(e),
                        type_args,
                        args,
                        span: sp,
                    };
                }
                TokenKind::LParen => {
                    let sp = self.bump().span;
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.at(TokenKind::Comma) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, "expected )")?;
                    e = Expr::Call {
                        func: Box::new(e),
                        type_args: Vec::new(),
                        args,
                        span: sp,
                    };
                }
                TokenKind::Dot => {
                    self.bump();
                    let field = self.parse_ident()?;
                    let sp = e.span();
                    e = Expr::Field {
                        base: Box::new(e),
                        field,
                        span: sp,
                    };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        match self.peek().kind.clone() {
            TokenKind::Int(n) => {
                let sp = self.bump().span;
                Ok(Expr::Int { value: n, span: sp })
            }
            TokenKind::Float(n) => {
                let sp = self.bump().span;
                Ok(Expr::Float { value: n, span: sp })
            }
            TokenKind::True => {
                let sp = self.bump().span;
                Ok(Expr::Bool {
                    value: true,
                    span: sp,
                })
            }
            TokenKind::False => {
                let sp = self.bump().span;
                Ok(Expr::Bool {
                    value: false,
                    span: sp,
                })
            }
            TokenKind::String(s) => {
                let sp = self.bump().span;
                string_literal_expr(s, sp)
            }
            TokenKind::DupString(s) => {
                let sp = self.bump().span;
                let arg = string_literal_expr(s, sp)?;
                Ok(Expr::Call {
                    func: Box::new(Expr::Ident {
                        name: "strdup".into(),
                        span: sp,
                    }),
                    type_args: Vec::new(),
                    args: vec![arg],
                    span: sp,
                })
            }
            TokenKind::Ident(name) => {
                let sp = self.bump().span;
                if name == "tensor" && self.at(TokenKind::LBracket) {
                    return self.parse_tensor_lit(sp);
                }
                Ok(Expr::Ident { name, span: sp })
            }
            TokenKind::Parallel => Err(Diagnostic::new(
                ErrorCode::EParse,
                "parallel is not in this version",
                self.peek().span,
            )),
            TokenKind::On => self.parse_on(),
            TokenKind::To => self.parse_to(),
            TokenKind::Match => self.parse_match(),
            TokenKind::If => self.parse_if(),
            TokenKind::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(TokenKind::RParen, "expected )")?;
                Ok(e)
            }
            _ => Err(Diagnostic::new(
                ErrorCode::EParse,
                "expected expression",
                self.peek().span,
            )),
        }
    }

    fn parse_tensor_lit(&mut self, span: Span) -> Result<Expr, Diagnostic> {
        self.expect(TokenKind::LBracket, "expected [")?;
        let mut rows = Vec::new();
        while !self.at(TokenKind::RBracket) {
            self.expect(TokenKind::LBracket, "expected [ for row")?;
            let mut row = Vec::new();
            while !self.at(TokenKind::RBracket) {
                row.push(self.parse_expr()?);
                if self.at(TokenKind::Comma) {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBracket, "expected ]")?;
            rows.push(row);
            if self.at(TokenKind::Comma) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBracket, "expected ]")?;
        Ok(Expr::TensorLit { rows, span })
    }

    fn parse_on(&mut self) -> Result<Expr, Diagnostic> {
        let sp = self.bump().span;
        let place = self.parse_place()?;
        let mut kernel = false;
        if self.at(TokenKind::Kernel) {
            self.bump();
            kernel = true;
        }
        self.expect(TokenKind::Newline, "expected newline after on")?;
        self.expect(TokenKind::Indent, "expected indented on body")?;
        let body = self.parse_block_inner()?;
        if self.at(TokenKind::Dedent) {
            self.bump();
        }
        Ok(Expr::On {
            place,
            kernel,
            body,
            span: sp,
        })
    }

    fn parse_to(&mut self) -> Result<Expr, Diagnostic> {
        let sp = self.bump().span;
        let place = self.parse_place()?;
        let expr = self.parse_unary()?;
        Ok(Expr::To {
            place,
            expr: Box::new(expr),
            span: sp,
        })
    }

    fn parse_match(&mut self) -> Result<Expr, Diagnostic> {
        let sp = self.bump().span;
        let scrutinee = self.parse_expr()?;
        self.expect(TokenKind::Newline, "expected newline after match")?;
        self.expect(TokenKind::Indent, "expected match arms")?;
        let mut arms = Vec::new();
        while !self.at(TokenKind::Dedent) {
            let arm_sp = self.peek().span;
            let pattern = self.parse_pattern()?;
            self.expect(TokenKind::FatArrow, "expected =>")?;
            let body = self.parse_expr()?;
            arms.push(MatchArm {
                pattern,
                body,
                span: arm_sp,
            });
            self.skip_newlines();
        }
        self.expect(TokenKind::Dedent, "expected dedent")?;
        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span: sp,
        })
    }

    fn parse_indented_body(&mut self, after_nl: &str, after_indent: &str) -> Result<Block, Diagnostic> {
        self.expect(TokenKind::Newline, after_nl)?;
        self.expect(TokenKind::Indent, after_indent)?;
        let block = self.parse_block_inner()?;
        if self.at(TokenKind::Dedent) {
            self.bump();
        }
        self.skip_newlines();
        Ok(block)
    }

    fn parse_if(&mut self) -> Result<Expr, Diagnostic> {
        let sp = self.bump().span;
        let cond = self.parse_expr()?;
        let then_block = self.parse_indented_body(
            "expected newline after if condition",
            "expected indented if body",
        )?;
        let mut elsifs = Vec::new();
        while self.at(TokenKind::Elsif) {
            let arm_sp = self.bump().span;
            let arm_cond = self.parse_expr()?;
            let body = self.parse_indented_body(
                "expected newline after elsif condition",
                "expected indented elsif body",
            )?;
            elsifs.push(Elsif {
                cond: arm_cond,
                body,
                span: arm_sp,
            });
        }
        let else_block = if self.at(TokenKind::Else) {
            self.bump();
            Some(self.parse_indented_body(
                "expected newline after else",
                "expected indented else body",
            )?)
        } else {
            None
        };
        Ok(Expr::If {
            cond: Box::new(cond),
            then_block,
            elsifs,
            else_block,
            span: sp,
        })
    }

    fn parse_pattern(&mut self) -> Result<Pattern, Diagnostic> {
        match self.peek().kind.clone() {
            TokenKind::Ident(n) if n == "_" => {
                let sp = self.bump().span;
                Ok(Pattern::Wild(sp))
            }
            TokenKind::Ident(_) => {
                let sp = self.peek().span;
                let name = self.parse_ident()?;
                if self.at(TokenKind::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        loop {
                            args.push(self.parse_pattern()?);
                            if self.at(TokenKind::Comma) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, "expected ) in pattern")?;
                    Ok(Pattern::Variant { name, args, span: sp })
                } else {
                    Ok(Pattern::Ident(name, sp))
                }
            }
            TokenKind::Int(n) => {
                let sp = self.bump().span;
                Ok(Pattern::Int(n, sp))
            }
            _ => Err(Diagnostic::new(
                ErrorCode::EParse,
                "expected pattern",
                self.peek().span,
            )),
        }
    }

    fn parse_ident(&mut self) -> Result<String, Diagnostic> {
        match self.bump().kind {
            TokenKind::Ident(n) => Ok(n),
            _ => Err(Diagnostic::new(
                ErrorCode::EParse,
                "expected identifier",
                self.peek().span,
            )),
        }
    }

    /// `path → path "." IDENT | IDENT | STRING`
    fn parse_import_path(&mut self) -> Result<String, Diagnostic> {
        if let TokenKind::String(_) = &self.peek().kind {
            return match self.bump().kind {
                TokenKind::String(s) => Ok(s),
                _ => unreachable!(),
            };
        }
        let mut path = self.parse_ident()?;
        while self.at(TokenKind::Dot) {
            self.bump();
            path.push('.');
            path.push_str(&self.parse_ident()?);
        }
        Ok(path)
    }

    /// `type_arg → type | INT | "?"`
    fn parse_type_arg(&mut self) -> Result<TypeArg, Diagnostic> {
        if let TokenKind::Int(n) = self.peek().kind {
            self.bump();
            return Ok(TypeArg::Dim(Dim::Static(n as u64)));
        }
        if self.at(TokenKind::Question) {
            self.bump();
            return Ok(TypeArg::Dim(Dim::Dynamic));
        }
        Ok(TypeArg::Type(self.parse_type()?))
    }
}

fn is_lvalue(e: &Expr) -> bool {
    matches!(e, Expr::Ident { .. } | Expr::Field { .. })
}

fn string_literal_expr(s: String, sp: Span) -> Result<Expr, Diagnostic> {
    let parts = split_interpolation(&s, sp)?;
    // `{{` / `}}` are brace escapes. When there is no real interpolation,
    // fold them into the string value (IR blobs use Debug-like `{ … }`).
    let value = if parts.is_empty() {
        unescape_doubled_braces(&s)
    } else {
        s
    };
    Ok(Expr::String {
        value,
        parts,
        span: sp,
    })
}

fn unescape_doubled_braces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'{') {
            chars.next();
            out.push('{');
        } else if c == '}' && chars.peek() == Some(&'}') {
            chars.next();
            out.push('}');
        } else {
            out.push(c);
        }
    }
    out
}

fn split_interpolation(s: &str, span: Span) -> Result<Vec<StringPart>, Diagnostic> {
    if !s.contains('{') {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut chars = s.chars().peekable();
    let mut saw_interp = false;
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                lit.push('{');
                continue;
            }
            if !lit.is_empty() {
                parts.push(StringPart::Lit(std::mem::take(&mut lit)));
            }
            let mut name = String::new();
            while let Some(&ch) = chars.peek() {
                if ch == '}' {
                    break;
                }
                name.push(chars.next().unwrap());
            }
            if chars.next() != Some('}') {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "unclosed interpolation in string",
                    span,
                ));
            }
            if name.is_empty()
                || !name
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_alphabetic() || c == '_')
                    .unwrap_or(false)
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "interpolation must be a simple identifier",
                    span,
                ));
            }
            saw_interp = true;
            parts.push(StringPart::Interp(name));
        } else if c == '}' {
            if chars.peek() == Some(&'}') {
                chars.next();
                lit.push('}');
            } else {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "stray `}` in string",
                    span,
                ));
            }
        } else {
            lit.push(c);
        }
    }
    if !saw_interp {
        return Ok(Vec::new());
    }
    if !lit.is_empty() {
        parts.push(StringPart::Lit(lit));
    }
    Ok(parts)
}

fn token_eq(a: &TokenKind, b: &TokenKind) -> bool {
    use TokenKind::*;
    match (a, b) {
        (Ident(x), Ident(y)) => x == y,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => (x - y).abs() < f64::EPSILON,
        (String(x), String(y)) => x == y,
        (Eof, Eof) | (Newline, Newline) | (Indent, Indent) | (Dedent, Dedent) => true,
        (Fn, Fn)
        | (Let, Let)
        | (If, If)
        | (Elsif, Elsif)
        | (Else, Else)
        | (Match, Match)
        | (On, On)
        | (To, To)
        | (Kernel, Kernel)
        | (Return, Return)
        | (True, True)
        | (False, False)
        | (Import, Import)
        | (Try, Try)
        | (While, While)
        | (Struct, Struct)
        | (Enum, Enum)
        | (Parallel, Parallel)
        | (Cpu, Cpu)
        | (Gpu, Gpu)
        | (Tpu, Tpu)
        | (Arrow, Arrow)
        | (Bang, Bang)
        | (Comma, Comma)
        | (Colon, Colon)
        | (LParen, LParen)
        | (RParen, RParen)
        | (LBracket, LBracket)
        | (RBracket, RBracket)
        | (Question, Question)
        | (Dot, Dot)
        | (Eq, Eq)
        | (Plus, Plus)
        | (Minus, Minus)
        | (Star, Star)
        | (Slash, Slash)
        | (EqEq, EqEq)
        | (Ne, Ne)
        | (Lt, Lt)
        | (Le, Le)
        | (Gt, Gt)
        | (Ge, Ge)
        | (FatArrow, FatArrow) => true,
        _ => false,
    }
}
