//! Exchange-facing projections. Public messages never expose account/order
//! identifiers; private accounting is projected independently for each owner.
use super::*;
use std::collections::BTreeSet;

pub(super) fn public_trades(e: &RoomExecutionSummary) -> Vec<serde_json::Value> {
    e.events.iter().filter_map(|event| match event {
        EventSummary::TradePrinted { seq, trade_id, price_tick, qty, taker_side, .. } =>
            Some(serde_json::json!({"type":"TradePrinted","seq":seq,"trade_id":trade_id,
                "price_tick":price_tick,"qty":qty,"taker_side":taker_side,"market_time_ms":e.market_time_ms})),
        _ => None,
    }).collect()
}

pub(super) fn public_liquidations(e: &RoomExecutionSummary) -> Vec<serde_json::Value> {
    if !e.is_liquidation || !e.accepted {
        return Vec::new();
    }
    // Publish each actual fill, including partial attempts. A settlement-only
    // notification cannot describe liquidations that are waiting for depth.
    e.events.iter().filter_map(|event| match event {
        EventSummary::TradePrinted { trade_id, price_tick, qty, taker_side, taker_position_side, .. } =>
            Some(serde_json::json!({"type":"LiquidationTrade","liquidation_attempt_seq":e.command_seq,
                "trade_id":trade_id,"instrument_id":e.instrument_id,"market_time_ms":e.market_time_ms,
                "side":taker_side,"position_side":taker_position_side,"price_tick":price_tick,"qty":qty})),
        _ => None,
    }).collect()
}

/// Event-local account IDs, including loss-sharing recipients with no trades.
pub(super) fn affected_accounts(e: &RoomExecutionSummary) -> BTreeSet<AccountId> {
    let mut ids = BTreeSet::new();
    ids.extend(e.submit_account_id);
    ids.extend(e.wallet_updates.iter().map(|u| u.account_id));
    ids.extend(e.margin_updates.iter().map(|u| u.account.account_id));
    for event in &e.events {
        if let EventSummary::TradePrinted {
            maker_account_id,
            taker_account_id,
            ..
        } = event
        {
            ids.extend([*maker_account_id, *taker_account_id]);
        }
    }
    for event in &e.clearing_events {
        match event {
            ClearingEventSummary::SpotTradeSettled {
                buyer_account_id,
                seller_account_id,
                ..
            }
            | ClearingEventSummary::PerpTradeSettled {
                buyer_account_id,
                seller_account_id,
                ..
            } => {
                ids.extend([*buyer_account_id, *seller_account_id]);
            }
            ClearingEventSummary::PerpFundingSettled { account_id, .. }
            | ClearingEventSummary::PerpMarginStatusChanged { account_id, .. } => {
                ids.insert(*account_id);
            }
            ClearingEventSummary::PerpLiquidationSettled {
                account_id,
                auto_deleveraging_allocations,
                socialized_loss_allocations,
                ..
            } => {
                ids.insert(*account_id);
                ids.extend(auto_deleveraging_allocations.iter().map(|a| a.account_id));
                ids.extend(socialized_loss_allocations.iter().map(|a| a.account_id));
            }
        }
    }
    ids
}

pub(super) fn account_events(
    e: &RoomExecutionSummary,
    ids: &BTreeSet<AccountId>,
) -> Vec<serde_json::Value> {
    let mut output = Vec::new();
    for event in &e.clearing_events {
        match event {
            ClearingEventSummary::SpotTradeSettled {
                trade_id,
                price_tick,
                qty,
                buyer_fee,
                seller_fee,
                buyer,
                seller,
                ..
            } => {
                for (account, fee, side) in
                    [(buyer, buyer_fee, "Buy"), (seller, seller_fee, "Sell")]
                {
                    if ids.contains(&account.account_id) {
                        output.push(serde_json::json!({"type":"SpotTradeSettled","account_id":account.account_id,
                            "trade_id":trade_id,"price_tick":price_tick,"qty":qty,"side":side,
                            "fee":fee.to_string(),"account":account}));
                    }
                }
            }
            ClearingEventSummary::PerpTradeSettled {
                trade_id,
                price_tick,
                qty,
                buyer_fee,
                seller_fee,
                buyer_realized_pnl,
                seller_realized_pnl,
                buyer,
                seller,
                ..
            } => {
                for (account, fee, pnl, side) in [
                    (buyer, buyer_fee, buyer_realized_pnl, "Buy"),
                    (seller.as_ref(), seller_fee, seller_realized_pnl, "Sell"),
                ] {
                    if ids.contains(&account.account_id) {
                        output.push(serde_json::json!({"type":"PerpTradeSettled","account_id":account.account_id,
                            "trade_id":trade_id,"price_tick":price_tick,"qty":qty,"side":side,
                            "fee":fee.to_string(),"realized_pnl":pnl.to_string(),"account":account}));
                    }
                }
            }
            ClearingEventSummary::PerpFundingSettled { account_id, .. }
            | ClearingEventSummary::PerpMarginStatusChanged { account_id, .. } => {
                if ids.contains(account_id) {
                    output
                        .push(serde_json::to_value(event).expect("serializable clearing summary"));
                }
            }
            ClearingEventSummary::PerpLiquidationSettled {
                account_id,
                auto_deleveraging_allocations,
                socialized_loss_allocations,
                ..
            } => {
                if ids.contains(account_id) {
                    let mut value =
                        serde_json::to_value(event).expect("serializable clearing summary");
                    let object = value.as_object_mut().unwrap();
                    object.remove("auto_deleveraging_allocations");
                    object.remove("socialized_loss_allocations");
                    output.push(value);
                }
                for allocation in auto_deleveraging_allocations {
                    if ids.contains(&allocation.account_id) {
                        let mut value =
                            serde_json::to_value(allocation).expect("serializable ADL summary");
                        value["type"] = "PerpAutoDeleveraged".into();
                        output.push(value);
                    }
                }
                for allocation in socialized_loss_allocations {
                    if ids.contains(&allocation.account_id) {
                        let mut value =
                            serde_json::to_value(allocation).expect("serializable loss summary");
                        value["type"] = "PerpSocializedLossApplied".into();
                        output.push(value);
                    }
                }
            }
        }
    }
    for update in &e.margin_updates {
        if ids.contains(&update.account.account_id) {
            let account = PerpAccountStateSummary::from_snapshot(update.account.clone());
            output.push(serde_json::json!({"type":"PerpMarginUpdated","instrument_id":update.instrument_id,"account_id":account.account_id,"account":account}));
        }
    }
    output
}

pub(super) async fn private_payload(
    e: &RoomExecutionSummary,
    user: &str,
    journal: &JournalCoordinator,
) -> Option<serde_json::Value> {
    let admin = journal
        .user_can_administer_room(user, &e.room_id)
        .await
        .ok()?;
    let mut ids = BTreeSet::new();
    for id in affected_accounts(e) {
        if admin
            || journal
                .user_can_access_account(user, &e.room_id, id)
                .await
                .ok()?
        {
            ids.insert(id);
        }
    }
    // Resolve order lifecycle ownership even when somebody else's command
    // expires/cancels a maker without printing a trade.
    let mut orders = BTreeSet::new();
    for event in &e.events {
        if let EventSummary::TradePrinted {
            maker_order_id,
            maker_account_id,
            taker_order_id,
            taker_account_id,
            ..
        } = event
        {
            if ids.contains(maker_account_id) {
                orders.insert(*maker_order_id);
            }
            if ids.contains(taker_account_id) {
                orders.insert(*taker_order_id);
            }
        }
    }
    let mut lifecycle = Vec::new();
    for event in &e.events {
        if matches!(event, EventSummary::TradePrinted { .. }) {
            continue;
        }
        let value = serde_json::to_value(event).ok()?;
        let id = value["order_id"].as_u64()?;
        let owned = admin
            || orders.contains(&id)
            || !journal
                .query_orders(
                    user,
                    &e.room_id,
                    e.instrument_id.as_deref(),
                    None,
                    1,
                    Some(id),
                )
                .await
                .ok()?
                .is_empty();
        // Rejected new orders have no order projection. Their submitter still
        // owns the rejection, but does not own other cancellations in a batch.
        let rejection = matches!(
            event,
            EventSummary::OrderRejected { .. }
                | EventSummary::RiskRejected { .. }
                | EventSummary::CancelRejected { .. }
                | EventSummary::AmendRejected { .. }
        );
        if owned || (rejection && e.submit_account_id.is_some_and(|id| ids.contains(&id))) {
            lifecycle.push(value);
        }
    }
    let accounting = account_events(e, &ids);
    let mut fills = Vec::new();
    for event in &e.events {
        if let EventSummary::TradePrinted {
            trade_id,
            maker_order_id,
            maker_account_id,
            taker_order_id,
            taker_account_id,
            price_tick,
            qty,
            taker_side,
            maker_position_side,
            taker_position_side,
            ..
        } = event
        {
            for (account, order, maker, side, leg) in [
                (
                    *maker_account_id,
                    *maker_order_id,
                    true,
                    taker_side.opposite(),
                    maker_position_side,
                ),
                (
                    *taker_account_id,
                    *taker_order_id,
                    false,
                    *taker_side,
                    taker_position_side,
                ),
            ] {
                if ids.contains(&account) {
                    fills.push(serde_json::json!({"type":"TradePrinted","trade_id":trade_id,
                    "account_id":account,"order_id":order,"is_maker":maker,"side":side,"position_side":leg,
                    "price_tick":price_tick,"qty":qty,"market_time_ms":e.market_time_ms}));
                }
            }
        }
    }
    if ids.is_empty() && lifecycle.is_empty() {
        return None;
    }
    let wallets = e
        .wallet_updates
        .iter()
        .filter(|update| ids.contains(&update.account_id))
        .map(|update| {
            let mut value = serde_json::to_value(update).expect("serializable wallet receipt");
            value["type"] = "WalletChanged".into();
            value["reasons"] = serde_json::json!(
                accounting
                    .iter()
                    .filter(|event| event["account_id"] == update.account_id)
                    .map(|event| event["type"].clone())
                    .collect::<Vec<_>>()
            );
            if value["reasons"].as_array().unwrap().is_empty() {
                value["reasons"] = serde_json::json!(["OrderReservationChanged"]);
            }
            value
        })
        .collect::<Vec<_>>();
    Some(
        serde_json::json!({"room_id":e.room_id,"instrument_id":e.instrument_id,"command_seq":e.command_seq,
        "visible_account_ids":ids,
        "market_time_ms":e.market_time_ms,"status":e.status,"accepted":e.accepted,
        "reject_reason":e.submit_account_id.filter(|id| ids.contains(id)).and(e.reject_reason.as_ref()),
        "events":lifecycle,"trades":fills,"clearing_event_count":accounting.len(),
        "clearing_events":accounting,"wallet_changes":wallets,"price_updates":e.price_updates,
        "funding_settlement":e.funding_settlement}),
    )
}

pub(super) async fn private_payload_allowed(
    payload: &serde_json::Value,
    user: &str,
    room: &str,
    journal: &JournalCoordinator,
) -> bool {
    fn collect(value: &serde_json::Value, ids: &mut BTreeSet<u64>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(id) = map.get("account_id").and_then(serde_json::Value::as_u64) {
                    ids.insert(id);
                }
                for name in ["accounts", "visible_account_ids"] {
                    if let Some(items) = map.get(name).and_then(serde_json::Value::as_array) {
                        ids.extend(items.iter().filter_map(serde_json::Value::as_u64));
                    }
                }
                for value in map.values() {
                    collect(value, ids);
                }
            }
            serde_json::Value::Array(items) => {
                for value in items {
                    collect(value, ids);
                }
            }
            _ => {}
        }
    }
    let mut ids = BTreeSet::new();
    collect(payload, &mut ids);
    for id in ids {
        if !journal
            .user_can_access_account(user, room, id)
            .await
            .unwrap_or(false)
        {
            return false;
        }
    }
    true
}
