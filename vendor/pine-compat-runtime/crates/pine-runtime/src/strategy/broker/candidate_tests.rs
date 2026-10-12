use super::candidates::{
    BrokerCandidate, BrokerCandidateEvent, BrokerCandidatePhase, cmp_candidates,
};
use super::pending_closes::PendingCloseQuantity;
use super::pending_exits::{
    PendingExit, PendingExitQuantity, PendingExitTrigger, PendingTrailingActivation,
    PendingTrailingExit, PendingTrailingSpec, PendingTrailingState,
};
use super::types::{InternalOrderKey, StrategyCommandOrigin};
use super::*;
use crate::runtime::strategy_path::{HistoricalPath, HistoricalPathKind, MagnifierHostGap};
use pine_ir::StrategyMarginSetting;

fn falling_leg() -> crate::runtime::strategy_path::PathLeg {
    HistoricalPath::from_ohlc(10.0, 11.0, 8.0, 9.0)
        .expect("path")
        .legs()[1]
}

fn rising_leg() -> crate::runtime::strategy_path::PathLeg {
    HistoricalPath::from_ohlc(10.0, 11.0, 8.0, 9.0)
        .expect("path")
        .legs()[0]
}

#[test]
fn fixed_fx_exit_touches_roundoff_bar_extreme_but_not_a_subtick_miss() {
    for (short, extreme, should_fill) in [
        (false, 1.3494599999999999, true),
        (false, 1.349459, false),
        (true, 1.3494600000000003, true),
        (true, 1.349461, false),
    ] {
        let mut broker = BrokerState::new(5_000.0).with_price_tick(0.00001);
        let leg = if short {
            assert!(broker.entry_short("E".to_owned(), 0, 0, 1.36, 106.0));
            HistoricalPath::from_ohlc(1.35, 1.36, extreme, 1.355)
                .unwrap()
                .legs()[0]
        } else {
            assert!(broker.entry_long("E".to_owned(), 0, 0, 1.34274, 106.0));
            HistoricalPath::from_ohlc(1.34807, extreme, 1.34542, 1.3475)
                .unwrap()
                .legs()[0]
        };
        broker.place_exit_limit("X".to_owned(), "E".to_owned(), 1.34946, 0);
        let outcome = broker.take_next_entry_path_event(super::EntryPathTick {
            bar_index: 1,
            time: 1,
            leg,
            path_kind: if short {
                HistoricalPathKind::OpenLowHighClose
            } else {
                HistoricalPathKind::OpenHighLowClose
            },
            mark: leg.from.price,
            long_blocked_at_path_start: false,
            short_blocked_at_path_start: false,
        });
        assert_eq!(
            outcome.is_some_and(|event| event.is_fill()),
            should_fill,
            "short={short} extreme={extreme}"
        );
        if should_fill {
            assert_eq!(broker.trades[0].exit_price, 1.34946);
        }
    }
}

#[test]
fn collecting_candidates_does_not_mutate_broker_state() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("LIM".to_owned(), 1.0, 9.5, 0);
    broker.place_pending_stop_long_entry("STP".to_owned(), 1.0, 10.5, 0);
    broker.place_pending_stop_limit_long_entry("SL".to_owned(), 1.0, 10.8, 8.5, 0);
    broker.place_pending_close(
        "C".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    let before = broker.clone();
    let _ = broker.collect_market_open_candidates(1);
    let _ = broker.collect_path_leg_candidates(1, falling_leg());
    assert_eq!(broker, before);
}

#[test]
fn long_and_short_limit_stop_origins_emit_fill_candidates() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("LE".to_owned(), 1.0, 9.5, 0);
    broker.place_pending_limit_long_order("LO".to_owned(), 1.0, 9.2, 0);
    broker.place_pending_stop_short_entry("SE".to_owned(), 1.0, 9.0, 0);
    let candidates = broker.collect_path_leg_candidates(1, falling_leg());
    assert!(candidates.iter().any(|candidate| {
        candidate.public_id == "LE"
            && candidate.origin == StrategyCommandOrigin::Entry
            && candidate.event_kind == BrokerCandidateEvent::EntryOrOrderFill
    }));
    assert!(candidates.iter().any(|candidate| {
        candidate.public_id == "LO" && candidate.origin == StrategyCommandOrigin::Order
    }));
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.public_id == "SE")
    );
}

#[test]
fn stop_limit_activation_is_not_a_fill() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_stop_limit_long_entry("SL".to_owned(), 1.0, 10.8, 8.5, 0);
    let high_first_open_high = rising_leg();
    let candidates = broker.collect_path_leg_candidates(1, high_first_open_high);
    assert!(candidates.iter().any(|candidate| {
        candidate.public_id == "SL"
            && candidate.event_kind == BrokerCandidateEvent::StopLimitActivation
    }));
    assert!(!candidates.iter().any(|candidate| {
        candidate.public_id == "SL"
            && candidate.event_kind == BrokerCandidateEvent::EntryOrOrderFill
    }));
}

#[test]
fn high_first_long_stop_limit_can_fill_after_same_bar_activation() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_stop_limit_long_entry("SL".to_owned(), 1.0, 10.8, 8.5, 0);
    let path = HistoricalPath::from_ohlc(10.0, 11.0, 8.0, 9.0).expect("path");
    let [rising, falling, _] = path.legs();
    let outcome = broker
        .take_next_entry_path_event(super::EntryPathTick {
            bar_index: 1,
            time: 10,
            leg: rising,
            path_kind: path.kind,
            mark: rising.from.price,
            long_blocked_at_path_start: false,
            short_blocked_at_path_start: false,
        })
        .expect("activation");
    assert!(!outcome.is_fill());
    let candidates = broker.collect_path_leg_candidates_for(1, falling, path.kind);
    assert!(candidates.iter().any(|candidate| {
        candidate.public_id == "SL"
            && candidate.event_kind == BrokerCandidateEvent::EntryOrOrderFill
            && candidate.fill_price_or_mark == 8.5
    }));
}

#[test]
fn short_stop_limit_does_not_collect_same_bar_fill() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_stop_limit_short_entry("SL".to_owned(), 1.0, 8.2, 10.8, 0);
    let path = HistoricalPath::from_ohlc(10.0, 13.0, 8.0, 11.0).expect("path");
    let [falling, rising, _] = path.legs();
    let outcome = broker
        .take_next_entry_path_event(super::EntryPathTick {
            bar_index: 1,
            time: 10,
            leg: falling,
            path_kind: path.kind,
            mark: falling.from.price,
            long_blocked_at_path_start: false,
            short_blocked_at_path_start: false,
        })
        .expect("activation");
    assert!(!outcome.is_fill());
    let candidates = broker.collect_path_leg_candidates_for(1, rising, path.kind);
    assert!(!candidates.iter().any(|candidate| {
        candidate.public_id == "SL"
            && candidate.event_kind == BrokerCandidateEvent::EntryOrOrderFill
    }));
}

#[test]
fn same_price_uses_creation_sequence_not_entry_before_exit() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("NX".to_owned(), 1.0, 9.5, 0);
    broker.order_book.replace_or_append_exit(PendingExit {
        key: InternalOrderKey(0),
        id: "EX".to_owned(),
        from_entry: "BASE".to_owned(),
        target_trade_key: None,
        trigger: PendingExitTrigger::Limit(9.5),
        quantity: PendingExitQuantity::Full,
        reserved_quantity: 1.0,
        multiple_reservation: false,
        last_update_bar_index: 0,
        metadata: StrategyExitMetadata::default(),
    });
    assert!(broker.entry_long("BASE".to_owned(), 0, 10, 10.0, 1.0));
    let candidates = broker.collect_path_leg_candidates(1, falling_leg());
    let nx = candidates
        .iter()
        .find(|candidate| candidate.public_id == "NX")
        .expect("nx");
    let ex = candidates
        .iter()
        .find(|candidate| candidate.public_id == "EX")
        .expect("ex");
    assert_eq!(nx.crossing_price, ex.crossing_price);
    assert_eq!(
        cmp_candidates(nx, ex, Some(falling_leg())),
        nx.creation_sequence.cmp(&ex.creation_sequence)
    );
    assert!(nx.creation_sequence < ex.creation_sequence);
}

#[test]
fn short_gap_fills_deeper_marketable_profit_target_first() {
    let mut broker = BrokerState::new_with_account_settings_and_pyramiding(
        100_000.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::default(),
        StrategyMarginSetting::default(),
        2,
    );
    assert!(broker.entry_short("A".to_owned(), 0, 10, 100.0, 87.0));
    assert!(broker.entry_short("B".to_owned(), 0, 10, 100.0, 131.0));
    broker.place_exit_limit("XA".to_owned(), "A".to_owned(), 98.0, 0);
    broker.place_exit_limit("XB".to_owned(), "B".to_owned(), 95.0, 0);

    let candidates =
        broker.collect_observed_price_candidates(1, 94.0, broker.event_generation(), false);
    let exits: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.event_kind == BrokerCandidateEvent::ExitFill)
        .map(|candidate| candidate.public_id.as_str())
        .collect();
    assert_eq!(exits, vec!["XB", "XA"]);
}

#[test]
fn rising_and_falling_legs_order_crossings() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_stop_long_entry("HI".to_owned(), 1.0, 10.8, 0);
    broker.place_pending_limit_long_entry("LO".to_owned(), 1.0, 9.2, 0);
    let falling = falling_leg();
    let falling_candidates = broker.collect_path_leg_candidates(1, falling);
    let lo = falling_candidates
        .iter()
        .find(|candidate| candidate.public_id == "LO")
        .expect("lo");
    assert_eq!(lo.event_kind, BrokerCandidateEvent::EntryOrOrderFill);
    let rising = rising_leg();
    let rising_candidates = broker.collect_path_leg_candidates(1, rising);
    assert!(
        rising_candidates
            .iter()
            .any(|candidate| candidate.public_id == "HI")
    );
}

#[test]
fn user_exit_ranks_before_margin_at_the_same_mark() {
    let exit = BrokerCandidate {
        event_kind: BrokerCandidateEvent::ExitFill,
        phase: BrokerCandidatePhase::PathLeg,
        path_leg: 1,
        crossing_price: 0.1937,
        fill_price_or_mark: 0.1937,
        creation_sequence: 3,
        stable_order_key: InternalOrderKey(3),
        observed_generation: 0,
        origin: StrategyCommandOrigin::Exit,
        public_id: "USER_EXIT".to_owned(),
    };
    let margin = BrokerCandidate {
        event_kind: BrokerCandidateEvent::MarginCall,
        phase: BrokerCandidatePhase::PathLeg,
        path_leg: 1,
        crossing_price: 0.1937,
        fill_price_or_mark: 0.1937,
        creation_sequence: u64::MAX,
        stable_order_key: super::candidates::MARGIN_ORDER_KEY,
        observed_generation: 0,
        origin: StrategyCommandOrigin::MarginCall,
        public_id: "Margin Call".to_owned(),
    };
    assert_eq!(
        cmp_candidates(&exit, &margin, Some(falling_leg())),
        std::cmp::Ordering::Less
    );
}

#[test]
fn margin_candidate_is_read_only() {
    let mut broker = BrokerState::new_with_account_settings(
        165.0,
        None,
        0.0,
        0.0,
        StrategyMarginSetting::explicit(25.0),
        StrategyMarginSetting::default(),
    );
    assert!(broker.entry_long("L".to_owned(), 0, 10, 4.0, 100.0));
    let adverse = HistoricalPath::from_ohlc(5.0, 6.0, 3.0, 4.0)
        .expect("path")
        .legs()[1];
    let before = broker.clone();
    let candidates = broker.collect_path_leg_candidates(1, adverse);
    assert_eq!(broker, before);
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.event_kind == BrokerCandidateEvent::MarginCall)
    );
}

#[test]
fn stale_generation_is_observable_without_applying() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_limit_long_entry("LIM".to_owned(), 1.0, 9.5, 0);
    let candidates = broker.collect_path_leg_candidates(1, falling_leg());
    let observed = candidates[0].observed_generation;
    broker.bump_event_generation();
    assert_ne!(observed, broker.event_generation());
}

#[test]
fn trailing_activation_is_not_a_fill() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 10.0, 1.0));
    broker.order_book.replace_or_append_exit(PendingExit {
        key: InternalOrderKey(0),
        id: "TR".to_owned(),
        from_entry: "L".to_owned(),
        target_trade_key: None,
        trigger: PendingExitTrigger::Trailing(PendingTrailingExit {
            spec: PendingTrailingSpec {
                activation: PendingTrailingActivation::Price(10.5),
                offset_price_distance: 0.5,
            },
            state: PendingTrailingState::Inactive,
        }),
        quantity: PendingExitQuantity::Full,
        reserved_quantity: 1.0,
        multiple_reservation: false,
        last_update_bar_index: 0,
        metadata: StrategyExitMetadata::default(),
    });
    let candidates = broker.collect_path_leg_candidates(1, rising_leg());
    assert!(candidates.iter().any(|candidate| {
        candidate.public_id == "TR"
            && candidate.event_kind == BrokerCandidateEvent::TrailingActivation
    }));
    assert!(!candidates.iter().any(|candidate| {
        candidate.public_id == "TR" && candidate.event_kind == BrokerCandidateEvent::ExitFill
    }));
}

#[test]
fn gap_open_activates_trailing_without_filling_it() {
    let mut broker = BrokerState::new(100_000.0);
    assert!(broker.entry_long("L".to_owned(), 0, 10, 10.0, 1.0));
    broker.order_book.replace_or_append_exit(PendingExit {
        key: InternalOrderKey(0),
        id: "TR".to_owned(),
        from_entry: "L".to_owned(),
        target_trade_key: None,
        trigger: PendingExitTrigger::Trailing(PendingTrailingExit {
            spec: PendingTrailingSpec {
                activation: PendingTrailingActivation::Price(10.5),
                offset_price_distance: 0.5,
            },
            state: PendingTrailingState::Inactive,
        }),
        quantity: PendingExitQuantity::Full,
        reserved_quantity: 1.0,
        multiple_reservation: false,
        last_update_bar_index: 0,
        metadata: StrategyExitMetadata::default(),
    });
    let gap = MagnifierHostGap {
        previous_close: 10.0,
        next_open: 11.0,
    };
    let candidates = broker.collect_gap_candidates(1, gap, broker.event_generation());
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].event_kind,
        BrokerCandidateEvent::TrailingActivation
    );

    let tick = super::EntryPathTick {
        bar_index: 1,
        time: 10,
        leg: crate::runtime::strategy_path::PathLeg::point(gap.next_open),
        path_kind: HistoricalPathKind::OpenHighLowClose,
        mark: gap.next_open,
        long_blocked_at_path_start: false,
        short_blocked_at_path_start: false,
    };
    let outcome = broker.take_next_gap_event(tick, gap).expect("activation");
    assert!(!outcome.is_fill());
    assert!(broker.take_next_gap_event(tick, gap).is_none());
}

#[test]
fn stop_limit_gap_does_not_reuse_pre_activation_prices() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_stop_limit_long_entry("SL".to_owned(), 1.0, 10.5, 10.2, 0);
    let gap = MagnifierHostGap {
        previous_close: 10.0,
        next_open: 11.0,
    };
    let tick = super::EntryPathTick {
        bar_index: 1,
        time: 10,
        leg: crate::runtime::strategy_path::PathLeg::point(gap.next_open),
        path_kind: HistoricalPathKind::OpenHighLowClose,
        mark: gap.next_open,
        long_blocked_at_path_start: false,
        short_blocked_at_path_start: false,
    };

    let outcome = broker.take_next_gap_event(tick, gap).expect("activation");
    assert!(!outcome.is_fill());
    assert!(broker.take_next_gap_event(tick, gap).is_none());
    assert_eq!(broker.position_size(), 0.0);
}

#[test]
fn stop_limit_gap_fills_at_open_only_when_limit_is_marketable() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_stop_limit_long_entry("SL".to_owned(), 1.0, 10.5, 11.5, 0);
    let gap = MagnifierHostGap {
        previous_close: 10.0,
        next_open: 11.0,
    };
    let outcome = broker
        .take_next_gap_event(
            super::EntryPathTick {
                bar_index: 1,
                time: 10,
                leg: crate::runtime::strategy_path::PathLeg::point(gap.next_open),
                path_kind: HistoricalPathKind::OpenHighLowClose,
                mark: gap.next_open,
                long_blocked_at_path_start: false,
                short_blocked_at_path_start: false,
            },
            gap,
        )
        .expect("fill");
    assert!(outcome.is_fill());
    assert_eq!(broker.position_size(), 1.0);
}

#[test]
fn market_open_collects_entries_and_closes() {
    let mut broker = BrokerState::new(100_000.0);
    broker.place_pending_market_long_entry("E".to_owned(), 1.0, 0);
    broker.place_pending_close(
        "E".to_owned(),
        PendingCloseQuantity::Full,
        0,
        StrategyOrderMetadata::default(),
    );
    let candidates = broker.collect_market_open_candidates(1);
    assert!(candidates.iter().any(|candidate| {
        candidate.public_id == "E" && candidate.event_kind == BrokerCandidateEvent::EntryOrOrderFill
    }));
    assert!(candidates.iter().any(|candidate| {
        candidate.public_id == "E" && candidate.event_kind == BrokerCandidateEvent::ExitFill
    }));
}
