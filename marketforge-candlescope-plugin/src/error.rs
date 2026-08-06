use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidContract,
    InvalidState,
    NotFound,
    Core,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterError {
    kind: ErrorKind,
    code: &'static str,
    message: String,
    path: Option<String>,
}

impl AdapterError {
    pub fn new(kind: ErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            path: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidContract, "INVALID_CONTRACT", message)
    }

    pub fn invalid_at(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::invalid(message).with_path(path)
    }

    pub fn invalid_state(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidState, code, message)
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, code, message)
    }

    pub fn core(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Core, "MARKETFORGE_CORE_ERROR", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, "INTERNAL_ERROR", message)
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(formatter, "{} at {path}", self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl Error for AdapterError {}
