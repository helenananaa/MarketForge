use crate::*;

/// A request sample owns collection contents, never a child-runtime handle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RequestedValue {
    Scalar(PineValue),
    Array {
        kind: ArrayElementKind,
        values: Vec<PineValue>,
    },
    Tuple(Vec<RequestedValue>),
}

impl HistoricalRuntime<'_> {
    pub(crate) fn freeze_requested_value(
        &mut self,
        value: &PineValue,
    ) -> Result<RequestedValue, RuntimeError> {
        match value {
            PineValue::Array(id) => {
                let kind = self
                    .array_kinds
                    .get(id)
                    .copied()
                    .ok_or_else(|| RuntimeError {
                        message: "requested array handle is invalid".to_owned(),
                    })?;
                if !matches!(
                    kind,
                    ArrayElementKind::Int
                        | ArrayElementKind::Float
                        | ArrayElementKind::Bool
                        | ArrayElementKind::String
                        | ArrayElementKind::Color
                ) {
                    return Err(RuntimeError{message:"requested arrays of reference elements require unsupported graph transfer".to_owned()});
                }
                let Some(source) = self.array_values(*id)? else {
                    return Err(RuntimeError {
                        message: "requested array storage is missing".to_owned(),
                    });
                };
                let copied = crate::runtime::collection_gc::collection_values_allocation_bytes(
                    source.iter(),
                );
                if !self.record_collection_bytes(copied) {
                    return Err(self.resource_budget.collection_error());
                }
                let values = self.array_values_clone(*id)?.ok_or_else(|| RuntimeError {
                    message: "requested array storage is missing".to_owned(),
                })?;
                if values.iter().any(|value| {
                    !matches!(
                        value,
                        PineValue::Int(_)
                            | PineValue::Float(_)
                            | PineValue::Bool(_)
                            | PineValue::String(_)
                            | PineValue::Color(_)
                            | PineValue::Na
                    )
                }) {
                    return Err(RuntimeError {
                        message: "requested scalar array contains an unsupported reference value"
                            .to_owned(),
                    });
                }
                Ok(RequestedValue::Array { kind, values })
            }
            PineValue::Tuple(values) => Ok(RequestedValue::Tuple(
                values
                    .iter()
                    .map(|value| self.freeze_requested_value(value))
                    .collect::<Result<_, _>>()?,
            )),
            PineValue::Int(_)
            | PineValue::Float(_)
            | PineValue::Bool(_)
            | PineValue::String(_)
            | PineValue::Color(_)
            | PineValue::Na
            | PineValue::Void => Ok(RequestedValue::Scalar(value.clone())),
            _ => Err(RuntimeError {
                message: "requested reference value requires unsupported graph transfer".to_owned(),
            }),
        }
    }

    pub(crate) fn import_requested_value(&mut self, value: &RequestedValue) -> PineValue {
        match value {
            RequestedValue::Scalar(value) => value.clone(),
            RequestedValue::Array { kind, values } => {
                self.new_array_from_borrowed_values(*kind, values)
            }
            RequestedValue::Tuple(values) => PineValue::Tuple(
                values
                    .iter()
                    .map(|value| self.import_requested_value(value))
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requested_array_slice_freezes_only_its_visible_contents() {
        let analysis = pine_sema::analyze_source(&pine_syntax::SourceFile::new(
            "slice.pine",
            "//@version=6\nindicator(\"slice\")\nplot(close)\n",
        ));
        let program = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        let PineValue::Array(parent) = runtime.new_array_from_values(
            ArrayElementKind::Float,
            vec![
                PineValue::Float(1.),
                PineValue::Float(2.),
                PineValue::Float(3.),
            ],
        ) else {
            panic!("array")
        };
        let id = runtime.next_array_id;
        runtime.next_array_id += 1;
        runtime.array_kinds.insert(id, ArrayElementKind::Float);
        runtime.array_slices.insert(
            id,
            crate::builtins::arrays::ArraySlice {
                parent_id: parent,
                start: 1,
                len: 2,
            },
        );
        let slice = PineValue::Array(id);
        assert_eq!(
            runtime.freeze_requested_value(&slice).unwrap(),
            RequestedValue::Array {
                kind: ArrayElementKind::Float,
                values: vec![PineValue::Float(2.), PineValue::Float(3.)],
            }
        );
    }

    #[test]
    fn request_array_samples_and_return_slots_are_independent() {
        let analysis = pine_sema::analyze_source(&pine_syntax::SourceFile::new(
            "request.pine",
            "//@version=6\nindicator(\"request\")\nplot(close)\n",
        ));
        let program = analysis.hir.unwrap();
        let mut child = HistoricalRuntime::new(&program);
        let array =
            child.new_array_from_values(ArrayElementKind::Float, vec![PineValue::Float(1.)]);
        let PineValue::Array(id) = array else {
            panic!("array")
        };
        let first = child
            .freeze_requested_value(&PineValue::Tuple(vec![array.clone(), array.clone()]))
            .unwrap();
        child.array_set_value(id, 0, PineValue::Float(2.)).unwrap();
        let second = child.freeze_requested_value(&array).unwrap();
        let mut parent = HistoricalRuntime::new(&program);
        parent.new_array_from_values(ArrayElementKind::Float, vec![PineValue::Float(99.)]);
        let PineValue::Tuple(imported) = parent.import_requested_value(&first) else {
            panic!("tuple")
        };
        assert_ne!(imported[0], imported[1]);
        let PineValue::Array(first_id) = imported[0] else {
            panic!("array")
        };
        assert_ne!(first_id, id);
        assert_eq!(
            parent.array_get_cloned(first_id, 0).unwrap(),
            Some(PineValue::Float(1.))
        );
        parent
            .array_set_value(first_id, 0, PineValue::Float(7.))
            .unwrap();
        let PineValue::Array(other_id) = imported[1] else {
            panic!("array")
        };
        assert_eq!(
            parent.array_get_cloned(other_id, 0).unwrap(),
            Some(PineValue::Float(1.))
        );
        let PineValue::Array(second_id) = parent.import_requested_value(&second) else {
            panic!("array")
        };
        assert_eq!(
            parent.array_get_cloned(second_id, 0).unwrap(),
            Some(PineValue::Float(2.))
        );
        let PineValue::Tuple(repeated) = parent.import_requested_value(&first) else {
            panic!("tuple")
        };
        let PineValue::Array(repeated_id) = repeated[0] else {
            panic!("array")
        };
        assert_eq!(
            parent.array_get_cloned(repeated_id, 0).unwrap(),
            Some(PineValue::Float(1.))
        );
        assert_eq!(
            child.array_get_cloned(id, 0).unwrap(),
            Some(PineValue::Float(2.))
        );
    }
}
