use crate::prelude::*;

impl Analyzer {
    pub(super) fn type_of_map_operation(
        &self,
        name: &str,
        args: &[CallArg],
        param_types: &HashMap<String, PineType>,
    ) -> Option<PineType> {
        match name {
            "map.put" | "map.clear" | "map.remove" | "map.put_all" => {
                Some(PineType::new(Qualifier::Series, ValueKind::Void))
            }
            "map.contains" => Some(PineType::new(Qualifier::Series, ValueKind::Bool)),
            "map.copy" => Some(PineType::new(Qualifier::Simple, ValueKind::Map)),
            "map.size" => Some(PineType::new(Qualifier::Simple, ValueKind::Int)),
            "map.keys" | "map.values" => {
                let first_arg = args.first()?;
                let info = self.map_type_of_expr(&first_arg.value)?;
                let element_kind = if name == "map.keys" {
                    info.key_kind
                } else {
                    info.value_kind
                };
                Some(PineType::new(
                    Qualifier::Simple,
                    element_kind.array_kind_from_element_kind()?,
                ))
            }
            "map.get" => {
                let first_arg = args.first()?;
                let info = self.map_type_of_expr(&first_arg.value).or_else(|| {
                    let ExprKind::Identifier(name) = &first_arg.value.kind else {
                        return None;
                    };
                    param_types
                        .get(name)
                        .filter(|pine_type| pine_type.kind == ValueKind::Map)?;
                    None
                })?;
                Some(PineType::new(Qualifier::Series, info.value_kind))
            }
            _ => None,
        }
    }
}
