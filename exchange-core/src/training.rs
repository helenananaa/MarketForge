use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    agents::AgentTemplate,
    model::{AccountId, BookSnapshot, OrderId, PriceTick, Qty, Side},
    scenario::ScenarioConfig,
};

pub const TRAINING_SPEC_VERSION: u16 = 1;
pub const SCORING_RULE_VERSION: u16 = 1;
pub const LOW_SLIPPAGE_BUY_TASK_VERSION: u16 = 1;
pub const SCENARIO_INJECTION_DISCLAIMER: &str = "Injected training scenarios and background agents are simulated facts; they are not evidence of live-market manipulation.";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TrainingStatus {
    Created,
    Running,
    Completed,
    Failed,
    Aborted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrainingSpec {
    pub spec_version: u16,
    pub scoring_version: u16,
    pub task_version: u16,
    pub run_id: String,
    pub room_id: String,
    pub scenario: ScenarioConfig,
    pub agents: Vec<AgentTemplate>,
    pub trainee_account_id: AccountId,
    pub target_qty: Qty,
    pub horizon_steps: u64,
    pub reference_price_tick: PriceTick,
    pub buy_only: bool,
}

impl TrainingSpec {
    pub fn low_slippage_buy(
        run_id: impl Into<String>,
        scenario: ScenarioConfig,
        agents: Vec<AgentTemplate>,
        trainee_account_id: AccountId,
        target_qty: Qty,
        horizon_steps: u64,
        reference_price_tick: PriceTick,
    ) -> Result<Self, TrainingError> {
        if target_qty == 0 || horizon_steps == 0 || reference_price_tick <= 0 {
            return Err(TrainingError::InvalidSpec);
        }
        let room_id = scenario.room_id.clone();
        Ok(Self {
            spec_version: TRAINING_SPEC_VERSION,
            scoring_version: SCORING_RULE_VERSION,
            task_version: LOW_SLIPPAGE_BUY_TASK_VERSION,
            run_id: run_id.into(),
            room_id,
            scenario,
            agents,
            trainee_account_id,
            target_qty,
            horizon_steps,
            reference_price_tick,
            buy_only: true,
        })
    }

    pub fn digest(&self) -> String {
        format!(
            "v{}/{}/{}/{}:Q{}:T{}:P{}",
            self.spec_version,
            self.task_version,
            self.scoring_version,
            self.run_id,
            self.target_qty,
            self.horizon_steps,
            self.reference_price_tick
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrainingFill {
    pub price_tick: PriceTick,
    pub qty: Qty,
    #[serde(default)]
    pub fee: i128,
    #[serde(default)]
    pub order_id: Option<OrderId>,
    #[serde(default)]
    pub command_seq: Option<u64>,
    #[serde(default)]
    pub book_before: Option<BookSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrainingRun {
    pub spec: TrainingSpec,
    pub status: TrainingStatus,
    pub paused: bool,
    pub steps_elapsed: u64,
    pub filled_qty: Qty,
    pub fills: Vec<TrainingFill>,
    pub fees_paid: i128,
    pub rejects: u64,
    pub cancels: u64,
    pub open_buy_qty: Qty,
}

impl TrainingRun {
    pub fn new(spec: TrainingSpec) -> Self {
        Self {
            spec,
            status: TrainingStatus::Created,
            paused: false,
            steps_elapsed: 0,
            filled_qty: 0,
            fills: Vec::new(),
            fees_paid: 0,
            rejects: 0,
            cancels: 0,
            open_buy_qty: 0,
        }
    }

    pub fn remaining_qty(&self) -> Qty {
        self.spec.target_qty.saturating_sub(self.filled_qty)
    }

    pub fn remaining_buy_capacity(&self) -> Qty {
        self.remaining_qty().saturating_sub(self.open_buy_qty)
    }

    pub fn start(&mut self) -> Result<(), TrainingError> {
        match self.status {
            TrainingStatus::Created => {
                self.status = TrainingStatus::Running;
                Ok(())
            }
            TrainingStatus::Running => Ok(()),
            TrainingStatus::Completed | TrainingStatus::Failed | TrainingStatus::Aborted => {
                Err(TrainingError::AlreadyFinished)
            }
        }
    }

    pub fn abort(&mut self) -> Result<(), TrainingError> {
        match self.status {
            TrainingStatus::Created | TrainingStatus::Running => {
                self.status = TrainingStatus::Aborted;
                self.paused = false;
                Ok(())
            }
            TrainingStatus::Aborted => Ok(()),
            TrainingStatus::Completed | TrainingStatus::Failed => {
                Err(TrainingError::AlreadyFinished)
            }
        }
    }

    pub fn set_paused(&mut self, paused: bool) -> Result<(), TrainingError> {
        if self.status != TrainingStatus::Running && paused {
            return Err(TrainingError::NotRunning);
        }
        if self.status == TrainingStatus::Running {
            self.paused = paused;
        }
        Ok(())
    }

    pub fn record_fill(&mut self, price_tick: PriceTick, qty: Qty, fee: i128) {
        self.record_fill_evidence(TrainingFill {
            price_tick,
            qty,
            fee,
            order_id: None,
            command_seq: None,
            book_before: None,
        });
    }

    pub fn record_fill_evidence(&mut self, fill: TrainingFill) {
        if self.status != TrainingStatus::Running || fill.qty == 0 {
            return;
        }
        self.filled_qty = self.filled_qty.saturating_add(fill.qty);
        self.fees_paid += fill.fee;
        self.open_buy_qty = self.open_buy_qty.saturating_sub(fill.qty);
        self.fills.push(fill);
        if self.filled_qty >= self.spec.target_qty {
            self.status = TrainingStatus::Completed;
            self.paused = false;
        }
    }

    pub fn record_open_buy(&mut self, qty: Qty) {
        self.open_buy_qty = self.open_buy_qty.saturating_add(qty);
    }

    pub fn record_cancel(&mut self, qty: Qty) {
        self.cancels = self.cancels.saturating_add(1);
        self.open_buy_qty = self.open_buy_qty.saturating_sub(qty);
    }

    pub fn record_reject(&mut self) {
        self.rejects = self.rejects.saturating_add(1);
    }

    pub fn on_step(&mut self) {
        if self.status != TrainingStatus::Running || self.paused {
            return;
        }
        self.steps_elapsed = self.steps_elapsed.saturating_add(1);
        if self.filled_qty >= self.spec.target_qty || self.steps_elapsed >= self.spec.horizon_steps
        {
            self.status = TrainingStatus::Completed;
        }
    }

    pub fn allows_trainee_action(&self, account_id: AccountId, side: Option<Side>) -> bool {
        if account_id != self.spec.trainee_account_id {
            return true;
        }
        if self.status != TrainingStatus::Running || self.paused {
            return false;
        }
        if self.spec.buy_only && side == Some(Side::Sell) {
            return false;
        }
        true
    }

    pub fn allows_trainee_transfer(&self, account_id: AccountId) -> bool {
        account_id != self.spec.trainee_account_id || matches!(self.status, TrainingStatus::Created)
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            TrainingStatus::Completed | TrainingStatus::Failed | TrainingStatus::Aborted
        )
    }

    pub fn score(&self) -> TrainingScore {
        score_run(self)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrainingScore {
    pub scoring_version: u16,
    pub q: Qty,
    pub target_qty: Qty,
    pub completion_ppm: u64,
    pub vwap_tick_num: Option<i128>,
    pub vwap_tick_den: Option<i128>,
    pub buy_slippage_bp: Option<i128>,
    pub fees_paid: i128,
    pub steps_elapsed: u64,
    pub rejects: u64,
    pub cancels: u64,
    pub remaining_qty: Qty,
    pub finished: bool,
    pub incomplete: bool,
    pub incomplete_penalty_ppm: u64,
}

pub fn score_run(run: &TrainingRun) -> TrainingScore {
    let q = run.filled_qty;
    let target = run.spec.target_qty.max(1);
    let completion_ppm = (u128::from(q) * 1_000_000 / u128::from(target)) as u64;
    let quote: i128 = run
        .fills
        .iter()
        .map(|fill| i128::from(fill.price_tick) * i128::from(fill.qty))
        .sum();
    let (vwap_tick_num, vwap_tick_den) = if q == 0 {
        (None, None)
    } else {
        (Some(quote), Some(i128::from(q)))
    };
    let buy_slippage_bp = match (vwap_tick_num, vwap_tick_den) {
        (Some(num), Some(den)) if den > 0 && run.spec.reference_price_tick > 0 => Some(
            10_000 * (num - i128::from(run.spec.reference_price_tick) * den)
                / (i128::from(run.spec.reference_price_tick) * den),
        ),
        _ => None,
    };
    let finished = matches!(
        run.status,
        TrainingStatus::Completed | TrainingStatus::Failed | TrainingStatus::Aborted
    );
    let incomplete = q < run.spec.target_qty;
    let incomplete_penalty_ppm = if finished && incomplete { 1_000_000 } else { 0 };
    TrainingScore {
        scoring_version: SCORING_RULE_VERSION,
        q,
        target_qty: run.spec.target_qty,
        completion_ppm,
        vwap_tick_num,
        vwap_tick_den,
        buy_slippage_bp,
        fees_paid: run.fees_paid,
        steps_elapsed: run.steps_elapsed,
        rejects: run.rejects,
        cancels: run.cancels,
        remaining_qty: run.remaining_qty(),
        finished,
        incomplete,
        incomplete_penalty_ppm,
    }
}

pub fn training_report_json(run: &TrainingRun) -> serde_json::Value {
    let score = run.score();
    let injected_agent_ids = run
        .spec
        .agents
        .iter()
        .map(AgentTemplate::participant_id)
        .collect::<Vec<_>>();
    json!({
        "api_version": "report.v1",
        "spec_digest": run.spec.digest(),
        "facts": {
            "q": score.q,
            "target_qty": score.target_qty,
            "fills": run.fills,
            "fees_paid": score.fees_paid,
            "reference_price_tick": run.spec.reference_price_tick,
            "steps_elapsed": score.steps_elapsed,
            "trainee_account_id": run.spec.trainee_account_id,
            "injected_agent_ids": injected_agent_ids,
        },
        "metrics": score,
        "inferences": [
            "Incomplete runs receive incomplete_penalty_ppm; do not treat lower fill count as higher execution quality.",
            SCENARIO_INJECTION_DISCLAIMER,
        ],
    })
}

pub fn training_report_markdown(run: &TrainingRun) -> String {
    let score = run.score();
    format!(
        "# Training report {}\n\n- status: {:?}\n- q/Q: {}/{}\n- VWAP: {:?}/{:?}\n- buy slippage bp: {:?}\n- fees: {}\n- incomplete: {} (penalty {})\n\n{}\n\nFacts are fills, fees, order ids, execution seq, and the visible book before each trainee order. Slippage is computed from VWAP vs P0={}.\n",
        run.spec.run_id,
        run.status,
        score.q,
        score.target_qty,
        score.vwap_tick_num,
        score.vwap_tick_den,
        score.buy_slippage_bp,
        score.fees_paid,
        score.incomplete,
        score.incomplete_penalty_ppm,
        run.fills
            .iter()
            .map(|fill| {
                format!(
                    "- fill px={} qty={} fee={} order_id={} seq={}",
                    fill.price_tick,
                    fill.qty,
                    fill.fee,
                    fill.order_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    fill.command_seq
                        .map(|seq| seq.to_string())
                        .unwrap_or_else(|| "-".to_string())
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        run.spec.reference_price_tick
    ) + SCENARIO_INJECTION_DISCLAIMER
        + "\n"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrainingError {
    InvalidSpec,
    AlreadyFinished,
    NotRunning,
    MissingTwoSidedBook,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpotRiskConfig,
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        scenario::ScenarioAccount,
        spot::SpotClearingConfig,
    };

    fn spec() -> TrainingSpec {
        let scenario = ScenarioConfig {
            room_id: "train-1".to_string(),
            market_events: Vec::new(),
            venue_preset: None,
            venue_rules: crate::VenueRuleConfig::default(),
            venue_asset_policy: crate::VenueAssetPolicyConfig::default(),
            assets: Vec::new(),
            market: MarketConfig::Spot(SpotMarketConfig {
                instrument: InstrumentConfig::new("V-BTC-SPOT", 1, 1).unwrap(),
                clearing: SpotClearingConfig::default(),
                risk: SpotRiskConfig::default(),
            }),
            extra_markets: Vec::new(),
            initial_portfolios: Vec::new(),
            initial_allocations: Vec::new(),
            routed_initial_allocations: Vec::new(),
            accounts: vec![ScenarioAccount::Basic {
                account_id: 20,
                cash_balance: 10_000,
            }],
            seed_orders: vec![],
            routed_seed_orders: Vec::new(),
        };
        TrainingSpec::low_slippage_buy("run-1", scenario, Vec::new(), 20, 10, 5, 100).unwrap()
    }

    #[test]
    fn hand_computed_metrics_match_fixture() {
        let mut run = TrainingRun::new(spec());
        run.start().unwrap();
        run.record_fill(101, 4, 0);
        run.record_fill(102, 6, 2);
        let score = run.score();
        assert_eq!(score.q, 10);
        assert_eq!(score.completion_ppm, 1_000_000);
        assert_eq!(score.vwap_tick_num, Some(1016));
        assert_eq!(score.vwap_tick_den, Some(10));
        assert_eq!(score.buy_slippage_bp, Some(160));
        assert_eq!(score.fees_paid, 2);
        assert!(!score.incomplete);
        assert_eq!(score.incomplete_penalty_ppm, 0);
        assert!(score.finished);
    }

    #[test]
    fn incomplete_run_is_marked_and_penalized() {
        let mut run = TrainingRun::new(spec());
        run.start().unwrap();
        run.record_fill(99, 3, 0);
        for _ in 0..5 {
            run.on_step();
        }
        let score = run.score();
        assert_eq!(score.q, 3);
        assert!(score.incomplete);
        assert_eq!(score.incomplete_penalty_ppm, 1_000_000);
        assert_eq!(score.buy_slippage_bp, Some(-100));
        assert!(run.abort().is_err());
        assert_eq!(run.start(), Err(TrainingError::AlreadyFinished));
    }

    #[test]
    fn remaining_capacity_prevents_overbuy() {
        let mut run = TrainingRun::new(spec());
        run.start().unwrap();
        run.record_open_buy(7);
        assert_eq!(run.remaining_buy_capacity(), 3);
        run.record_fill(100, 7, 0);
        assert_eq!(run.remaining_buy_capacity(), 3);
        assert!(!run.allows_trainee_action(20, Some(Side::Sell)));
        assert!(run.allows_trainee_action(20, Some(Side::Buy)));
        assert!(!run.allows_trainee_transfer(20));
        assert!(run.allows_trainee_transfer(10));
    }

    #[test]
    fn report_binds_fill_evidence() {
        let mut run = TrainingRun::new(spec());
        run.start().unwrap();
        run.record_fill_evidence(TrainingFill {
            price_tick: 101,
            qty: 4,
            fee: 1,
            order_id: Some(9),
            command_seq: Some(3),
            book_before: Some(BookSnapshot {
                bids: vec![],
                asks: vec![crate::model::BookLevel {
                    price_tick: 101,
                    qty: 8,
                }],
            }),
        });
        let json = training_report_json(&run);
        assert_eq!(json["facts"]["fills"][0]["order_id"], 9);
        assert_eq!(json["facts"]["fills"][0]["command_seq"], 3);
        assert_eq!(json["facts"]["fills"][0]["fee"], 1);
        assert_eq!(
            json["facts"]["fills"][0]["book_before"]["asks"][0]["price_tick"],
            101
        );
        let markdown = training_report_markdown(&run);
        assert!(markdown.contains("order_id=9"));
        assert!(markdown.contains("seq=3"));
        assert!(markdown.contains(SCENARIO_INJECTION_DISCLAIMER));
        assert!(
            json["inferences"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str() == Some(SCENARIO_INJECTION_DISCLAIMER))
        );
        assert_eq!(json["facts"]["injected_agent_ids"], serde_json::json!([]));
        assert!(markdown.contains("not evidence of live-market manipulation"));
    }

    #[test]
    fn report_lists_injected_agents_as_facts() {
        let mut spec = spec();
        spec.agents = crate::training_scenarios::basic_execution("train-1", 3).agents;
        let mut run = TrainingRun::new(spec);
        run.start().unwrap();
        let json = training_report_json(&run);
        assert_eq!(
            json["facts"]["injected_agent_ids"],
            serde_json::json!(["mm"])
        );
        let inferences = json["inferences"].as_array().unwrap();
        assert!(
            inferences
                .iter()
                .any(|item| item.as_str() == Some(SCENARIO_INJECTION_DISCLAIMER))
        );
    }
}
