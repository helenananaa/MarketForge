#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallSiteId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirCallSiteSource {
    pub call_site_id: CallSiteId,
    pub source_id: usize,
    pub library_key: Option<String>,
    pub start: usize,
    pub end: usize,
}
