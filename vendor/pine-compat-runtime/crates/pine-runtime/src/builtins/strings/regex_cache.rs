//! Lazy compilation is a pure cache; source/pattern evaluation and errors stay
//! at the str.match call site. Checkpoints share compiled patterns by Arc.
use std::sync::Arc;

use pine_ir::{CallSiteId, HirCallArg};
use regex::Regex;

use super::normalize_pine_regex_with_metadata;
use crate::{HistoricalRuntime, PineValue, RuntimeError};

/// A dynamic pattern replaces its predecessor, bounding storage per call site.
pub(crate) struct CachedPineRegex {
    source_pattern: String,
    compiled: Result<(Regex, Vec<String>), RuntimeError>,
}

impl CachedPineRegex {
    fn new(source_pattern: String) -> Self {
        let normalized = normalize_pine_regex_with_metadata(&source_pattern);
        let compiled = Regex::new(&normalized.pattern)
            .map(|regex| (regex, normalized.final_newline_captures))
            .map_err(|err| RuntimeError {
                message: format!("str.match invalid regex: {err}"),
            });
        Self {
            source_pattern,
            compiled,
        }
    }
}

impl HistoricalRuntime<'_> {
    pub(crate) fn eval_str_match_regex(
        &mut self,
        call_site_id: CallSiteId,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        let PineValue::String(source) = self.eval_expr(&args[0].value)? else {
            return Ok(PineValue::Na);
        };
        let PineValue::String(regex) = self.eval_expr(&args[1].value)? else {
            return Ok(PineValue::Na);
        };
        let cached = match self.regex_cache.get(&call_site_id) {
            Some(cached) if cached.source_pattern == regex => Arc::clone(cached),
            _ => {
                let cached = Arc::new(CachedPineRegex::new(regex));
                self.regex_cache.insert(call_site_id, Arc::clone(&cached));
                cached
            }
        };
        let (regex, final_newline_captures) = cached.compiled.as_ref().map_err(Clone::clone)?;
        let Some(captures) = regex.captures(&source) else {
            return Ok(PineValue::String(String::new()));
        };
        let matched = captures
            .get(0)
            .expect("successful regex captures contain the complete match");
        let consumed_final_newline = final_newline_captures
            .iter()
            .any(|name| captures.name(name).is_some());
        let end = matched.end() - usize::from(consumed_final_newline);

        Ok(PineValue::String(source[matched.start()..end].to_owned()))
    }
}
