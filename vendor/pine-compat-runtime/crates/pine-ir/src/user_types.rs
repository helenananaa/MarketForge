#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirUserTypeIdentity {
    pub source_id: usize,
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirUserTypeInfo {
    /// Name in the declaring module, independent of import aliases.
    pub declaration_name: String,
    pub identity: HirUserTypeIdentity,
    pub fields: Vec<HirUserTypeField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirUserTypeField {
    pub varip: bool,
    pub name: String,
    pub user_type_name: Option<String>,
}
