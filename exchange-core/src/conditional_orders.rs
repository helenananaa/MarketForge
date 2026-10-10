//! Exchange-owned conditional entries; each armed condition submits once.
use serde::{Deserialize, Serialize};

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
