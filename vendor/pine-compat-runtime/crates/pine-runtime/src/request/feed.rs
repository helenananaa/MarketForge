use std::collections::HashMap;
use std::fmt;

use crate::{Bar, BarUpdate, BarUpdateKind, RuntimeError};

use super::RequestKey;
use crate::runtime::append_history::AppendHistory;

#[derive(Debug, Clone, Default)]
pub(crate) struct RequestFeed {
    streams: HashMap<RequestKey, RequestStream>,
}

#[derive(Debug, Clone, Default)]
struct RequestStream {
    confirmed: AppendHistory<Bar>,
    forming: Option<Bar>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestFeedError {
    DuplicateBars { time: i64 },
    FormingMismatch { expected: i64, time: i64 },
    FormingOpen { time: i64 },
    Stale { time: i64, last_confirmed: i64 },
}

impl RequestFeed {
    pub(crate) fn contains(&self, key: &RequestKey) -> bool {
        self.streams
            .get(key)
            .is_some_and(|stream| !stream.confirmed.is_empty() || stream.forming.is_some())
    }

    pub(crate) fn has_forming(&self, key: &RequestKey) -> bool {
        self.streams
            .get(key)
            .is_some_and(|stream| stream.forming.is_some())
    }

    pub(crate) fn last_update_time(&self, key: &RequestKey) -> Option<i64> {
        self.streams.get(key).and_then(|stream| {
            stream
                .forming
                .or_else(|| stream.confirmed.last().copied())
                .map(|bar| bar.time)
        })
    }

    pub(crate) fn resolved_len(
        &self,
        key: &RequestKey,
        provider_len: usize,
        include_forming: bool,
    ) -> usize {
        provider_len
            + self.streams.get(key).map_or(0, |stream| {
                stream.confirmed.len() + usize::from(include_forming && stream.forming.is_some())
            })
    }

    pub(crate) fn resolved_from(
        &self,
        key: &RequestKey,
        provider: &[Bar],
        include_forming: bool,
        start: usize,
    ) -> Vec<Bar> {
        let mut bars = provider.get(start..).unwrap_or(&[]).to_vec();
        if let Some(stream) = self.streams.get(key) {
            bars.extend(stream.confirmed.tail(start.saturating_sub(provider.len())));
            if include_forming
                && start <= provider.len() + stream.confirmed.len()
                && let Some(forming) = stream.forming
            {
                bars.push(forming);
            }
        }
        bars
    }

    /// Keep request bars closed within the retained chart boundary. The last
    /// retained bar must close nominally; earlier bars may close at a retained
    /// successor's open. Unclosed forming requests remain for the next observation.
    pub(crate) fn trim_after(&mut self, chart_close: Option<i64>) {
        let Some(chart_close) = chart_close else {
            self.streams.clear();
            return;
        };
        self.streams.retain(|key, stream| {
            let timeframe = key.timeframe();
            let close_at = |index: usize| {
                let nominal = timeframe.nominal_close(stream.confirmed[index].time);
                stream
                    .confirmed
                    .get(index + 1)
                    .map_or(nominal, |next| nominal.min(next.time))
            };
            let len = stream.confirmed.len();
            if len > 0 && close_at(len - 1) > chart_close {
                let (mut lo, mut hi) = (0, len);
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    if close_at(mid) <= chart_close {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                // Removing a successor can extend the last retained close.
                // Prune that now-unclosed suffix before mutating the history.
                // Only successor-based closes are searched: calendar fallback
                // at extreme timestamps need not have monotone nominal closes.
                while lo > 0 && timeframe.nominal_close(stream.confirmed[lo - 1].time) > chart_close
                {
                    lo -= 1;
                }
                stream.confirmed.truncate(lo);
            }
            if stream
                .forming
                .is_some_and(|bar| timeframe.nominal_close(bar.time) <= chart_close)
            {
                stream.forming = None;
            }
            !stream.confirmed.is_empty() || stream.forming.is_some()
        });
    }

    pub(crate) fn apply(
        &mut self,
        key: RequestKey,
        update: BarUpdate,
        provider_last: Option<i64>,
    ) -> Result<(), RequestFeedError> {
        let last_confirmed = self
            .streams
            .get(&key)
            .and_then(|stream| stream.confirmed.last().map(|bar| bar.time))
            .or(provider_last);
        let stream = self.streams.entry(key).or_default();
        match update.kind {
            BarUpdateKind::Historical => {
                if stream.forming.is_some() {
                    return Err(RequestFeedError::FormingOpen {
                        time: update.bar.time,
                    });
                }
                append_confirmed(&mut stream.confirmed, update.bar, last_confirmed)
            }
            BarUpdateKind::Forming => apply_forming(stream, update.bar, last_confirmed),
            BarUpdateKind::Confirmed => apply_confirmed(stream, update.bar, last_confirmed),
        }
    }
}

fn append_confirmed(
    confirmed: &mut AppendHistory<Bar>,
    bar: Bar,
    last_confirmed: Option<i64>,
) -> Result<(), RequestFeedError> {
    if let Some(last) = last_confirmed {
        if bar.time == last {
            return Err(RequestFeedError::DuplicateBars { time: bar.time });
        }
        if bar.time < last {
            return Err(RequestFeedError::Stale {
                time: bar.time,
                last_confirmed: last,
            });
        }
    }
    confirmed.push(bar);
    Ok(())
}

fn apply_forming(
    stream: &mut RequestStream,
    bar: Bar,
    last_confirmed: Option<i64>,
) -> Result<(), RequestFeedError> {
    if let Some(last) = last_confirmed
        && bar.time <= last
    {
        return Err(RequestFeedError::Stale {
            time: bar.time,
            last_confirmed: last,
        });
    }
    if let Some(forming) = stream.forming
        && forming.time != bar.time
    {
        return Err(RequestFeedError::FormingMismatch {
            expected: forming.time,
            time: bar.time,
        });
    }
    stream.forming = Some(bar);
    Ok(())
}

fn apply_confirmed(
    stream: &mut RequestStream,
    bar: Bar,
    last_confirmed: Option<i64>,
) -> Result<(), RequestFeedError> {
    if let Some(forming) = stream.forming {
        if forming.time != bar.time {
            return Err(RequestFeedError::FormingMismatch {
                expected: forming.time,
                time: bar.time,
            });
        }
        stream.forming = None;
        return append_confirmed(&mut stream.confirmed, bar, last_confirmed);
    }
    append_confirmed(&mut stream.confirmed, bar, last_confirmed)
}

impl RequestFeedError {
    pub(crate) fn runtime_error(self) -> RuntimeError {
        RuntimeError {
            message: self.to_string(),
        }
    }
}

impl fmt::Display for RequestFeedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateBars { time } => {
                write!(
                    formatter,
                    "E_REQUEST_FEED_TIME: duplicate requested bar time `{time}`"
                )
            }
            Self::FormingMismatch { expected, time } => write!(
                formatter,
                "E_REQUEST_FEED_FORMING: requested bar `{time}` does not match forming time `{expected}`"
            ),
            Self::FormingOpen { time } => write!(
                formatter,
                "E_REQUEST_FEED_FORMING: cannot append historical requested bar `{time}` while a forming request bar is open"
            ),
            Self::Stale {
                time,
                last_confirmed,
            } => write!(
                formatter,
                "E_REQUEST_FEED_TIME: requested bar `{time}` must be later than confirmed time `{last_confirmed}`"
            ),
        }
    }
}

#[cfg(test)]
mod trim_tests {
    use super::*;
    use crate::RequestTimeframe;

    fn bar(time: i64) -> Bar {
        Bar {
            time,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        }
    }

    #[test]
    fn trimming_preserves_unclosed_forming_requests_and_removes_closed_ones() {
        let ltf = RequestKey::new("L", RequestTimeframe::parse("1").unwrap());
        let htf = RequestKey::new("H", RequestTimeframe::parse("5").unwrap());
        let mut feed = RequestFeed::default();
        feed.apply(ltf.clone(), BarUpdate::forming(bar(240_000)), None)
            .unwrap();
        feed.apply(htf.clone(), BarUpdate::forming(bar(300_000)), None)
            .unwrap();
        let checkpoint = feed.clone();
        feed.trim_after(Some(420_000));
        assert!(!feed.contains(&ltf));
        assert!(feed.has_forming(&htf));
        assert!(checkpoint.has_forming(&ltf));
        assert!(checkpoint.has_forming(&htf));
        feed.trim_after(None);
        assert!(feed.streams.is_empty());
    }

    #[test]
    fn discarded_early_successor_cannot_shorten_a_retained_request_close() {
        let key = RequestKey::new("H", RequestTimeframe::parse("5").unwrap());
        let mut feed = RequestFeed::default();
        for time in [-300_000, 0, 60_000, 120_000] {
            feed.apply(key.clone(), BarUpdate::confirmed(bar(time)), None)
                .unwrap();
        }
        let checkpoint = feed.clone();
        feed.trim_after(Some(180_000));
        let retained = feed.resolved_from(&key, &[], false, 0);
        assert_eq!(retained, vec![bar(-300_000)]);
        feed.trim_after(Some(180_000));
        assert_eq!(feed.resolved_from(&key, &[], false, 0), retained);
        assert_eq!(checkpoint.resolved_len(&key, 0, false), 4);
    }

    #[test]
    fn extreme_calendar_fallback_keeps_closes_bounded_by_retained_successors() {
        let timeframe = RequestTimeframe::parse("2M").unwrap();
        let key = RequestKey::new("H", timeframe.clone());
        let earliest_calendar = chrono::DateTime::<chrono::Utc>::MIN_UTC.timestamp_millis();
        let chart_close = timeframe.nominal_close(earliest_calendar);
        assert!(timeframe.nominal_close(earliest_calendar - 1) > chart_close);
        let mut feed = RequestFeed::default();
        for time in [earliest_calendar - 1, earliest_calendar, chart_close] {
            feed.apply(key.clone(), BarUpdate::confirmed(bar(time)), None)
                .unwrap();
        }
        feed.trim_after(Some(chart_close));
        let retained = vec![bar(earliest_calendar - 1), bar(earliest_calendar)];
        assert_eq!(feed.resolved_from(&key, &[], false, 0), retained);
        feed.trim_after(Some(chart_close));
        assert_eq!(feed.resolved_from(&key, &[], false, 0), retained);
    }
}
