use std::collections::BTreeMap;
use std::fmt;

use crate::RuntimeError;

pub const SESSION_WINDOW_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWindowIds {
    pub window_id: String,
    pub trading_day_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionWindowInput {
    bars: BTreeMap<usize, SessionWindowIds>,
}

impl SessionWindowInput {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    #[must_use]
    pub fn ids_for(&self, bar_index: usize) -> Option<&SessionWindowIds> {
        self.bars.get(&bar_index)
    }

    pub fn validate_coverage(&self, chart_bar_count: usize) -> Result<(), SessionWindowInputError> {
        self.validate_range(0, chart_bar_count)
    }

    pub(crate) fn validate_range(
        &self,
        start: usize,
        end: usize,
    ) -> Result<(), SessionWindowInputError> {
        if self.is_empty() {
            return Ok(());
        }
        let mut expected = start;
        for (&bar_index, _) in self.bars.range(start..end) {
            if bar_index != expected {
                return Err(SessionWindowInputError::MissingBar {
                    bar_index: expected,
                });
            }
            expected += 1;
        }
        if expected != end {
            return Err(SessionWindowInputError::MissingBar {
                bar_index: expected,
            });
        }
        Ok(())
    }

    pub(crate) fn validate_replacement(
        &self,
        input: &Self,
        processed_bars: usize,
    ) -> Result<(), SessionWindowInputError> {
        for bar_index in 0..processed_bars {
            if self.ids_for(bar_index) != input.ids_for(bar_index) {
                return Err(SessionWindowInputError::HistoryChanged { bar_index });
            }
        }
        self.validate_extension(input, processed_bars)
    }

    pub(crate) fn validate_extension(
        &self,
        input: &Self,
        processed_bars: usize,
    ) -> Result<(), SessionWindowInputError> {
        if processed_bars > 0 && self.is_empty() && !input.is_empty() {
            return Err(SessionWindowInputError::HistoryChanged { bar_index: 0 });
        }
        for (&bar_index, ids) in input.bars.range(..processed_bars) {
            if self.ids_for(bar_index) != Some(ids) {
                return Err(SessionWindowInputError::HistoryChanged { bar_index });
            }
        }
        Ok(())
    }

    pub(crate) fn extend_validated(&mut self, input: Self) {
        self.bars.extend(input.bars);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionWindowInputError {
    UnsupportedSchemaVersion { version: u32 },
    DuplicateBar { bar_index: usize },
    EmptyId { bar_index: usize },
    MissingBar { bar_index: usize },
    HistoryChanged { bar_index: usize },
}

impl SessionWindowInputError {
    pub fn runtime_error(self) -> RuntimeError {
        RuntimeError {
            message: self.to_string(),
        }
    }
}

impl fmt::Display for SessionWindowInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { version } => write!(
                f,
                "E_SESSION_SCHEMA_VERSION: session window schemaVersion {version} is unsupported; expected {SESSION_WINDOW_SCHEMA_VERSION}"
            ),
            Self::DuplicateBar { bar_index } => write!(
                f,
                "E_SESSION_DUPLICATE_BAR: session window input repeats barIndex {bar_index}"
            ),
            Self::EmptyId { bar_index } => write!(
                f,
                "E_SESSION_EMPTY_ID: session window input has an empty windowId or tradingDayId at barIndex {bar_index}"
            ),
            Self::MissingBar { bar_index } => write!(
                f,
                "E_SESSION_COVERAGE: session window input is missing barIndex {bar_index}"
            ),
            Self::HistoryChanged { bar_index } => write!(
                f,
                "E_SESSION_HISTORY_CHANGED: session window input changes the executed context at barIndex {bar_index}"
            ),
        }
    }
}

pub fn session_window_input_from_bars(
    bars: Vec<(usize, SessionWindowIds)>,
) -> Result<SessionWindowInput, SessionWindowInputError> {
    let mut input = SessionWindowInput::new();
    for (bar_index, ids) in bars {
        if ids.window_id.is_empty() || ids.trading_day_id.is_empty() {
            return Err(SessionWindowInputError::EmptyId { bar_index });
        }
        if input.bars.contains_key(&bar_index) {
            return Err(SessionWindowInputError::DuplicateBar { bar_index });
        }
        input.bars.insert(bar_index, ids);
    }
    Ok(input)
}

pub fn session_window_input_from_v1(
    schema_version: u32,
    bars: Vec<(usize, SessionWindowIds)>,
) -> Result<SessionWindowInput, SessionWindowInputError> {
    if schema_version != SESSION_WINDOW_SCHEMA_VERSION {
        return Err(SessionWindowInputError::UnsupportedSchemaVersion {
            version: schema_version,
        });
    }
    session_window_input_from_bars(bars)
}

#[derive(Debug, serde::Deserialize)]
struct SessionWindowInputV1Json {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    bars: Vec<SessionWindowBarJson>,
}

#[derive(Debug, serde::Deserialize)]
struct SessionWindowBarJson {
    #[serde(rename = "barIndex")]
    bar_index: usize,
    #[serde(rename = "windowId")]
    window_id: String,
    #[serde(rename = "tradingDayId")]
    trading_day_id: String,
}

pub fn session_window_input_from_json(json: &str) -> Result<SessionWindowInput, String> {
    let parsed: SessionWindowInputV1Json = serde_json::from_str(json)
        .map_err(|err| format!("E_SESSION_MALFORMED: session window JSON is invalid: {err}"))?;
    let bars = parsed
        .bars
        .into_iter()
        .map(|bar| {
            (
                bar.bar_index,
                SessionWindowIds {
                    window_id: bar.window_id,
                    trading_day_id: bar.trading_day_id,
                },
            )
        })
        .collect();
    session_window_input_from_v1(parsed.schema_version, bars).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_schema_version() {
        let error = session_window_input_from_json(
            r#"{"schemaVersion":2,"bars":[{"barIndex":0,"windowId":"eth","tradingDayId":"d1"}]}"#,
        )
        .expect_err("schema");
        assert!(error.contains("E_SESSION_SCHEMA_VERSION"), "{error}");
    }

    #[test]
    fn rejects_duplicate_bar_index() {
        let error = session_window_input_from_bars(vec![
            (
                0,
                SessionWindowIds {
                    window_id: "eth".into(),
                    trading_day_id: "d1".into(),
                },
            ),
            (
                0,
                SessionWindowIds {
                    window_id: "rth".into(),
                    trading_day_id: "d1".into(),
                },
            ),
        ])
        .expect_err("duplicate");
        assert!(matches!(
            error,
            SessionWindowInputError::DuplicateBar { bar_index: 0 }
        ));
    }

    #[test]
    fn rejects_empty_ids() {
        let error = session_window_input_from_bars(vec![(
            1,
            SessionWindowIds {
                window_id: String::new(),
                trading_day_id: "d1".into(),
            },
        )])
        .expect_err("empty");
        assert!(matches!(
            error,
            SessionWindowInputError::EmptyId { bar_index: 1 }
        ));
    }

    #[test]
    fn empty_input_does_not_require_coverage() {
        SessionWindowInput::new()
            .validate_coverage(4)
            .expect("utc subset");
    }

    #[test]
    fn populated_input_requires_every_chart_bar() {
        let input = session_window_input_from_json(
            r#"{"schemaVersion":1,"bars":[{"barIndex":0,"windowId":"eth","tradingDayId":"d1"}]}"#,
        )
        .expect("parse");
        let error = input.validate_coverage(2).expect_err("missing bar 1");
        assert!(matches!(
            error,
            SessionWindowInputError::MissingBar { bar_index: 1 }
        ));
        input.validate_coverage(1).expect("complete");
    }

    #[test]
    fn coverage_range_only_requires_the_new_interval() {
        let input = session_window_input_from_bars(vec![(
            100_000,
            SessionWindowIds {
                window_id: "day".into(),
                trading_day_id: "day".into(),
            },
        )])
        .expect("input");
        input
            .validate_range(100_000, 100_001)
            .expect("new interval");
        assert_eq!(
            input.validate_range(100_000, 100_002),
            Err(SessionWindowInputError::MissingBar { bar_index: 100_001 })
        );
    }

    #[test]
    fn malformed_json_is_session_malformed() {
        let error = session_window_input_from_json("{").expect_err("malformed");
        assert!(error.starts_with("E_SESSION_MALFORMED"), "{error}");
    }
}
