use super::Analyzer;
use crate::source_graph::SourceContextId;
use pine_ir::CallSiteId;
use pine_syntax::{Diagnostic, Span};

impl Analyzer {
    pub(crate) fn push_diagnostic_in_context(
        &mut self,
        mut diagnostic: Diagnostic,
        context: SourceContextId,
    ) {
        if let Some((source_id, key)) = self.source_context_origins.get(&context)
            && let Some(source) = self.source_texts.get(source_id)
        {
            diagnostic.attach_source(source_id.get(), key.as_deref(), source);
        }
        self.diagnostics.push(diagnostic);
    }

    pub(crate) fn with_source_context<R>(
        &mut self,
        source_context_id: SourceContextId,
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous_context = self.source_context_id.replace(source_context_id);
        let previous_depth = self.source_context_depth.get();
        self.source_context_depth.set(previous_depth + 1);
        let diagnostic_start = self.diagnostics.len();
        let result = operation(self);
        self.attach_diagnostic_sources(source_context_id, diagnostic_start);
        self.source_context_id.set(previous_context);
        self.source_context_depth.set(previous_depth);
        result
    }

    pub(crate) fn with_source_context_ref<R>(
        &self,
        source_context_id: SourceContextId,
        operation: impl FnOnce(&Self) -> R,
    ) -> R {
        let previous_context = self.source_context_id.replace(source_context_id);
        let previous_depth = self.source_context_depth.get();
        self.source_context_depth.set(previous_depth + 1);
        let result = operation(self);
        self.source_context_id.set(previous_context);
        self.source_context_depth.set(previous_depth);
        result
    }

    pub(super) fn attach_diagnostic_sources(&mut self, context: SourceContextId, start: usize) {
        if let Some((source_id, key)) = self.source_context_origins.get(&context)
            && let Some(source) = self.source_texts.get(source_id)
        {
            for diagnostic in &mut self.diagnostics[start..] {
                diagnostic.attach_source(source_id.get(), key.as_deref(), source);
            }
        }
    }

    pub(crate) fn alloc_call_site_at(&mut self, span: Span) -> CallSiteId {
        let id = self.alloc_call_site();
        if let Some((source_id, library_key)) = self
            .source_context_origins
            .get(&self.current_source_context_id())
        {
            self.call_site_sources.push(pine_ir::HirCallSiteSource {
                call_site_id: id,
                source_id: source_id.get(),
                library_key: library_key.clone(),
                start: span.start,
                end: span.end,
            });
        }
        id
    }
}
