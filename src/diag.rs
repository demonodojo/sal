use serde::{Deserialize, Serialize};

use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    EMoved,
    EEffect,
    EPlace,
    EShape,
    ETensorelem,
    EDevice,
    EDeviceMissing,
    EOom,
    EParse,
    EType,
    EInternal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::EMoved => "E_MOVED",
            ErrorCode::EEffect => "E_EFFECT",
            ErrorCode::EPlace => "E_PLACE",
            ErrorCode::EShape => "E_SHAPE",
            ErrorCode::ETensorelem => "E_TENSOR_ELEM",
            ErrorCode::EDevice => "E_DEVICE",
            ErrorCode::EDeviceMissing => "E_DEVICE_MISSING",
            ErrorCode::EOom => "E_OOM",
            ErrorCode::EParse => "E_PARSE",
            ErrorCode::EType => "E_TYPE",
            ErrorCode::EInternal => "E_INTERNAL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: ErrorCode,
    pub message: String,
    pub span: Span,
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn new(code: ErrorCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            span,
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn format_human(&self) -> String {
        let mut s = format!(
            "error[{}]: {} ({}:{})\n  {}",
            self.code.as_str(),
            self.message,
            self.span.line,
            self.span.col,
            self.message
        );
        if let Some(h) = &self.hint {
            s.push_str(&format!("\n  hint: {h}"));
        }
        s
    }
}

pub type DiagResult<T> = Result<T, Vec<Diagnostic>>;

pub fn ok<T>(v: T) -> DiagResult<T> {
    Ok(v)
}

pub fn err(code: ErrorCode, message: impl Into<String>, span: Span) -> DiagResult<()> {
    Err(vec![Diagnostic::new(code, message, span)])
}
