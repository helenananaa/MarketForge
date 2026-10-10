//! Complete journal pages with public market data and strictly scoped accounting.
use super::*;
use std::collections::BTreeSet;

#[derive(Deserialize)]
pub(super) struct Args {
    account_id: AccountId,
    after_command_seq: Option<u64>,
    #[serde(default)]
    from_start: bool,
    limit: Option<usize>,
    order_id: Option<String>,
    #[serde(default)]
    include_market: bool,
}

pub(super) async fn history(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, instrument)): Path<(String, String)>,
    Query(q): Query<Args>,
) -> ApiResult<serde_json::Value> {
    if q.from_start && q.after_command_seq.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "conflicting history cursors",
        ));
    }
    let order_id = q
        .order_id
        .as_ref()
        .map(|v| v.parse::<u64>())
        .transpose()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid order_id"))?;
    let auth = authorize_room_read(
        &state,
        &headers,
        &room,
        RoomReadAccess::Account(q.account_id),
    )
    .await?;
    {
        let app = lock_state(&state).await?;
        let simulation = app
            .rooms
            .simulation_room(&room)
            .map_err(api_error_from_room)?;
        if !simulation.instrument_ids().contains(&instrument) {
            return Err(api_error(StatusCode::NOT_FOUND, "instrument not found"));
        }
    }
    let page = auth
        .journal
        .query_executions(
            &room,
            q.after_command_seq,
            q.from_start,
            q.limit.unwrap_or(100).clamp(1, 500),
        )
        .await
        .map_err(api_error_from_journal)?;
    validate_event_cursor(&room, q.after_command_seq, page.latest_command_seq)?;
    // Expiring an exhausted reduce-only maker can occur in somebody else's
    // command without a fill. Resolve those lifecycle IDs from durable ownership.
    let mut candidates = BTreeSet::new();
    for e in &page.executions {
        if e.instrument_id.as_deref() != Some(instrument.as_str()) {
            continue;
        }
        for event in &e.events {
            if matches!(
                event,
                EventSummary::OrderExpired { .. } | EventSummary::OrderCanceled { .. }
            ) && let Some(id) = serde_json::to_value(event)
                .ok()
                .and_then(|v| v.get("order_id").and_then(serde_json::Value::as_u64))
            {
                candidates.insert(id);
            }
        }
    }
    let mut owned = BTreeSet::new();
    for id in candidates {
        if !auth
            .journal
            .query_orders(
                &auth.user_id,
                &room,
                Some(&instrument),
                Some(q.account_id),
                1,
                Some(id),
            )
            .await
            .map_err(api_error_from_journal)?
            .is_empty()
        {
            owned.insert(id);
        }
    }
    let activities = page
        .executions
        .iter()
        .filter(|e| e.instrument_id.as_deref() == Some(instrument.as_str()))
        .filter_map(|e| activity(e, q.account_id, order_id, q.include_market, &owned))
        .collect::<Vec<_>>();
    let next = page
        .executions
        .last()
        .map(|e| e.command_seq)
        .or(q.after_command_seq);
    Ok(Json(
        serde_json::json!({"room_id":room,"instrument_id":instrument,"account_id":q.account_id,
        "activities":activities,"next_after_command_seq":next,"latest_command_seq":page.latest_command_seq,"has_more":page.has_more}),
    ))
}

fn activity(
    e: &RoomExecutionSummary,
    account: AccountId,
    filter: Option<u64>,
    public: bool,
    known_owned: &BTreeSet<u64>,
) -> Option<serde_json::Value> {
    let mut own_ids = known_owned.clone();
    let mut fills = Vec::new();
    let mut market = Vec::new();
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
            ..
        } = event
        {
            if public {
                market.push(
                    serde_json::json!({"type":"TradePrinted","trade_id":trade_id,
                "price_tick":price_tick,"qty":qty,"taker_side":taker_side}),
                );
            }
            let maker = *maker_account_id == account;
            let taker = *taker_account_id == account;
            if maker {
                own_ids.insert(*maker_order_id);
            }
            if taker {
                own_ids.insert(*taker_order_id);
            }
            if !maker && !taker {
                continue;
            }
            if filter.is_some_and(|id| {
                !(maker && id == *maker_order_id || taker && id == *taker_order_id)
            }) {
                continue;
            }
            let settlements=e.clearing_events.iter().filter_map(|c|match c {
                ClearingEventSummary::SpotTradeSettled{trade_id:id,buyer_account_id,seller_account_id,buyer_fee,seller_fee,buyer,seller,..} if id==trade_id=>{
                    let mut values=Vec::new();
                    if *buyer_account_id==account { values.push(serde_json::json!({"side":"Buy","fee_paid":buyer_fee.to_string(),"account_after":buyer})); }
                    if *seller_account_id==account { values.push(serde_json::json!({"side":"Sell","fee_paid":seller_fee.to_string(),"account_after":seller})); }
                    Some(values)
                }
                ClearingEventSummary::PerpTradeSettled{trade_id:id,buyer_account_id,seller_account_id,buyer_fee,seller_fee,buyer_realized_pnl,seller_realized_pnl,buyer,seller,..} if id==trade_id=>{
                    let mut values=Vec::new();
                    if *buyer_account_id==account { values.push(serde_json::json!({"side":"Buy","fee_paid":buyer_fee.to_string(),"realized_pnl":buyer_realized_pnl.to_string(),"account_after":buyer})); }
                    if *seller_account_id==account { values.push(serde_json::json!({"side":"Sell","fee_paid":seller_fee.to_string(),"realized_pnl":seller_realized_pnl.to_string(),"account_after":seller})); }
                    Some(values)
                }
                _=>None
            }).flatten().collect::<Vec<_>>();
            fills.push(serde_json::json!({"trade_id":trade_id,"price_tick":price_tick,"qty":qty,"taker_side":taker_side,
                "own_maker_order_id":if maker {Some(maker_order_id.to_string())}else{None},
                "own_taker_order_id":if taker {Some(taker_order_id.to_string())}else{None},"settlements":settlements}));
        } else if e.submit_account_id == Some(account)
            && matches!(
                event,
                EventSummary::OrderAccepted { .. }
                    | EventSummary::OrderRejected { .. }
                    | EventSummary::RiskRejected { .. }
                    | EventSummary::OrderAmended { .. }
                    | EventSummary::AmendRejected { .. }
                    | EventSummary::OrderCanceled { .. }
                    | EventSummary::CancelRejected { .. }
            )
            && let Ok(value) = serde_json::to_value(event)
            && let Some(id) = value.get("order_id").and_then(serde_json::Value::as_u64)
        {
            own_ids.insert(id);
        }
    }
    let orders = e
        .events
        .iter()
        .filter_map(|event| {
            if matches!(event, EventSummary::TradePrinted { .. }) {
                return None;
            }
            let mut data = serde_json::to_value(event).ok()?;
            let id = data.get("order_id")?.as_u64()?;
            if !own_ids.contains(&id) || filter.is_some_and(|v| v != id) {
                return None;
            }
            data["order_id"] = id.to_string().into();
            Some(data)
        })
        .collect::<Vec<_>>();
    let mut settlements = Vec::new();
    if filter.is_none() {
        for event in &e.clearing_events {
            let id = match event {
                ClearingEventSummary::PerpFundingSettled { account_id, .. }
                | ClearingEventSummary::PerpMarginStatusChanged { account_id, .. }
                | ClearingEventSummary::PerpLiquidationSettled { account_id, .. } => *account_id,
                _ => continue,
            };
            if id != account {
                continue;
            }
            let mut data = serde_json::to_value(event).ok()?;
            if let Some(value) = data.as_object_mut() {
                value.remove("auto_deleveraging_allocations");
                value.remove("socialized_loss_allocations");
            }
            settlements.push(data);
        }
    }
    let admission = if filter.is_none() && e.submit_account_id == Some(account) {
        Some(serde_json::json!({"accepted":e.accepted,"reject_reason":e.reject_reason}))
    } else {
        None
    };
    let price_updates = if public {
        serde_json::to_value(&e.price_updates).ok()?
    } else {
        serde_json::json!([])
    };
    if admission.is_none()
        && orders.is_empty()
        && fills.is_empty()
        && settlements.is_empty()
        && market.is_empty()
        && price_updates.as_array().is_none_or(Vec::is_empty)
    {
        return None;
    }
    Some(
        serde_json::json!({"command_seq":e.command_seq,"market_time_ms":e.market_time_ms,
        "orders":orders,"fills":fills,"settlements":settlements,"market_events":market,"price_updates":price_updates,"admission":admission}),
    )
}

pub(super) async fn rules(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, instrument)): Path<(String, String)>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(&state, &headers, &room, RoomReadAccess::Room).await?;
    let app = lock_state(&state).await?;
    let simulation = app
        .rooms
        .simulation_room(&room)
        .map_err(api_error_from_room)?;
    for venue in simulation.venue_ids() {
        let exchange = simulation
            .exchange(venue)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")))?;
        if let Some(config) = exchange
            .config()
            .markets
            .iter()
            .find(|m| m.instrument_id() == instrument)
        {
            return Ok(Json(
                serde_json::json!({"room_id":room,"instrument_id":instrument,"venue_id":venue,
                "market":config,"venue_rules":exchange.config().venue_rules,"price_unit":"integer price ticks","quantity_unit":"integer lots",
                "order_actions":["limit","market","ioc","fok","post_only","reduce_only","reduce_only_fok","reduce_only_limit","reduce_only_post_only","conditional","amend","cancel","bracket","protection"]}),
            ));
        }
    }
    Err(api_error(StatusCode::NOT_FOUND, "instrument not found"))
}

pub(super) async fn portfolio(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, account)): Path<(String, AccountId)>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(&state, &headers, &room, RoomReadAccess::Account(account)).await?;
    let app = lock_state(&state).await?;
    let portfolio = app
        .rooms
        .portfolio_snapshot(&room, account)
        .map_err(api_error_from_room)?;
    let venues = app
        .rooms
        .venue_account_snapshots_by_venue(&room)
        .map_err(api_error_from_room)?;
    let venues = venues
        .into_iter()
        .filter(|v| v.account.account_id == account)
        .collect::<Vec<_>>();
    Ok(Json(
        serde_json::json!({"room_id":room,"account_id":account,"portfolio":portfolio,"venues":venues}),
    ))
}

pub(super) async fn conditionals(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, instrument)): Path<(String, String)>,
    Query(q): Query<Args>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(
        &state,
        &headers,
        &room,
        RoomReadAccess::Account(q.account_id),
    )
    .await?;
    let app = lock_state(&state).await?;
    let simulation = app
        .rooms
        .simulation_room(&room)
        .map_err(api_error_from_room)?;
    for venue in simulation.venue_ids() {
        let exchange = simulation
            .exchange(venue)
            .map_err(|_| api_error(StatusCode::NOT_FOUND, "venue not found"))?;
        if exchange.instrument_ids().contains(&instrument.as_str()) {
            return Ok(Json(
                serde_json::json!({"conditionals":exchange.conditional_orders(&instrument,q.account_id)}),
            ));
        }
    }
    Err(api_error(StatusCode::NOT_FOUND, "instrument not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_trade_settlement_never_exposes_counterparty() {
        let mut execution=serde_json::from_value::<RoomExecutionSummary>(serde_json::json!({"room_id":"r","instrument_id":"x","submit_account_id":99,
            "command_seq":1,"market_time_ms":1,"status":"Running","accepted":true,"reject_reason":null,
            "events":[{"type":"TradePrinted","seq":1,"trade_id":1,"maker_order_id":10,"maker_account_id":20,"taker_order_id":11,"taker_account_id":99,"price_tick":100,"qty":2,"taker_side":"Buy"},
                {"type":"OrderFilled","seq":2,"order_id":10},{"type":"OrderFilled","seq":3,"order_id":11}],"clearing_event_count":0})).unwrap();
        execution
            .clearing_events
            .push(ClearingEventSummary::SpotTradeSettled {
                trade_id: 1,
                buyer_account_id: 99,
                seller_account_id: 20,
                price_tick: 100,
                qty: 2,
                notional: 200,
                buyer_fee: 3,
                seller_fee: 2,
                buyer: SpotAccountStateSummary {
                    account_id: 99,
                    cash_balance: 500,
                    position_qty: 2,
                    fees_paid: 3,
                },
                seller: SpotAccountStateSummary {
                    account_id: 20,
                    cash_balance: 1200,
                    position_qty: 0,
                    fees_paid: 2,
                },
            });
        let value = activity(&execution, 20, Some(10), true, &BTreeSet::new()).unwrap();
        assert_eq!(value["orders"].as_array().unwrap().len(), 1);
        assert_eq!(value["fills"][0]["settlements"][0]["fee_paid"], "2");
        assert!(value["fills"][0]["own_taker_order_id"].is_null());
        assert!(!serde_json::to_string(&value).unwrap().contains("99"));
        assert!(activity(&execution, 21, None, false, &BTreeSet::new()).is_none());
        execution.events.clear();
        execution.clearing_events.clear();
        execution.submit_account_id = Some(20);
        assert_eq!(
            activity(&execution, 20, None, false, &BTreeSet::new()).unwrap()["admission"]["accepted"],
            true
        );
        assert!(activity(&execution, 21, None, false, &BTreeSet::new()).is_none());
    }
}
