use crate::{LineCol, SourceFile, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSource {
    /// Physical source identity within one analysis (root is zero).
    pub source_id: usize,
    pub library_key: Option<String>,
    pub source_name: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    /// Absent for the root source. Library spans are relative to this source,
    /// never to the root script passed to an embedding host.
    pub source: Option<Box<DiagnosticSource>>,
}

impl Diagnostic {
    #[must_use]
    pub fn error(code: impl Into<String>, message: impl Into<String>, span: Span) -> Self {
        Self {
            code: code.into(),
            severity: Severity::Error,
            message: message.into(),
            span,
            source: None,
        }
    }

    pub fn attach_source(
        &mut self,
        source_id: usize,
        library_key: Option<&str>,
        source: &SourceFile,
    ) {
        if self.source.is_some() || source_id == 0 {
            return;
        }
        let location = source.line_col(self.span.start);
        self.source = Some(Box::new(DiagnosticSource {
            source_id,
            library_key: library_key.map(str::to_owned),
            source_name: source.name().to_owned(),
            line: location.line,
            column: location.column,
        }));
    }

    #[must_use]
    pub fn line_col(&self, root: &SourceFile) -> LineCol {
        self.source.as_ref().map_or_else(
            || root.line_col(self.span.start),
            |source| LineCol {
                line: source.line,
                column: source.column,
            },
        )
    }

    #[must_use]
    pub fn format(&self, root: &SourceFile) -> String {
        let location = self.line_col(root);
        let source_label = self.source.as_ref().map_or_else(String::new, |source| {
            format!(
                "{} ({}):",
                source.library_key.as_deref().unwrap_or(&source.source_name),
                source.source_name
            )
        });
        format!(
            "{}:{:?}:{}{}:{}: {}",
            self.code, self.severity, source_label, location.line, location.column, self.message
        )
    }
}
