#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct InternalOrderKey(pub(super) u64);

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum OcaType {
    None,
    Cancel,
    Reduce,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct OcaGroupKey {
    pub(super) name: String,
    pub(super) oca_type: OcaType,
}

impl OcaGroupKey {
    #[allow(dead_code)]
    pub(super) fn new(name: impl Into<String>, oca_type: OcaType) -> Self {
        Self {
            name: name.into(),
            oca_type,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct OcaPeerEffects {
    pub(super) cancelled: Vec<InternalOrderKey>,
    pub(super) reduced: std::collections::HashMap<InternalOrderKey, f64>,
    pub(super) reduce_taken: Vec<InternalOrderKey>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum OcaMember {
    Order(InternalOrderKey),
    Exit {
        id: String,
        from_entry: String,
        target_trade_key: Option<u64>,
    },
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StrategyCommandOrigin {
    Entry,
    Order,
    Exit,
    Close,
    CloseAll,
    MarginCall,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StrategyOrderMetadata {
    pub(crate) comment: Option<String>,
    pub(crate) alert_message: Option<String>,
    pub(crate) disable_alert: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StrategyExitMetadata {
    pub(crate) comment: Option<String>,
    pub(crate) comment_profit: Option<String>,
    pub(crate) comment_loss: Option<String>,
    pub(crate) comment_trailing: Option<String>,
    pub(crate) alert_message: Option<String>,
    pub(crate) alert_profit: Option<String>,
    pub(crate) alert_loss: Option<String>,
    pub(crate) alert_trailing: Option<String>,
    pub(crate) disable_alert: bool,
}

pub(crate) type StrategyOrderFillAlertEvent = crate::StrategyOrderFillAlertOutput;

pub(super) struct EntryFill {
    pub(super) id: String,
    pub(super) bar_index: usize,
    pub(super) time: i64,
    pub(super) price: f64,
    pub(super) apply_slippage: bool,
    pub(super) qty: f64,
    pub(super) metadata: StrategyOrderMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EntryPyramidingMode {
    EnforceLimit,
    #[allow(dead_code)]
    BypassLimit,
    SameTickPriceException,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ClosedTradeMetrics {
    pub(super) commission: f64,
    pub(super) profit_percent: f64,
    pub(super) max_runup: f64,
    pub(super) max_drawdown: f64,
    pub(super) entry_comment: Option<String>,
    pub(super) exit_comment: Option<String>,
    pub(super) close_metadata: StrategyOrderMetadata,
}
