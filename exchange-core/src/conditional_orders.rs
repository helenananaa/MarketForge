//! Exchange-owned conditional entries; each armed condition submits once.
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::shared_map::SharedMap;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalOrderSpec {
    pub side: crate::Side,
    #[serde(default)]
    pub position_side: crate::PositionSide,
    pub qty: u64,
    pub trigger_price_tick: i64,
    pub above: bool,
    #[serde(default)]
    pub trigger: crate::ProtectionTrigger,
    #[serde(default)]
    pub limit_price_tick: Option<i64>,
    #[serde(default)]
    pub protection: Option<crate::PositionProtectionSpec>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConditionalOrder {
    pub key: String,
    pub instrument_id: String,
    pub account_id: u64,
    pub spec: ConditionalOrderSpec,
    pub status: String,
    pub last_checked_event_seq: u64,
    pub submitted_order_id: Option<u64>,
}

/// Armed entries that trigger on the same price of one market, in one direction.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ArmedGroup {
    pub(crate) instrument_id: String,
    /// The trigger is the last trade (else the mark).
    pub(crate) last: bool,
    pub(crate) above: bool,
}

/// The conditional orders of an exchange, by key, and what the pass after
/// each order needs to visit only the entries that can change.
///
/// The pass moves every armed entry's event cursor to its market's latest
/// event. Rather than rewriting each entry, it records that event as the
/// market's mark: an armed entry's cursor is the greater of its stored cursor
/// and its market's mark (`cursor`). Reads and writes see that cursor; an
/// entry's stored copy only lags while it is armed. Armed entries are indexed
/// by market, trigger, direction and trigger price, so the pass finds crossed
/// entries by range. An entry armed after its market's mark (fresh) has a
/// later cursor than the others there and is checked on its own.
#[derive(Clone, Debug, Default)]
pub(crate) struct ConditionalOrders {
    entries: SharedMap<String, ConditionalOrder>,
    marks: SharedMap<String, u64>,
    armed: SharedMap<ArmedGroup, im::OrdSet<(i64, String)>>,
    fresh: SharedMap<String, im::OrdSet<String>>,
}

impl ConditionalOrders {
    fn group(entry: &ConditionalOrder) -> ArmedGroup {
        ArmedGroup {
            instrument_id: entry.instrument_id.clone(),
            last: entry.spec.trigger == crate::ProtectionTrigger::Last,
            above: entry.spec.above,
        }
    }

    pub(crate) fn mark(&self, instrument: &str) -> u64 {
        self.marks.get(instrument).copied().unwrap_or(0)
    }

    /// The entry's event cursor.
    pub(crate) fn cursor(&self, entry: &ConditionalOrder) -> u64 {
        if entry.status == "armed" {
            entry.last_checked_event_seq.max(self.mark(&entry.instrument_id))
        } else {
            entry.last_checked_event_seq
        }
    }

    /// A copy of the entry with its cursor.
    pub(crate) fn exact(&self, entry: &ConditionalOrder) -> ConditionalOrder {
        let mut entry = entry.clone();
        entry.last_checked_event_seq = self.cursor(&entry);
        entry
    }

    /// The stored entry: its `last_checked_event_seq` can lag (see `cursor`).
    pub(crate) fn get(&self, key: &str) -> Option<&ConditionalOrder> {
        self.entries.get(key)
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Stored entries in key order (see `get`).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &ConditionalOrder)> {
        self.entries.iter()
    }

    pub(crate) fn has_armed(&self) -> bool {
        !self.armed.is_empty()
    }

    /// Unchanged since `previous`, as the entries read: no entry and no mark
    /// was written. A pass that only moves cursors writes a mark.
    pub(crate) fn same(&self, previous: &Self) -> bool {
        self.entries.ptr_eq(&previous.entries) && self.marks.ptr_eq(&previous.marks)
    }

    /// Markets with an armed entry, in order.
    pub(crate) fn armed_markets(&self) -> Vec<&str> {
        let mut markets = Vec::<&str>::new();
        for group in self.armed.keys() {
            if markets.last() != Some(&group.instrument_id.as_str()) {
                markets.push(&group.instrument_id);
            }
        }
        markets
    }

    /// The armed groups of a market, with their entries by trigger price.
    pub(crate) fn armed_groups<'a>(
        &'a self,
        instrument: &'a str,
    ) -> impl Iterator<Item = (&'a ArmedGroup, &'a im::OrdSet<(i64, String)>)> + 'a {
        self.armed
            .iter()
            .skip_while(move |(group, _)| group.instrument_id.as_str() < instrument)
            .take_while(move |(group, _)| group.instrument_id == instrument)
    }

    /// Keys of the market's fresh entries (armed after its mark).
    pub(crate) fn fresh(&self, instrument: &str) -> Option<&im::OrdSet<String>> {
        self.fresh.get(instrument)
    }

    /// Advances every armed entry of the market to `seq`, its latest event:
    /// none is then fresh.
    pub(crate) fn set_mark(&mut self, instrument: &str, seq: u64) {
        self.marks.insert(instrument.to_string(), seq);
        self.fresh.remove(instrument);
    }

    fn unindex(&mut self, key: &str) {
        let Some(entry) = self.entries.get(key).filter(|e| e.status == "armed") else {
            return;
        };
        let (group, price) = (Self::group(entry), entry.spec.trigger_price_tick);
        if let Some(set) = self.armed.get_mut(&group) {
            set.remove(&(price, key.to_string()));
            if set.is_empty() {
                self.armed.remove(&group);
            }
        }
        if let Some(set) = self.fresh.get_mut(&group.instrument_id) {
            set.remove(key);
            if set.is_empty() {
                self.fresh.remove(&group.instrument_id);
            }
        }
    }

    fn index(&mut self, key: &str, entry: &ConditionalOrder) {
        if entry.status != "armed" {
            return;
        }
        self.armed
            .entry(Self::group(entry))
            .or_default()
            .insert((entry.spec.trigger_price_tick, key.to_string()));
        if entry.last_checked_event_seq > self.mark(&entry.instrument_id) {
            self.fresh
                .entry(entry.instrument_id.clone())
                .or_default()
                .insert(key.to_string());
        }
    }

    /// Writes the entry with the cursor it holds.
    pub(crate) fn insert(&mut self, key: String, entry: ConditionalOrder) {
        self.unindex(&key);
        self.index(&key, &entry);
        self.entries.insert(key, entry);
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<ConditionalOrder> {
        self.unindex(key);
        self.entries.remove(key)
    }

    /// Changes an entry, given with its cursor; false when there is none.
    pub(crate) fn update(&mut self, key: &str, change: impl FnOnce(&mut ConditionalOrder)) -> bool {
        let Some(previous) = self.entries.get(key) else {
            return false;
        };
        let mut entry = self.exact(previous);
        change(&mut entry);
        self.insert(key.to_string(), entry);
        true
    }
}

/// The wire form is the plain key-ordered map, every entry with its cursor.
impl Serialize for ConditionalOrders {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, entry) in self.entries.iter() {
            if self.cursor(entry) == entry.last_checked_event_seq {
                map.serialize_entry(key, entry)?;
            } else {
                map.serialize_entry(key, &self.exact(entry))?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ConditionalOrders {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = std::collections::BTreeMap::<String, ConditionalOrder>::deserialize(deserializer)?;
        let mut orders = Self::default();
        for (key, entry) in entries {
            orders.insert(key, entry);
        }
        Ok(orders)
    }
}
