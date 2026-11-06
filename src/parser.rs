use crate::ast::*;
use crate::diag::{Diagnostic, ErrorCode};
use crate::lexer::{Token, TokenKind};
use crate::span::Span;

pub fn parse(source: &str) -> Result<Program, Diagnostic> {
    let tokens = crate::lexer::lex(source)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        group: 0,
        deprecated: None,
    };
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
    /// Depth of `(` and `[`. Inside it, newlines are not statement breaks.
    group: i32,
    /// The message of an `@deprecated("…")` read by `parse_attrs`, taken by the item it precedes.
    deprecated: Option<String>,
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

    /// Leading `@name` markers (and `(arg)` for `@layout`), each with its span.
    fn parse_attrs(&mut self) -> Result<Vec<(String, Span)>, Diagnostic> {
        let mut attrs = Vec::new();
        while let TokenKind::Attr(name) = self.peek().kind.clone() {
            let sp = self.bump().span;
            match name.as_str() {
                "layout" => {
                    self.expect(TokenKind::LParen, "expected (c) after @layout")?;
                    let arg = self.parse_ident()?;
                    if arg != "c" {
                        return Err(Diagnostic::new(
                            ErrorCode::EParse,
                            "only @layout(c) is supported",
                            sp,
                        ));
                    }
                    self.expect(TokenKind::RParen, "expected ) after @layout(c")?;
                }
                "frozen" | "stack" | "no_heap" => {}
                "deprecated" => {
                    self.expect(TokenKind::LParen, "expected (\"message\") after @deprecated")?;
                    let TokenKind::String(msg) = self.peek().kind.clone() else {
                        return Err(Diagnostic::new(
                            ErrorCode::EParse,
                            "@deprecated takes a string message",
                            sp,
                        ));
                    };
                    self.bump();
                    self.expect(TokenKind::RParen, "expected ) after @deprecated message")?;
                    self.deprecated = Some(msg);
                }
                _ => {
                    return Err(Diagnostic::new(
                        ErrorCode::EParse,
                        format!("unknown attribute @{name}"),
                        sp,
                    ))
                }
            }
            attrs.push((name, sp));
            self.skip_newlines();
        }
        Ok(attrs)
    }

    fn reject_attrs(attrs: &[(String, Span)], allowed: &[&str]) -> Result<(), Diagnostic> {
        match attrs.iter().find(|(n, _)| !allowed.contains(&n.as_str())) {
            Some((n, sp)) => Err(Diagnostic::new(
                ErrorCode::EParse,
                format!("@{n} is not allowed here"),
                *sp,
            )),
            None => Ok(()),
        }
    }

    fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        if matches!(self.peek().kind, TokenKind::Attr(_)) {
            let attrs = self.parse_attrs()?;
            let deprecated = self.deprecated.take();
            return match self.parse_item()? {
                Item::Fn(mut f) => {
                    Self::reject_attrs(&attrs, &["no_heap", "deprecated"])?;
                    f.no_heap = attrs.iter().any(|(n, _)| n == "no_heap");
                    f.deprecated = deprecated;
                    Ok(Item::Fn(f))
                }
                Item::Struct(mut st) => {
                    Self::reject_attrs(&attrs, &["layout", "deprecated"])?;
                    st.layout_c = attrs.iter().any(|(n, _)| n == "layout");
                    st.deprecated = deprecated;
                    Ok(Item::Struct(st))
                }
                Item::Enum(mut en) => {
                    Self::reject_attrs(&attrs, &["deprecated"])?;
                    en.deprecated = deprecated;
                    Ok(Item::Enum(en))
                }
                _ => Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "attributes apply to fn, struct or let only",
                    attrs[0].1,
                )),
            };
        }
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
            let alias = if matches!(&self.peek().kind, TokenKind::Ident(n) if n == "as") {
                self.bump();
                Some(self.parse_ident()?)
            } else {
                None
            };
            Ok(Item::Import(Import {
                path,
                alias,
                span: sp,
            }))
        } else if self.at(TokenKind::Struct) {
            Ok(Item::Struct(self.parse_struct()?))
        } else if self.at(TokenKind::Enum) {
            Ok(Item::Enum(self.parse_enum()?))
        } else if self.at(TokenKind::Frame) {
            Ok(Item::Frame(self.parse_frame()?))
        } else if matches!(&self.peek().kind, TokenKind::Ident(n) if n == "protocol")
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::Ident(_)))
        {
            Ok(Item::Protocol(self.parse_protocol()?))
        } else if matches!(&self.peek().kind, TokenKind::Ident(n) if n == "method")
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::Ident(_)))
        {
            self.bump();
            self.parse_method_after_kw().map(Item::Fn)
        } else if matches!(&self.peek().kind, TokenKind::Ident(n) if n == "static")
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::Ident(_)))
            && matches!(self.tokens.get(self.pos + 2).map(|t| &t.kind), Some(TokenKind::Dot))
        {
            self.bump();
            self.parse_static_after_kw().map(Item::Fn)
        } else if matches!(&self.peek().kind, TokenKind::Ident(n) if n == "init")
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::Ident(_)))
            && matches!(self.tokens.get(self.pos + 2).map(|t| &t.kind), Some(TokenKind::LParen))
        {
            self.bump();
            self.parse_init_after_kw().map(Item::Fn)
        } else {
            self.expect(TokenKind::Fn, "expected fn, method, static, init, import, struct, enum or frame")?;
            self.parse_fn_after_kw().map(Item::Fn)
        }
    }

    fn parse_method_after_kw(&mut self) -> Result<FnDef, Diagnostic> {
        let receiver = self.parse_ident()?;
        self.expect(TokenKind::Dot, "expected . after the method receiver type")?;
        let mut f = self.parse_fn_after_kw_as(true, None)?;
        Self::no_type_params(&f, "a method")?;
        f.receiver = Some(receiver);
        Ok(f)
    }

    /// `static T.name(params) -> R`: a function in the namespace of `T`.
    fn parse_static_after_kw(&mut self) -> Result<FnDef, Diagnostic> {
        let receiver = self.parse_ident()?;
        self.expect(TokenKind::Dot, "expected . after the static receiver type")?;
        let mut f = self.parse_fn_after_kw_as(false, None)?;
        Self::no_type_params(&f, "a static method")?;
        f.receiver = Some(receiver);
        f.method_kind = MethodKind::Static;
        Ok(f)
    }

    /// `init T(params)`: the constructor `T.new(params)`; it returns a `T`.
    fn parse_init_after_kw(&mut self) -> Result<FnDef, Diagnostic> {
        let receiver = self.parse_ident()?;
        let mut f = self.parse_fn_after_kw_as(false, Some(&receiver))?;
        f.receiver = Some(receiver);
        f.method_kind = MethodKind::Init;
        Ok(f)
    }

    fn no_type_params(f: &FnDef, what: &str) -> Result<(), Diagnostic> {
        if f.type_params.is_empty() {
            Ok(())
        } else {
            Err(Diagnostic::new(
                ErrorCode::EParse,
                format!("{what} has no type parameters"),
                f.span,
            ))
        }
    }

    fn parse_impl_clause(&mut self) -> Result<Vec<ProtocolClaim>, Diagnostic> {
        if !matches!(&self.peek().kind, TokenKind::Ident(n) if n == "impl") {
            return Ok(Vec::new());
        }
        self.bump();
        let mut list = Vec::new();
        loop {
            let sp = self.peek().span;
            let name = self.parse_ident()?;
            list.push(ProtocolClaim { name, span: sp });
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(list)
    }

    fn parse_protocol(&mut self) -> Result<ProtocolDef, Diagnostic> {
        let sp = self.peek().span;
        let _ = self.bump();
        let name = self.parse_ident()?;
        self.expect(TokenKind::Newline, "expected newline after protocol header")?;
        self.expect(TokenKind::Indent, "expected indented protocol methods")?;
        let mut methods = Vec::new();
        while !self.at(TokenKind::Dedent) && !self.at(TokenKind::Eof) {
            methods.push(self.parse_protocol_method_sig()?);
            self.skip_newlines();
        }
        if self.at(TokenKind::Dedent) {
            self.bump();
        }
        Ok(ProtocolDef {
            name,
            methods,
            span: sp,
        })
    }

    fn parse_protocol_method_sig(&mut self) -> Result<ProtocolMethod, Diagnostic> {
        let sp = self.peek().span;
        if !matches!(&self.peek().kind, TokenKind::Ident(n) if n == "method") {
            return Err(Diagnostic::new(
                ErrorCode::EParse,
                "expected method signature in protocol",
                sp,
            ));
        }
        self.bump();
        let name = self.parse_ident()?;
        self.expect(TokenKind::LParen, "expected ( after protocol method name")?;
        self.skip_soft_layout();
        let mut params = Vec::new();
        let mut self_mode = ParamMode::Inferred;
        if matches!(&self.peek().kind, TokenKind::Ident(n) if n == "self") {
            let next = self.tokens.get(self.pos + 1).map(|t| t.kind.clone());
            let bare = match &next {
                Some(TokenKind::Comma) | Some(TokenKind::RParen) => true,
                Some(TokenKind::Ident(m)) => m == "take" || m == "borrow",
                _ => false,
            };
            if !bare {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "`self` in a protocol method: write `self`, `self take` or `self borrow`",
                    self.peek().span,
                ));
            }
            self.bump();
            if let TokenKind::Ident(m) = self.peek().kind.clone() {
                if m == "take" || m == "borrow" {
                    self.bump();
                    self_mode = if m == "take" { ParamMode::Take } else { ParamMode::Borrow };
                }
            }
            self.skip_soft_layout();
            if self.at(TokenKind::Comma) {
                self.bump();
                self.skip_soft_layout();
            }
        }
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
        self.expect(TokenKind::RParen, "expected ) after protocol method params")?;
        self.expect(TokenKind::Arrow, "expected -> after protocol method params")?;
        let ret = self.parse_type()?;
        let effects = self.parse_effects()?;
        self.expect(TokenKind::Newline, "expected newline after protocol method signature")?;
        Ok(ProtocolMethod {
            name,
            params,
            ret,
            effects,
            self_mode,
            span: sp,
        })
    }

    fn parse_type_params(&mut self) -> Result<Vec<String>, Diagnostic> {
        if !self.at(TokenKind::LBracket) {
            return Ok(Vec::new());
        }
        self.bump();
        self.enter_group();
        let mut params = Vec::new();
        self.skip_group_newlines();
        while !self.at(TokenKind::RBracket) {
            params.push(self.parse_ident()?);
            self.skip_group_newlines();
            if self.at(TokenKind::Comma) {
                self.bump();
                self.skip_group_newlines();
            } else {
                break;
            }
        }
        self.skip_group_newlines();
        self.expect(TokenKind::RBracket, "expected ] after type parameters")?;
        self.leave_group();
        Ok(params)
    }

    fn parse_column_elem(&mut self) -> Result<ColumnElem, Diagnostic> {
        let sp = self.peek().span;
        let name = self.parse_ident()?;
        match name.as_str() {
            "F32" => Ok(ColumnElem::F32),
            "F16" => Ok(ColumnElem::F16),
            "BF16" => Ok(ColumnElem::BF16),
            "I8" => Ok(ColumnElem::I8),
            "String" => Ok(ColumnElem::String),
            _ => Err(Diagnostic::new(
                ErrorCode::EParse,
                format!("expected column type F32, F16, BF16, I8 or String, found `{name}`"),
                sp,
            )),
        }
    }

    fn parse_frame(&mut self) -> Result<FrameDef, Diagnostic> {
        let sp = self.expect(TokenKind::Frame, "expected frame")?.span;
        let name = self.parse_ident()?;
        let type_params = self.parse_type_params()?;
        self.expect(TokenKind::Newline, "expected newline after frame header")?;
        self.expect(TokenKind::Indent, "expected indented frame columns")?;
        let mut columns = Vec::new();
        while !self.at(TokenKind::Dedent) && !self.at(TokenKind::Eof) {
            let csp = self.peek().span;
            let cname = self.parse_ident()?;
            self.expect(TokenKind::Colon, "expected : in frame column")?;
            let elem = self.parse_column_elem()?;
            columns.push(FrameColumn {
                name: cname,
                elem,
                span: csp,
            });
            self.skip_newlines();
        }
        if self.at(TokenKind::Dedent) {
            self.bump();
        }
        Ok(FrameDef {
            name,
            type_params,
            columns,
            span: sp,
        })
    }

    fn parse_struct(&mut self) -> Result<StructDef, Diagnostic> {
        let sp = self.expect(TokenKind::Struct, "expected struct")?.span;
        let name = self.parse_ident()?;
        if name == "any" {
            return Err(Diagnostic::new(
                ErrorCode::EParse,
                "struct name `any` is reserved",
                sp,
            ));
        }
        let type_params = self.parse_type_params()?;
        let protocols = self.parse_impl_clause()?;
        if !type_params.is_empty() && !protocols.is_empty() {
            return Err(Diagnostic::new(
                ErrorCode::EParse,
                "generic struct cannot declare protocol impl",
                protocols[0].span,
            ));
        }
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
            deprecated: None,
            name,
            type_params,
            protocols,
            fields,
            layout_c: false,
            span: sp,
        })
    }

    fn parse_enum(&mut self) -> Result<EnumDef, Diagnostic> {
        let sp = self.expect(TokenKind::Enum, "expected enum")?.span;
        let name = self.parse_ident()?;
        let type_params = self.parse_type_params()?;
        let protocols = self.parse_impl_clause()?;
        if !type_params.is_empty() && !protocols.is_empty() {
            return Err(Diagnostic::new(
                ErrorCode::EParse,
                "generic enum cannot declare protocol impl",
                protocols[0].span,
            ));
        }
        self.expect(TokenKind::Newline, "expected newline after enum header")?;
        self.expect(TokenKind::Indent, "expected indented enum variants")?;
        let mut variants = Vec::new();
        while !self.at(TokenKind::Dedent) && !self.at(TokenKind::Eof) {
            let vsp = self.peek().span;
            let vname = self.parse_ident()?;
            let mut fields = Vec::new();
            if self.at(TokenKind::LParen) {
                self.bump();
                self.enter_group();
                self.skip_group_newlines();
                if !self.at(TokenKind::RParen) {
                    loop {
                        fields.push(self.parse_type()?);
                        self.skip_group_newlines();
                        if self.at(TokenKind::Comma) {
                            self.bump();
                            self.skip_group_newlines();
                        } else {
                            break;
                        }
                    }
                }
                self.skip_group_newlines();
                self.expect(TokenKind::RParen, "expected ) after variant fields")?;
                self.leave_group();
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
            deprecated: None,
            name,
            type_params,
            protocols,
            variants,
            span: sp,
        })
    }

    fn parse_fn_after_kw(&mut self) -> Result<FnDef, Diagnostic> {
        self.parse_fn_after_kw_as(false, None)
    }

    /// With `method`, the list may start with the bare `self`, optionally
    /// followed by `take` or `borrow`; it sets the mode of the implicit `self`.
    /// With `init`, the name is `new`, there is no `->` and the result is `init`.
    fn parse_fn_after_kw_as(&mut self, method: bool, init: Option<&str>) -> Result<FnDef, Diagnostic> {
        let (name, type_params) = match init {
            Some(_) => ("new".to_string(), Vec::new()),
            None => (self.parse_ident()?, self.parse_type_params()?),
        };
        self.expect(TokenKind::LParen, "expected (")?;
        self.skip_soft_layout();
        let mut params = Vec::new();
        let mut self_mode = ParamMode::Inferred;
        if method && matches!(&self.peek().kind, TokenKind::Ident(n) if n == "self") {
            let next = self.tokens.get(self.pos + 1).map(|t| t.kind.clone());
            let bare = match &next {
                Some(TokenKind::Comma) | Some(TokenKind::RParen) => true,
                Some(TokenKind::Ident(m)) => m == "take" || m == "borrow",
                _ => false,
            };
            if !bare {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "`self` is implicit in a method: write `self`, `self take` or `self borrow`",
                    self.peek().span,
                ));
            }
            self.bump();
            if let TokenKind::Ident(m) = self.peek().kind.clone() {
                if m == "take" || m == "borrow" {
                    self.bump();
                    self_mode = if m == "take" { ParamMode::Take } else { ParamMode::Borrow };
                }
            }
            self.skip_soft_layout();
            if self.at(TokenKind::Comma) {
                self.bump();
                self.skip_soft_layout();
            }
        }
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
        let ret = match init {
            Some(t) => Type::Named { name: t.to_string(), args: vec![], span: self.peek().span },
            None => {
                self.expect(TokenKind::Arrow, "expected ->")?;
                self.parse_type()?
            }
        };
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
            deprecated: None,
            name,
            receiver: None,
            self_mode,
            method_kind: MethodKind::Method,
            type_params,
            params,
            ret,
            effects,
            body,
            no_heap: false,
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
            if self.at(TokenKind::Dot) {
                self.bump();
                let member = self.parse_ident()?;
                let mut args = Vec::new();
                if self.at(TokenKind::LBracket) {
                    self.bump();
                    self.enter_group();
                    self.skip_group_newlines();
                    while !self.at(TokenKind::RBracket) {
                        args.push(self.parse_type()?);
                        self.skip_group_newlines();
                        if self.at(TokenKind::Comma) {
                            self.bump();
                            self.skip_group_newlines();
                        }
                    }
                    self.skip_group_newlines();
                    self.expect(TokenKind::RBracket, "expected ]")?;
                    self.leave_group();
                }
                return Ok(Type::Qualified {
                    qual: name,
                    name: member,
                    args,
                    span,
                });
            }
            let mut args = Vec::new();
            if self.at(TokenKind::LBracket) {
                self.bump();
                self.enter_group();
                self.skip_group_newlines();
                while !self.at(TokenKind::RBracket) {
                    args.push(self.parse_type()?);
                    self.skip_group_newlines();
                    if self.at(TokenKind::Comma) {
                        self.bump();
                        self.skip_group_newlines();
                    }
                }
                self.skip_group_newlines();
                self.expect(TokenKind::RBracket, "expected ]")?;
                self.leave_group();
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
        self.enter_group();
        self.skip_group_newlines();
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
            self.skip_group_newlines();
        }
        loop {
            self.skip_group_newlines();
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
            self.skip_group_newlines();
            if self.at(TokenKind::Comma) {
                self.bump();
            }
        }
        self.skip_group_newlines();
        self.expect(TokenKind::RBracket, "expected ]")?;
        self.leave_group();
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
            if self.at(TokenKind::Break) {
                if let Some(e) = pending_expr.take() {
                    stmts.push(Stmt::Expr(e));
                }
                let sp = self.bump().span;
                stmts.push(Stmt::Break { span: sp });
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
            let attrs = if matches!(self.peek().kind, TokenKind::Attr(_)) {
                self.parse_attrs()?
            } else {
                Vec::new()
            };
            if !attrs.is_empty() && !self.at(TokenKind::Let) {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    "@frozen and @stack apply to let only",
                    attrs[0].1,
                ));
            }
            if self.at(TokenKind::Let) {
                if let Some(e) = pending_expr.take() {
                    stmts.push(Stmt::Expr(e));
                }
                if let Some((n, sp)) = attrs.iter().find(|(n, _)| n != "frozen" && n != "stack") {
                    return Err(Diagnostic::new(
                        ErrorCode::EParse,
                        format!("@{n} is not allowed on let"),
                        *sp,
                    ));
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
                    frozen: attrs.iter().any(|(n, _)| n == "frozen"),
                    stack: attrs.iter().any(|(n, _)| n == "stack"),
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

    /// Inside `(…)` or `[…]`, newlines separate tokens of the same expression.
    fn skip_group_newlines(&mut self) {
        if self.group > 0 {
            self.skip_newlines();
        }
    }

    fn enter_group(&mut self) {
        self.group += 1;
    }

    fn leave_group(&mut self) {
        if self.group > 0 {
            self.group -= 1;
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
        self.skip_group_newlines();
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
        let left = self.parse_or()?;
        self.skip_group_newlines();
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

    /// `or → or "||" and | and`
    fn parse_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_and()?;
        loop {
            self.skip_group_newlines();
            if !self.at(TokenKind::PipePipe) {
                break;
            }
            let sp = self.bump().span;
            let right = self.parse_and()?;
            left = Expr::Binary {
                op: BinOp::Or,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `and → and "&&" cmp | cmp`
    fn parse_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_cmp()?;
        loop {
            self.skip_group_newlines();
            if !self.at(TokenKind::AmpAmp) {
                break;
            }
            let sp = self.bump().span;
            let right = self.parse_cmp()?;
            left = Expr::Binary {
                op: BinOp::And,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `cmp → cmp cmp_op bor | bor`. Todas las comparaciones asocian a la izquierda.
    fn parse_cmp(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_bor()?;
        loop {
            self.skip_group_newlines();
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
            let right = self.parse_bor()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `bor → bor "|" bxor | bxor`
    fn parse_bor(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_bxor()?;
        loop {
            self.skip_group_newlines();
            if !self.at(TokenKind::Pipe) {
                break;
            }
            let sp = self.bump().span;
            let right = self.parse_bxor()?;
            left = Expr::Binary {
                op: BinOp::Bor,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `bxor → bxor "^" band | band`
    fn parse_bxor(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_band()?;
        loop {
            self.skip_group_newlines();
            if !self.at(TokenKind::Caret) {
                break;
            }
            let sp = self.bump().span;
            let right = self.parse_band()?;
            left = Expr::Binary {
                op: BinOp::Bxor,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `band → band "&" shift | shift`
    fn parse_band(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_shift()?;
        loop {
            self.skip_group_newlines();
            if !self.at(TokenKind::Amp) {
                break;
            }
            let sp = self.bump().span;
            let right = self.parse_shift()?;
            left = Expr::Binary {
                op: BinOp::Band,
                left: Box::new(left),
                right: Box::new(right),
                span: sp,
            };
        }
        Ok(left)
    }

    /// `shift → shift "<<" add | shift ">>" add | add`
    fn parse_shift(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_add()?;
        loop {
            self.skip_group_newlines();
            let op = match self.peek().kind {
                TokenKind::Shl => BinOp::Shl,
                TokenKind::Shr => BinOp::Shr,
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
            self.skip_group_newlines();
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
            self.skip_group_newlines();
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
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
        self.skip_group_newlines();
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
            self.skip_group_newlines();
            match self.peek().kind {
                TokenKind::LBracket => {
                    let sp = self.bump().span;
                    self.enter_group();
                    self.skip_group_newlines();
                    let mut type_args = Vec::new();
                    while !self.at(TokenKind::RBracket) {
                        type_args.push(self.parse_type_arg()?);
                        self.skip_group_newlines();
                        if self.at(TokenKind::Comma) {
                            self.bump();
                            self.skip_group_newlines();
                        }
                    }
                    self.skip_group_newlines();
                    self.expect(TokenKind::RBracket, "expected ]")?;
                    self.leave_group();
                    self.expect(TokenKind::LParen, "expected ( after type arguments")?;
                    self.enter_group();
                    self.skip_group_newlines();
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            self.skip_group_newlines();
                            if self.at(TokenKind::Comma) {
                                self.bump();
                                self.skip_group_newlines();
                            } else {
                                break;
                            }
                        }
                    }
                    self.skip_group_newlines();
                    self.expect(TokenKind::RParen, "expected )")?;
                    self.leave_group();
                    e = Expr::Call {
                        func: Box::new(e),
                        type_args,
                        args,
                        span: sp,
                    };
                }
                TokenKind::LParen => {
                    let sp = self.bump().span;
                    self.enter_group();
                    self.skip_group_newlines();
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            self.skip_group_newlines();
                            if self.at(TokenKind::Comma) {
                                self.bump();
                                self.skip_group_newlines();
                            } else {
                                break;
                            }
                        }
                    }
                    self.skip_group_newlines();
                    self.expect(TokenKind::RParen, "expected )")?;
                    self.leave_group();
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
                plain_string_literal_expr(s, sp)
            }
            TokenKind::FString(s) => {
                let sp = self.bump().span;
                fstring_literal_expr(s, sp)
            }
            TokenKind::DupString(s) => {
                let sp = self.bump().span;
                let arg = plain_string_literal_expr(s, sp)?;
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
            TokenKind::RegexString(_) => Err(Diagnostic::new(
                ErrorCode::EParse,
                "regex literal is only allowed in a match pattern",
                self.peek().span,
            )),
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
                self.enter_group();
                let e = self.parse_expr()?;
                self.skip_group_newlines();
                self.expect(TokenKind::RParen, "expected )")?;
                self.leave_group();
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
        self.enter_group();
        let mut rows = Vec::new();
        loop {
            self.skip_group_newlines();
            if self.at(TokenKind::RBracket) {
                break;
            }
            self.expect(TokenKind::LBracket, "expected [ for row")?;
            self.enter_group();
            let mut row = Vec::new();
            loop {
                self.skip_group_newlines();
                if self.at(TokenKind::RBracket) {
                    break;
                }
                row.push(self.parse_expr()?);
                self.skip_group_newlines();
                if self.at(TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.skip_group_newlines();
            self.expect(TokenKind::RBracket, "expected ]")?;
            self.leave_group();
            rows.push(row);
            self.skip_group_newlines();
            if self.at(TokenKind::Comma) {
                self.bump();
            }
        }
        self.skip_group_newlines();
        self.expect(TokenKind::RBracket, "expected ]")?;
        self.leave_group();
        Ok(Expr::TensorLit { rows, span })
    }

    fn parse_on(&mut self) -> Result<Expr, Diagnostic> {
        let sp = self.bump().span;
        let place = self.parse_place()?;
        let mut kernel = false;
        let mut kernel_index = None;
        if self.at(TokenKind::Kernel) {
            self.bump();
            kernel = true;
            if self.at_ident() {
                kernel_index = Some(self.parse_kernel_index()?);
            }
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
            kernel_index,
            body,
            span: sp,
        })
    }

    fn parse_kernel_index(&mut self) -> Result<KernelIndex, Diagnostic> {
        let sp = self.peek().span;
        let mut names = vec![self.parse_ident()?];
        while self.at(TokenKind::Comma) {
            self.bump();
            names.push(self.parse_ident()?);
        }
        self.expect(TokenKind::In, "expected `in` after kernel indices")?;
        let ty = self.parse_type()?;
        let shape = match ty {
            Type::Tensor { .. } => KernelShape::Type(ty),
            Type::Named { name, .. } => KernelShape::Binding(name),
            other => {
                return Err(Diagnostic::new(
                    ErrorCode::EParse,
                    format!("expected tensor shape in kernel index, got `{other:?}`"),
                    sp,
                ))
            }
        };
        Ok(KernelIndex {
            names,
            shape,
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
            let body = if self.at(TokenKind::Newline) {
                self.bump();
                self.skip_newlines();
                if self.at(TokenKind::Indent) {
                    self.bump();
                    let block = self.parse_block_inner()?;
                    if self.at(TokenKind::Dedent) {
                        self.bump();
                    }
                    self.skip_newlines();
                    Expr::Block(block)
                } else {
                    return Err(Diagnostic::new(
                        ErrorCode::EParse,
                        "expected indented match arm body",
                        self.peek().span,
                    ));
                }
            } else {
                self.parse_expr()?
            };
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
                if self.at(TokenKind::Dot) {
                    self.bump();
                    let member = self.parse_ident()?;
                    if !self.at(TokenKind::LParen) {
                        // `Enum.Variant` without payload
                        return Ok(Pattern::Variant {
                            qual: Some(name),
                            name: member,
                            args: Vec::new(),
                            span: sp,
                        });
                    }
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
                    Ok(Pattern::Variant {
                        qual: Some(name),
                        name: member,
                        args,
                        span: sp,
                    })
                } else if self.at(TokenKind::LParen) {
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
                    Ok(Pattern::Variant {
                        qual: None,
                        name,
                        args,
                        span: sp,
                    })
                } else {
                    Ok(Pattern::Ident(name, sp))
                }
            }
            TokenKind::Int(n) => {
                let sp = self.bump().span;
                Ok(Pattern::Int(n, sp))
            }
            TokenKind::String(s) => {
                let sp = self.bump().span;
                Ok(Pattern::Str(s, sp))
            }
            TokenKind::RegexString(s) => {
                let sp = self.bump().span;
                Ok(Pattern::Regex(s, sp))
            }
            TokenKind::DupString(_) | TokenKind::FString(_) => Err(Diagnostic::new(
                ErrorCode::EParse,
                "expected pattern",
                self.peek().span,
            )),
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

fn plain_string_literal_expr(s: String, sp: Span) -> Result<Expr, Diagnostic> {
    Ok(Expr::String {
        value: s,
        parts: Vec::new(),
        span: sp,
    })
}

fn fstring_literal_expr(s: String, sp: Span) -> Result<Expr, Diagnostic> {
    let parts = split_fstring_interpolation(&s, sp)?;
    let value = if parts.is_empty() { s.clone() } else { s };
    Ok(Expr::String {
        value,
        parts,
        span: sp,
    })
}

fn split_fstring_interpolation(s: &str, span: Span) -> Result<Vec<StringPart>, Diagnostic> {
    if !s.contains('#') {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut chars = s.chars().peekable();
    let mut saw_interp = false;
    while let Some(c) = chars.next() {
        if c == '#' {
            if chars.peek() == Some(&'#') {
                chars.next();
                if chars.peek() == Some(&'{') {
                    chars.next();
                    lit.push('#');
                    lit.push('{');
                    continue;
                }
                lit.push('#');
                continue;
            }
            if chars.peek() != Some(&'{') {
                lit.push('#');
                continue;
            }
            chars.next(); // '{'
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
        (Ident(x), Ident(y)) | (Attr(x), Attr(y)) => x == y,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => (x - y).abs() < f64::EPSILON,
        (String(x), String(y)) | (DupString(x), DupString(y)) | (FString(x), FString(y)) => x == y,
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
        | (In, In)
        | (Return, Return)
        | (Break, Break)
        | (True, True)
        | (False, False)
        | (Import, Import)
        | (Try, Try)
        | (While, While)
        | (Struct, Struct)
        | (Enum, Enum)
        | (Frame, Frame)
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
        | (Percent, Percent)
        | (Amp, Amp)
        | (Pipe, Pipe)
        | (Caret, Caret)
        | (Shl, Shl)
        | (Shr, Shr)
        | (AmpAmp, AmpAmp)
        | (PipePipe, PipePipe)
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
