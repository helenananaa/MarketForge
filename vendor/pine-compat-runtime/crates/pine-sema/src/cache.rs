use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::Arc,
};

use pine_syntax::SourceFile;

use crate::analysis::{Analysis, analyze_input};
use crate::source_graph::AnalysisInput;

#[derive(Debug, Default, Clone)]
pub struct CompileCache {
    entries: HashMap<u64, Vec<(CompileCacheKey, Arc<Analysis>)>>,
    hits: usize,
    misses: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileCacheStats {
    pub entries: usize,
    pub hits: usize,
    pub misses: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CompileCacheKey {
    translator_revision: u32,
    name: String,
    text: String,
    libraries: Vec<CompileCacheLibraryKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CompileCacheLibraryKey {
    key: String,
    name: String,
    text: String,
}
impl CompileCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn analyze(&mut self, source: &SourceFile) -> Analysis {
        (*self.analyze_shared(source)).clone()
    }

    pub fn analyze_input(&mut self, input: &AnalysisInput) -> Analysis {
        (*self.analyze_input_shared(input)).clone()
    }

    /// Reuse an immutable analysis without copying its HIR on cache hits.
    pub fn analyze_shared(&mut self, source: &SourceFile) -> Arc<Analysis> {
        let mut hash = root_fingerprint(source);
        0usize.hash(&mut hash);
        if let Some(bucket) = self.entries.get(&hash.finish())
            && let Some((_, analysis)) = bucket.iter().find(|(key, _)| {
                key.translator_revision == crate::legacy::LEGACY_TRANSLATOR_REVISION
                    && key.name == source.name()
                    && key.text == source.text()
                    && key.libraries.is_empty()
            })
        {
            self.hits += 1;
            return Arc::clone(analysis);
        }
        self.analyze_input_shared(&AnalysisInput::new(source.clone()))
    }

    /// Hash borrowed source text; allocate an owned identity only on a miss.
    /// Hash collisions are checked against the complete root/library identity.
    pub fn analyze_input_shared(&mut self, input: &AnalysisInput) -> Arc<Analysis> {
        self.analyze_fingerprint(input, source_fingerprint(input))
    }

    fn analyze_fingerprint(&mut self, input: &AnalysisInput, fingerprint: u64) -> Arc<Analysis> {
        let bucket = self.entries.entry(fingerprint).or_default();
        if let Some((_, analysis)) = bucket.iter().find(|(key, _)| key.matches_input(input)) {
            self.hits += 1;
            return Arc::clone(analysis);
        }
        self.misses += 1;
        let analysis = Arc::new(analyze_input(input));
        bucket.push((CompileCacheKey::from_input(input), Arc::clone(&analysis)));
        analysis
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.hits = 0;
        self.misses = 0;
    }

    #[must_use]
    pub fn stats(&self) -> CompileCacheStats {
        CompileCacheStats {
            entries: self.entries.values().map(Vec::len).sum(),
            hits: self.hits,
            misses: self.misses,
        }
    }
}
impl CompileCacheKey {
    fn matches_input(&self, input: &AnalysisInput) -> bool {
        self.translator_revision == crate::legacy::LEGACY_TRANSLATOR_REVISION
            && self.name == input.root().name()
            && self.text == input.root().text()
            && self.libraries.len() == input.library_sources().len()
            && self
                .libraries
                .iter()
                .zip(input.library_sources())
                .all(|(key, library)| {
                    key.key == library.key()
                        && key.name == library.source().name()
                        && key.text == library.source().text()
                })
    }

    fn from_input(input: &AnalysisInput) -> Self {
        Self {
            translator_revision: crate::legacy::LEGACY_TRANSLATOR_REVISION,
            name: input.root().name().to_owned(),
            text: input.root().text().to_owned(),
            libraries: input
                .library_sources()
                .iter()
                .map(|library| CompileCacheLibraryKey {
                    key: library.key().to_owned(),
                    name: library.source().name().to_owned(),
                    text: library.source().text().to_owned(),
                })
                .collect(),
        }
    }
}

fn source_fingerprint(input: &AnalysisInput) -> u64 {
    let mut hash = root_fingerprint(input.root());
    input.library_sources().len().hash(&mut hash);
    for library in input.library_sources() {
        library.key().hash(&mut hash);
        library.source().name().hash(&mut hash);
        library.source().text().hash(&mut hash);
    }
    hash.finish()
}

fn root_fingerprint(source: &SourceFile) -> DefaultHasher {
    let mut hash = DefaultHasher::new();
    crate::legacy::LEGACY_TRANSLATOR_REVISION.hash(&mut hash);
    source.name().hash(&mut hash);
    source.text().hash(&mut hash);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_root_hits_share_analysis_with_the_input_api() {
        let source = SourceFile::new(
            "shared.pine",
            "//@version=6\nindicator(\"shared\")\nplot(close)\n",
        );
        let input = AnalysisInput::new(source.clone());
        let mut cache = CompileCache::new();
        let first = cache.analyze_input_shared(&input);
        let second = cache.analyze_shared(&source);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            cache.stats(),
            CompileCacheStats {
                entries: 1,
                hits: 1,
                misses: 1
            }
        );
    }

    #[test]
    fn shared_hits_preserve_identity_and_collisions_check_full_source() {
        let mut cache = CompileCache::new();
        let first = AnalysisInput::new(SourceFile::new(
            "same.pine",
            "//@version=6\nindicator(\"a\")\nplot(close)\n",
        ));
        let second = AnalysisInput::new(SourceFile::new(
            "same.pine",
            "//@version=6\nindicator(\"b\")\nplot(open)\n",
        ));
        let a = cache.analyze_fingerprint(&first, 42);
        let b = cache.analyze_fingerprint(&second, 42);
        assert_ne!(a.hir, b.hir);
        assert!(Arc::ptr_eq(&a, &cache.analyze_fingerprint(&first, 42)));
        assert!(Arc::ptr_eq(&b, &cache.analyze_fingerprint(&second, 42)));
        assert_eq!(
            cache.stats(),
            CompileCacheStats {
                entries: 2,
                hits: 2,
                misses: 2
            }
        );
        cache.clear();
        assert!(a.hir.is_some());
        assert!(b.hir.is_some());
    }

    #[test]
    fn key_carries_legacy_translator_revision() {
        let input = AnalysisInput::new(SourceFile::new("cache.pine", "//@version=6\n"));
        let key = CompileCacheKey::from_input(&input);
        assert_eq!(
            key.translator_revision,
            crate::legacy::LEGACY_TRANSLATOR_REVISION
        );
    }

    #[test]
    fn cache_separates_implicit_v1_and_explicit_v2_dialects() {
        let body = "study(\"cache dialect\")\nplot(close)\n";
        let mut cache = CompileCache::new();
        let v1 = cache.analyze(&SourceFile::new("same.pine", body));
        let v2 = cache.analyze(&SourceFile::new(
            "same.pine",
            format!("//@version=2\n{body}"),
        ));

        assert_eq!(v1.compatibility.language_version, Some(1));
        assert_eq!(v2.compatibility.language_version, Some(2));
        assert_eq!(
            cache.stats(),
            CompileCacheStats {
                entries: 2,
                hits: 0,
                misses: 2,
            }
        );

        cache.analyze(&SourceFile::new("same.pine", body));
        cache.analyze(&SourceFile::new(
            "same.pine",
            format!("//@version=2\n{body}"),
        ));
        assert_eq!(
            cache.stats(),
            CompileCacheStats {
                entries: 2,
                hits: 2,
                misses: 2,
            }
        );
    }
}
