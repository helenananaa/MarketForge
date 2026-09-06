use serde::{Deserialize, Serialize};

use crate::{
    gateway::{
        GatewayError, GatewayExecution, GatewayRequest, OrderAction, ParticipantId, TradingApi,
    },
    market::InstrumentId,
    model::AccountId,
    observation::ParticipantObservation,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ParticipantKind {
    Human,
    RuleAgent,
    LlmAgent,
    Strategy,
    MarketMaker,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParticipantConfig {
    pub participant_id: ParticipantId,
    pub kind: ParticipantKind,
    pub room_id: String,
    pub account_id: AccountId,
    #[serde(default)]
    pub instrument_id: Option<InstrumentId>,
}

pub trait Participant: Send {
    fn config(&self) -> &ParticipantConfig;
    fn observe(&mut self, view: &ParticipantObservation);
    fn decide(&mut self) -> Vec<OrderAction>;
}

pub fn run_participant_once<T: TradingApi, P: Participant + ?Sized>(
    api: &mut T,
    participant: &mut P,
) -> Result<Vec<GatewayExecution>, GatewayError> {
    let config = participant.config().clone();
    let instrument_id = config
        .instrument_id
        .clone()
        .ok_or(GatewayError::MissingInstrument)?;
    let view = api.participant_observation(&config.room_id, &instrument_id, config.account_id)?;
    participant.observe(&view);

    participant
        .decide()
        .into_iter()
        .map(|action| {
            api.submit_action(GatewayRequest {
                participant_id: config.participant_id.clone(),
                room_id: config.room_id.clone(),
                instrument_id: Some(instrument_id.clone()),
                account_id: config.account_id,
                action,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpotRiskConfig,
        actor::{ActorExecutionResult, MarketExecution},
        gateway::OrderGateway,
        market::{InstrumentConfig, MarketConfig, SpotMarketConfig},
        model::Side,
        room::RoomManager,
        scenario::{ScenarioAccount, ScenarioConfig},
        spot::SpotClearingConfig,
    };

    struct FixedBuyer {
        config: ParticipantConfig,
        has_acted: bool,
    }

    impl Participant for FixedBuyer {
        fn config(&self) -> &ParticipantConfig {
            &self.config
        }

        fn observe(&mut self, _view: &ParticipantObservation) {}

        fn decide(&mut self) -> Vec<OrderAction> {
            if self.has_acted {
                return Vec::new();
            }
            self.has_acted = true;
            vec![OrderAction::PlaceLimit {
                side: Side::Buy,
                price_tick: 100,
                qty: 2,
            }]
        }
    }

    fn spot_scenario() -> ScenarioConfig {
        ScenarioConfig {
            room_id: "room-1".to_string(),
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
                cash_balance: 1_000,
            }],
            seed_orders: vec![],
            routed_seed_orders: Vec::new(),
        }
    }

    #[test]
    fn participant_runs_through_same_gateway_as_humans() {
        let mut rooms = RoomManager::new();
        rooms.create_room(spot_scenario()).unwrap();
        let mut gateway = OrderGateway::new(&mut rooms, 1);
        let mut participant = FixedBuyer {
            config: ParticipantConfig {
                participant_id: "rule-buyer-1".to_string(),
                kind: ParticipantKind::RuleAgent,
                room_id: "room-1".to_string(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".to_string()),
            },
            has_acted: false,
        };

        let executions = run_participant_once(&mut gateway, &mut participant)
            .expect("participant should submit through gateway");

        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].participant_id, "rule-buyer-1");
        let ActorExecutionResult::Accepted(MarketExecution::Spot(result)) =
            &executions[0].execution.result
        else {
            panic!("expected accepted spot execution");
        };
        assert!(result.clearing_events.is_empty());
    }
}
