use pine_ir::HirCallArg;

use super::arrays::{ArrayElementKind, array_value_for_kind};
use crate::runtime::collection_gc::collection_values_allocation_bytes;
use crate::{HistoricalRuntime, PineValue, RuntimeError};

#[path = "maps/storage.rs"]
mod storage;
use storage::MapEntries;

// Pine counts a map key and its value as two collection elements.
const MAX_MAP_ENTRIES: usize = crate::MAX_ARRAY_ELEMENTS / 2;

fn map_capacity_error(function: &str) -> RuntimeError {
    RuntimeError {
        message: format!("{function} cannot exceed {MAX_MAP_ENTRIES} key-value pairs"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapStorage {
    pub(crate) key_kind: ArrayElementKind,
    pub(crate) value_kind: ArrayElementKind,
    pub(crate) entries: MapEntries,
}

impl<'a> HistoricalRuntime<'a> {
    pub(crate) fn eval_map_call(
        &mut self,
        callee: &str,
        args: &[HirCallArg],
    ) -> Option<Result<PineValue, RuntimeError>> {
        if !(callee.starts_with("map.new<") || callee.starts_with("map.")) {
            return None;
        }

        Some(match callee {
            name if is_supported_map_new_name(name) => self.eval_map_new(name, args),
            "map.put" => self.eval_map_put(args),
            "map.get" => self.eval_map_get(args),
            "map.contains" => self.eval_map_contains(args),
            "map.clear" => self.eval_map_clear(args),
            "map.remove" => self.eval_map_remove(args),
            "map.copy" => self.eval_map_copy(args),
            "map.put_all" => self.eval_map_put_all(args),
            "map.size" => self.eval_map_size(args),
            "map.keys" => self.eval_map_keys(args),
            "map.values" => self.eval_map_values(args),
            _ => {
                return Some(Err(RuntimeError {
                    message: format!("unsupported runtime call `{callee}`"),
                }));
            }
        })
    }

    fn eval_map_new(
        &mut self,
        callee: &str,
        args: &[HirCallArg],
    ) -> Result<PineValue, RuntimeError> {
        if !args.is_empty() {
            return Err(RuntimeError {
                message: "map.new does not accept arguments in the current subset".to_owned(),
            });
        }
        let Some((key_kind, value_kind)) = parse_map_new_kinds(callee) else {
            return Err(RuntimeError {
                message: "unsupported map.new template".to_owned(),
            });
        };
        let id = self.next_map_id;
        self.next_map_id += 1;
        self.map_store.insert(
            id,
            MapStorage {
                key_kind,
                value_kind,
                entries: MapEntries::default(),
            },
        );
        Ok(PineValue::Map(id))
    }

    fn eval_map_put(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(id_arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.put expects a map argument".to_owned(),
            });
        };
        let id = self.eval_expr(&id_arg.value)?;
        let PineValue::Map(id) = id else {
            if let Some(key_arg) = crate::builtins::args::positional_arg(args, 1) {
                let _ = self.eval_expr(&key_arg.value)?;
            }
            if let Some(value_arg) = crate::builtins::args::positional_arg(args, 2) {
                let _ = self.eval_expr(&value_arg.value)?;
            }
            return Ok(PineValue::Void);
        };
        let Some(storage) = self.map_store.get(&id) else {
            return Ok(PineValue::Void);
        };
        let key_kind = storage.key_kind;
        let value_kind = storage.value_kind;
        let key = self.eval_map_key(&args[1], key_kind)?;
        let value = self.eval_map_value(&args[2], value_kind)?;
        // Arguments may have mutated this map. Locate only after both have
        // finished, and account for the original payload before store COW.
        let (lookup, copied) = if let Some(storage) = self.map_store.get(&id) {
            let lookup = storage.entries.lookup_for_put(&key);
            if storage.entries.len() >= MAX_MAP_ENTRIES && !lookup.is_present() {
                return Err(map_capacity_error("map.put"));
            }
            let copied = storage
                .entries
                .put_allocation_bytes_for_lookup(&lookup, self.map_store.get_mut_clones_value(&id));
            (Some(lookup), copied)
        } else {
            (None, 0)
        };
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        if !self.record_collection_values([&key, &value]) {
            return Err(self.resource_budget.collection_error());
        }
        if let (Some(storage), Some(lookup)) = (self.map_store.get_mut(&id), lookup) {
            storage.entries.put_with_lookup(key, value, lookup);
        }
        Ok(PineValue::Void)
    }

    fn eval_map_get(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(id_arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.get expects a map argument".to_owned(),
            });
        };
        let id = self.eval_expr(&id_arg.value)?;
        let PineValue::Map(id) = id else {
            if let Some(key_arg) = crate::builtins::args::positional_arg(args, 1) {
                let _ = self.eval_expr(&key_arg.value)?;
            }
            return Ok(PineValue::Na);
        };
        let Some(storage) = self.map_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        let key_kind = storage.key_kind;
        let key = self.eval_map_key(&args[1], key_kind)?;
        Ok(self
            .map_store
            .get(&id)
            .and_then(|storage| storage.entries.get(&key).cloned())
            .unwrap_or(PineValue::Na))
    }

    fn eval_map_contains(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(id_arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.contains expects a map argument".to_owned(),
            });
        };
        let id = self.eval_expr(&id_arg.value)?;
        let PineValue::Map(id) = id else {
            if let Some(key_arg) = crate::builtins::args::positional_arg(args, 1) {
                let _ = self.eval_expr(&key_arg.value)?;
            }
            return Ok(PineValue::Bool(false));
        };
        let Some(storage) = self.map_store.get(&id) else {
            return Ok(PineValue::Bool(false));
        };
        let key_kind = storage.key_kind;
        let key = self.eval_map_key(&args[1], key_kind)?;
        Ok(PineValue::Bool(self.map_store.get(&id).is_some_and(
            |storage| storage.entries.get(&key).is_some(),
        )))
    }

    fn eval_map_clear(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(id_arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.clear expects a map argument".to_owned(),
            });
        };
        let id = self.eval_expr(&id_arg.value)?;
        let PineValue::Map(id) = id else {
            return Ok(PineValue::Void);
        };
        let copied = self.map_store.get(&id).map_or(0, |storage| {
            if self.map_store.get_mut_clones_value(&id) {
                storage.entries.clone_allocation_bytes()
            } else {
                0
            }
        });
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        if let Some(storage) = self.map_store.get_mut(&id) {
            storage.entries.clear();
        }
        Ok(PineValue::Void)
    }

    fn eval_map_remove(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(id_arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.remove expects a map argument".to_owned(),
            });
        };
        let id = self.eval_expr(&id_arg.value)?;
        let PineValue::Map(id) = id else {
            if let Some(key_arg) = crate::builtins::args::positional_arg(args, 1) {
                let _ = self.eval_expr(&key_arg.value)?;
            }
            return Ok(PineValue::Void);
        };
        let Some(storage) = self.map_store.get(&id) else {
            return Ok(PineValue::Void);
        };
        let key_kind = storage.key_kind;
        let key = self.eval_map_key(&args[1], key_kind)?;
        // Locate after key evaluation, which may have changed the map, and
        // measure original payload capacities before store-entry COW.
        let (lookup, copied) = if let Some(storage) = self.map_store.get(&id) {
            let lookup = storage.entries.lookup_for_remove(&key);
            let copied = storage.entries.remove_allocation_bytes_for_lookup(
                &lookup,
                self.map_store.get_mut_clones_value(&id),
            );
            (Some(lookup), copied)
        } else {
            (None, 0)
        };
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        if let (Some(storage), Some(lookup)) = (self.map_store.get_mut(&id), lookup) {
            storage.entries.remove_with_lookup(lookup);
        }
        Ok(PineValue::Void)
    }

    fn eval_map_copy(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(id_arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.copy expects a map argument".to_owned(),
            });
        };
        let id = self.eval_expr(&id_arg.value)?;
        let PineValue::Map(id) = id else {
            return Ok(PineValue::Na);
        };
        Ok(self.copy_map(id))
    }

    fn eval_map_put_all(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(target_arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.put_all expects a target map argument".to_owned(),
            });
        };
        let target = self.eval_expr(&target_arg.value)?;
        let source = if let Some(source_arg) = crate::builtins::args::positional_arg(args, 1) {
            self.eval_expr(&source_arg.value)?
        } else {
            return Err(RuntimeError {
                message: "map.put_all expects a source map argument".to_owned(),
            });
        };
        let (PineValue::Map(target_id), PineValue::Map(source_id)) = (target, source) else {
            return Ok(PineValue::Void);
        };
        if target_id == source_id || !self.validate_map_merge(target_id, source_id)? {
            return Ok(PineValue::Void);
        }
        let Some(copied) = self
            .map_store
            .get(&source_id)
            .map(|storage| storage.entries.clone_allocation_bytes())
        else {
            return Ok(PineValue::Void);
        };
        if !self.record_collection_bytes(copied) {
            return Err(self.resource_budget.collection_error());
        }
        let source_entries = self
            .map_store
            .get(&source_id)
            .expect("validated map")
            .entries
            .clone();
        for (key, value) in source_entries.iter() {
            if !self.record_map_put_pressure(target_id, key) {
                return Err(self.resource_budget.collection_error());
            }
            if !self.record_collection_values([key, value]) {
                return Err(self.resource_budget.collection_error());
            }
            self.map_store
                .get_mut(&target_id)
                .expect("target map")
                .entries
                .put(key.clone(), value.clone());
        }
        Ok(PineValue::Void)
    }

    fn validate_map_merge(&self, target_id: u32, source_id: u32) -> Result<bool, RuntimeError> {
        let (Some(target), Some(source)) = (
            self.map_store.get(&target_id),
            self.map_store.get(&source_id),
        ) else {
            return Ok(false);
        };
        let available = MAX_MAP_ENTRIES.saturating_sub(target.entries.len());
        if source.entries.len() <= available {
            return Ok(true);
        }
        let mut additional = 0;
        // Preflight the distinct source keys before any overwrite or append.
        // Argument evaluation has already completed; an oversized merge leaves
        // the target entries and their insertion order unchanged.
        for (key, _) in source.entries.iter() {
            if target.entries.get(key).is_none() {
                additional += 1;
                if additional > available {
                    return Err(map_capacity_error("map.put_all"));
                }
            }
        }
        Ok(true)
    }

    fn eval_map_size(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.size expects a map argument".to_owned(),
            });
        };
        match self.eval_expr(&arg.value)? {
            PineValue::Map(id) => Ok(self.map_store.get(&id).map_or(PineValue::Na, |storage| {
                PineValue::Int(storage.entries.len() as i64)
            })),
            PineValue::Na => Ok(PineValue::Na),
            _ => Err(RuntimeError {
                message: "map.size receiver is not a map".to_owned(),
            }),
        }
    }

    fn eval_map_keys(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.keys expects a map argument".to_owned(),
            });
        };
        let PineValue::Map(id) = self.eval_expr(&arg.value)? else {
            return Ok(PineValue::Na);
        };
        let Some(storage) = self.map_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        let kind = storage.key_kind;
        let bytes = collection_values_allocation_bytes(storage.entries.iter().map(|(key, _)| key));
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let values = self
            .map_store
            .get(&id)
            .expect("validated map")
            .entries
            .iter()
            .map(|(key, _)| key.clone())
            .collect();
        Ok(self.insert_precharged_array_values(kind, values))
    }

    fn eval_map_values(&mut self, args: &[HirCallArg]) -> Result<PineValue, RuntimeError> {
        let Some(arg) = args.first() else {
            return Err(RuntimeError {
                message: "map.values expects a map argument".to_owned(),
            });
        };
        let PineValue::Map(id) = self.eval_expr(&arg.value)? else {
            return Ok(PineValue::Na);
        };
        let Some(storage) = self.map_store.get(&id) else {
            return Ok(PineValue::Na);
        };
        let kind = storage.value_kind;
        let bytes =
            collection_values_allocation_bytes(storage.entries.iter().map(|(_, value)| value));
        if !self.record_collection_bytes(bytes) {
            return Err(self.resource_budget.collection_error());
        }
        let values = self
            .map_store
            .get(&id)
            .expect("validated map")
            .entries
            .iter()
            .map(|(_, value)| value.clone())
            .collect();
        Ok(self.insert_precharged_array_values(kind, values))
    }

    fn eval_map_key(
        &mut self,
        arg: &HirCallArg,
        kind: ArrayElementKind,
    ) -> Result<PineValue, RuntimeError> {
        let value = array_value_for_kind(kind, self.eval_expr(&arg.value)?);
        match value {
            PineValue::Na => Err(RuntimeError {
                message: "map keys cannot be na".to_owned(),
            }),
            PineValue::Float(value) if !value.is_finite() => Err(RuntimeError {
                message: "map float keys must be finite".to_owned(),
            }),
            value => Ok(value),
        }
    }

    fn eval_map_value(
        &mut self,
        arg: &HirCallArg,
        kind: ArrayElementKind,
    ) -> Result<PineValue, RuntimeError> {
        Ok(array_value_for_kind(kind, self.eval_expr(&arg.value)?))
    }

    pub(crate) fn copy_map(&mut self, source_id: u32) -> PineValue {
        let Some(source) = self.map_store.get(&source_id) else {
            return PineValue::Na;
        };
        let bytes = source.entries.clone_allocation_bytes();
        if !self.record_collection_bytes(bytes) {
            return PineValue::Na;
        }
        let source = self
            .map_store
            .get(&source_id)
            .expect("validated map")
            .clone();
        let id = self.next_map_id;
        self.next_map_id += 1;
        self.map_store.insert(id, source);
        PineValue::Map(id)
    }

    fn record_map_put_pressure(&mut self, id: u32, key: &PineValue) -> bool {
        let copied = self.map_store.get(&id).map_or(0, |storage| {
            storage
                .entries
                .put_allocation_bytes(key, self.map_store.get_mut_clones_value(&id))
        });
        self.record_collection_bytes(copied)
    }
}

fn is_supported_map_new_name(name: &str) -> bool {
    parse_map_new_types(name).is_some_and(|(key, value)| {
        is_supported_map_scalar_type(key) && is_supported_map_scalar_type(value)
    })
}

fn parse_map_new_types(name: &str) -> Option<(&str, &str)> {
    let inner = name.strip_prefix("map.new<")?.strip_suffix('>')?;
    inner.split_once(',')
}

fn is_supported_map_scalar_type(name: &str) -> bool {
    matches!(name, "int" | "float" | "bool" | "string" | "color")
}

fn parse_map_new_kinds(name: &str) -> Option<(ArrayElementKind, ArrayElementKind)> {
    let (key, value) = parse_map_new_types(name)?;
    Some((map_scalar_kind(key)?, map_scalar_kind(value)?))
}

fn map_scalar_kind(name: &str) -> Option<ArrayElementKind> {
    match name {
        "int" => Some(ArrayElementKind::Int),
        "float" => Some(ArrayElementKind::Float),
        "bool" => Some(ArrayElementKind::Bool),
        "string" => Some(ArrayElementKind::String),
        "color" => Some(ArrayElementKind::Color),
        _ => None,
    }
}

fn map_keys_equal(left: &PineValue, right: &PineValue) -> bool {
    match (left, right) {
        (PineValue::Int(left), PineValue::Int(right)) => left == right,
        (PineValue::Float(left), PineValue::Float(right)) => left == right,
        (PineValue::Bool(left), PineValue::Bool(right)) => left == right,
        (PineValue::String(left), PineValue::String(right)) => left == right,
        (PineValue::Color(left), PineValue::Color(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
#[path = "maps/capacity_tests.rs"]
mod capacity_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bar, BarUpdate, RealtimeRuntime};

    #[test]
    fn rejected_copy_projection_and_merge_do_not_insert_payloads() {
        for operation in [
            "map.copy(source)",
            "map.keys(source)",
            "map.values(source)",
            "map.put_all(target,source)",
        ] {
            let source = pine_syntax::SourceFile::new(
                "map-budget.pine",
                format!(
                    "//@version=6\nindicator(\"budget\")\nvar source=map.new<int,string>()\nvar target=map.new<int,string>()\nif barstate.isfirst\n    map.put(source,1,str.repeat(\"x\",1024))\nif not barstate.isfirst\n    {operation}\nplot(map.size(source))\n"
                ),
            );
            let analysis = pine_sema::analyze_source(&source);
            assert!(
                analysis.diagnostics.is_empty(),
                "{operation}: {:?}",
                analysis.diagnostics
            );
            let program = analysis.hir.unwrap();
            let mut runtime = HistoricalRuntime::new(&program);
            let bar = |time| Bar {
                time,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            };
            runtime.append_bar(bar(0)).unwrap();
            let profile = runtime.profile();
            let map_slots = runtime.map_store.len();
            let source = runtime.map_store.get(&0).unwrap().clone();
            let mut runtime = runtime.with_resource_limits(crate::ResourceLimits {
                max_collection_bytes_per_bar: Some(0),
                ..crate::ResourceLimits::default()
            });
            let error = runtime.append_bar(bar(60_000)).unwrap_err();
            assert!(
                error.message.contains("E_RESOURCE_BUDGET"),
                "{operation}: {}",
                error.message
            );
            assert_eq!(runtime.map_store.get(&0).unwrap(), &source);
            assert!(runtime.map_store.get(&1).unwrap().entries.is_empty());
            assert_eq!(runtime.map_store.len(), map_slots);
            assert_eq!(runtime.profile().array_slots, profile.array_slots);
        }
    }

    #[test]
    fn short_lived_large_string_map_copies_collect_after_one_page_write() {
        let source = pine_syntax::SourceFile::new(
            "map-pressure.pine",
            r#"//@version=6
indicator("map pressure", max_bars_back=0)
var source = map.new<int,string>()
if barstate.isfirst
    for key = 0 to 511
        map.put(source, key, str.repeat("x", 40960))
temporary = map.copy(source)
map.put(temporary, 256, "short")
plot(str.length(map.get(source, 256)))
plot(str.length(map.get(temporary, 256)))
"#,
        );
        let analysis = pine_sema::analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let program = analysis.hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        for index in 0..8 {
            runtime
                .append_bar(Bar {
                    time: index * 60_000,
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 1.0,
                    volume: 1.0,
                })
                .unwrap();
            assert!(
                runtime.map_store.len() <= 3,
                "bar {index}: {}",
                runtime.map_store.len()
            );
        }
        assert!(runtime.next_map_id < 1024);
        let result = runtime.result();
        assert_eq!(result.plots[0].values, vec![PineValue::Int(40960); 8]);
        assert_eq!(result.plots[1].values, vec![PineValue::Int(5); 8]);
    }

    #[test]
    fn small_string_map_copies_record_the_deep_cloned_payload_once() {
        let source = pine_syntax::SourceFile::new(
            "small-map-pressure.pine",
            "//@version=6\nindicator(\"small pressure\")\nplot(close)\n",
        );
        let program = pine_sema::analyze_source(&source).hir.unwrap();
        let mut runtime = HistoricalRuntime::new(&program);
        let entries: MapEntries = (0..128)
            .map(|key| (PineValue::Int(key), PineValue::String("x".repeat(40960))))
            .collect::<Vec<_>>()
            .into();
        runtime.map_store.insert(
            0,
            MapStorage {
                key_kind: ArrayElementKind::Int,
                value_kind: ArrayElementKind::String,
                entries,
            },
        );
        runtime.next_map_id = 1;
        runtime
            .ta_state
            .call_state
            .insert(pine_ir::CallSiteId(0), PineValue::Map(0));
        runtime.collection_gc_allocated_bytes = 0;
        runtime.copy_map(0);
        let copied = 128 * (2 * std::mem::size_of::<PineValue>() + 40960);
        assert_eq!(runtime.collection_gc_allocated_bytes, copied);
        runtime.collect_temporary_collections();
        assert_eq!(runtime.map_store.len(), 1);
        assert_eq!(runtime.next_map_id, 2);
    }

    #[test]
    fn large_map_alias_copy_and_varip_follow_realtime_rollback() {
        let source = pine_syntax::SourceFile::new(
            "map.pine",
            r#"//@version=6
indicator("maps")
var plain = map.new<int,float>()
var alias = plain
if barstate.isfirst
    for key = 0 to 2047
        map.put(plain, key, key)
var saved = map.copy(plain)
varip sticky = map.copy(plain)
map.put(alias, 1024, map.get(alias, 1024) + close)
map.put(sticky, 1024, map.get(sticky, 1024) + close)
plot(map.get(plain, 1024))
plot(map.get(saved, 1024))
plot(map.get(sticky, 1024))
"#,
        );
        let analysis = pine_sema::analyze_source(&source);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let program = analysis.hir.unwrap();
        let bar = |time, close| Bar {
            time,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.,
        };
        let mut runtime = RealtimeRuntime::new(&program);
        runtime.seed_historical(&[bar(0, 1.)]).unwrap();
        for update in [
            BarUpdate::forming(bar(60000, 2.)),
            BarUpdate::forming(bar(60000, 3.)),
            BarUpdate::confirmed(bar(60000, 4.)),
        ] {
            runtime.apply_update(update).unwrap();
        }
        let observed: Vec<_> = runtime
            .result()
            .plots
            .iter()
            .map(|plot| plot.values.last().cloned().unwrap())
            .collect();
        assert_eq!(
            observed,
            vec![
                PineValue::Float(1029.),
                PineValue::Float(1024.),
                PineValue::Float(1034.)
            ]
        );
    }
}
