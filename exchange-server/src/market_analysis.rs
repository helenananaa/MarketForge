//! Exact public window statistics at a pinned room clock/journal head.
use super::*;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Args {
    window_ms: Option<u64>,
    end_market_time_ms: Option<u64>,
    scan_limit: Option<usize>,
    bucket_ms: Option<u64>,
    price_group_tick: Option<i64>,
    book_levels: Option<usize>,
    large_trade_min_notional: Option<String>,
}

fn bad(message: &str) -> ApiError {
    api_error(StatusCode::BAD_REQUEST, message)
}
fn overflow() -> ApiError {
    api_error(
        StatusCode::UNPROCESSABLE_ENTITY,
        "window aggregate exceeds exact i128 range",
    )
}
fn add(value: &mut i128, amount: i128) -> Result<(), ApiError> {
    *value = value.checked_add(amount).ok_or_else(overflow)?;
    Ok(())
}

#[derive(Default)]
struct Flow {
    count: u64,
    buy: i128,
    sell: i128,
    quote: i128,
    buy_quote: i128,
    sell_quote: i128,
}
impl Flow {
    fn fill(&mut self, price: i64, qty: u64, side: exchange_core::Side) -> Result<(), ApiError> {
        self.count += 1;
        let qty = i128::from(qty);
        let quote = i128::from(price) * qty;
        add(&mut self.quote, quote)?;
        if side == exchange_core::Side::Buy {
            add(&mut self.buy, qty)?;
            add(&mut self.buy_quote, quote)?;
        } else {
            add(&mut self.sell, qty)?;
            add(&mut self.sell_quote, quote)?;
        }
        Ok(())
    }
    fn json(&self) -> Result<serde_json::Value, ApiError> {
        let qty = self.buy.checked_add(self.sell).ok_or_else(overflow)?;
        Ok(
            serde_json::json!({"trade_count":self.count,"volume_qty":qty.to_string(),"quote_volume":self.quote.to_string(),
            "buy_volume_qty":self.buy.to_string(),"sell_volume_qty":self.sell.to_string(),
            "buy_quote_volume":self.buy_quote.to_string(),"sell_quote_volume":self.sell_quote.to_string(),
            "delta_qty":(self.buy-self.sell).to_string(),"delta_quote":(self.buy_quote-self.sell_quote).to_string(),
            "vwap_tick":if qty == 0 { serde_json::Value::Null } else { serde_json::json!({"numerator":self.quote.to_string(),"denominator":qty.to_string()}) }}),
        )
    }
}

pub(super) async fn window(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, instrument)): Path<(String, String)>,
    Query(q): Query<Args>,
) -> ApiResult<serde_json::Value> {
    analyze(state, headers, room, instrument, q, false).await
}

pub(super) async fn order_flow(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, instrument)): Path<(String, String)>,
    Query(q): Query<Args>,
) -> ApiResult<serde_json::Value> {
    analyze(state, headers, room, instrument, q, true).await
}

async fn analyze(
    state: SharedState,
    headers: HeaderMap,
    room: String,
    instrument: String,
    q: Args,
    detailed: bool,
) -> ApiResult<serde_json::Value> {
    let window_ms = q.window_ms.unwrap_or(30000);
    let limit = q.scan_limit.unwrap_or(20000);
    let bucket_ms = q.bucket_ms.unwrap_or(window_ms.div_ceil(511).max(1000));
    let group = q.price_group_tick.unwrap_or(1);
    let levels = q.book_levels.unwrap_or(10);
    if !(1..=86400000).contains(&window_ms)
        || !(1..=100000).contains(&limit)
        || bucket_ms == 0
        || window_ms / bucket_ms >= 512
        || group < 1
        || !(1..=100).contains(&levels)
    {
        return Err(bad(
            "window_ms 1-86400000; scan_limit 1-100000; at most 512 buckets; positive price_group_tick; book_levels 1-100",
        ));
    }
    let large_min = q
        .large_trade_min_notional
        .as_deref()
        .unwrap_or("1000000")
        .parse::<i128>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| {
            bad("large_trade_min_notional must be a nonnegative i128 decimal integer")
        })?;
    let auth = authorize_room_read(&state, &headers, &room, RoomReadAccess::Room).await?;
    let (now, head, book, status) = {
        let app = state.app.read_room(&room).await;
        let book = app
            .rooms
            .book_snapshot_for(&room, &instrument)
            .map_err(api_error_from_room)?;
        (
            app.rooms
                .clock(&room)
                .map_err(api_error_from_room)?
                .market_time_ms(),
            app.rooms
                .simulation_room(&room)
                .map_err(api_error_from_room)?
                .next_command_seq()
                .saturating_sub(1),
            book,
            app.rooms.status(&room).map_err(api_error_from_room)?,
        )
    };
    let end = q.end_market_time_ms.unwrap_or(now);
    if end > now {
        return Err(bad("window end exceeds current simulation time"));
    }
    let start = end.saturating_sub(window_ms);
    let page = auth
        .journal
        .query_execution_window(&room, start, end, head, limit)
        .await
        .map_err(api_error_from_journal)?;
    let mut flow = Flow::default();
    let mut liquidations = Flow::default();
    let mut large = Flow::default();
    let mut buckets: BTreeMap<u64, Flow> = BTreeMap::new();
    let mut profile: BTreeMap<i64, Flow> = BTreeMap::new();
    let mut profile_truncated = false;
    let (mut funding_count, mut settled_count, mut skipped_count, mut transfer, mut last_rate) =
        (0u64, 0u64, 0u64, 0i128, None);
    let (mut first_price, mut last_price) = (None, None);
    for e in &page.executions {
        if e.instrument_id.as_deref() == Some(instrument.as_str()) {
            for event in &e.events {
                if let EventSummary::TradePrinted {
                    price_tick,
                    qty,
                    taker_side,
                    ..
                } = event
                {
                    flow.fill(*price_tick, *qty, *taker_side)?;
                    first_price.get_or_insert(*price_tick);
                    last_price = Some(*price_tick);
                    if e.is_liquidation && e.accepted {
                        liquidations.fill(*price_tick, *qty, *taker_side)?;
                    }
                    if i128::from(*price_tick) * i128::from(*qty) >= large_min {
                        large.fill(*price_tick, *qty, *taker_side)?;
                    }
                    if detailed {
                        let time = e.market_time_ms.expect("window rows have timestamps");
                        let bucket = start + (time - start) / bucket_ms * bucket_ms;
                        buckets
                            .entry(bucket)
                            .or_default()
                            .fill(*price_tick, *qty, *taker_side)?;
                        let price = *price_tick / group * group;
                        if profile.contains_key(&price) || profile.len() < 512 {
                            profile.entry(price).or_default().fill(
                                *price_tick,
                                *qty,
                                *taker_side,
                            )?;
                        } else {
                            profile_truncated = true;
                        }
                    }
                }
            }
        }
        if let Some(funding) = &e.funding_settlement
            && funding.instrument_id == instrument
        {
            funding_count += 1;
            if funding.status == exchange_core::FundingStatus::Settled {
                settled_count += 1;
                add(&mut transfer, funding.total_transfer)?;
            } else {
                skipped_count += 1;
            }
            last_rate = Some(funding.rate_ppm);
        }
    }
    let mut metrics = flow.json()?;
    metrics["last_price_tick"] = serde_json::json!(last_price);
    metrics["first_price_tick"] = serde_json::json!(first_price);
    metrics["price_change_ppm"] = match first_price.zip(last_price) {
        Some((a, b)) if a > 0 => {
            serde_json::json!((i128::from(b) - i128::from(a)) * 1000000 / i128::from(a))
        }
        _ => serde_json::Value::Null,
    };
    metrics["liquidation_count"] = liquidations.count.into();
    metrics["liquidation_notional"] = liquidations.quote.to_string().into();
    metrics["long_liquidation_notional"] = liquidations.sell_quote.to_string().into();
    metrics["short_liquidation_notional"] = liquidations.buy_quote.to_string().into();
    metrics["liquidation_volume_qty"] = liquidations
        .buy
        .checked_add(liquidations.sell)
        .ok_or_else(overflow)?
        .to_string()
        .into();
    metrics["large_trade_count"] = large.count.into();
    metrics["large_trade_notional"] = large.quote.to_string().into();
    metrics["funding_receipt_count"] = funding_count.into();
    metrics["funding_settled_count"] = settled_count.into();
    metrics["funding_skipped_count"] = skipped_count.into();
    metrics["funding_total_transfer"] = transfer.to_string().into();
    metrics["last_funding_rate_ppm"] = serde_json::json!(last_rate);
    let book_value = if end == now {
        let mut bid_qty = 0;
        let mut ask_qty = 0;
        let mut bid_quote = 0;
        let mut ask_quote = 0;
        for level in book.bids.iter().take(levels) {
            add(&mut bid_qty, i128::from(level.qty))?;
            add(
                &mut bid_quote,
                i128::from(level.qty) * i128::from(level.price_tick),
            )?;
        }
        for level in book.asks.iter().take(levels) {
            add(&mut ask_qty, i128::from(level.qty))?;
            add(
                &mut ask_quote,
                i128::from(level.qty) * i128::from(level.price_tick),
            )?;
        }
        let total = bid_qty.checked_add(ask_qty).ok_or_else(overflow)?;
        metrics["book_imbalance_ppm"] = if total == 0 {
            serde_json::Value::Null
        } else {
            serde_json::json!(
                (bid_qty - ask_qty)
                    .checked_mul(1000000)
                    .ok_or_else(overflow)?
                    / total
            )
        };
        metrics["book_bid_qty"] = bid_qty.to_string().into();
        metrics["book_ask_qty"] = ask_qty.to_string().into();
        serde_json::json!({"market_time_ms":now,"through_command_seq":head,"levels":levels,
            "best_bid_tick":book.bids.first().map(|l|l.price_tick),"best_ask_tick":book.asks.first().map(|l|l.price_tick),
            "bid_qty":bid_qty.to_string(),"ask_qty":ask_qty.to_string(),"bid_notional":bid_quote.to_string(),"ask_notional":ask_quote.to_string()})
    } else {
        serde_json::Value::Null
    };
    let mut result = serde_json::json!({"room_id":room,"instrument_id":instrument,"status":status,
        "market_time_ms":now,"start_market_time_ms":start,"end_market_time_ms":end,"window_ms":window_ms,
        "through_command_seq":head,"scanned_commands":page.executions.len(),"scan_limit":limit,
        "complete":!page.truncated && !page.missing_timestamps,"truncated":page.truncated,"missing_timestamps":page.missing_timestamps,
        "metrics":metrics,"book":book_value,"large_trade_min_notional":large_min.to_string(),
        "units":"quantities in native lots; amounts in native quote units; wide integers as decimal strings; VWAP is an exact rational",
        "window_bounds":"inclusive simulation time; CVD starts at zero at the window boundary"});
    if detailed {
        let mut cvd = 0;
        let mut rows = Vec::new();
        for (time, bucket) in buckets {
            let mut row = bucket.json()?;
            add(&mut cvd, bucket.buy - bucket.sell)?;
            row["open_market_time_ms"] = time.into();
            row["cvd_qty"] = cvd.to_string().into();
            rows.push(row);
        }
        let mut prices = Vec::new();
        for (price, volume) in profile {
            let mut row = volume.json()?;
            row["price_from_tick"] = price.into();
            prices.push(row);
        }
        result["buckets"] = rows.into();
        result["bucket_ms"] = bucket_ms.into();
        result["volume_profile"] = prices.into();
        result["profile_truncated"] = profile_truncated.into();
        result["price_group_tick"] = group.into();
    }
    Ok(Json(result))
}
