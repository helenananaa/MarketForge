//! Public journal history. Projection is shared with streams; private receipts
//! and rejected/unfilled liquidation attempts never become public events.
use super::*;

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Liquidation,
    Funding,
    PriceUpdate,
    Trade,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Args {
    after_command_seq: Option<u64>,
    #[serde(default)]
    from_start: bool,
    limit: Option<usize>,
    event_type: Option<Kind>,
    start_market_time_ms: Option<u64>,
    end_market_time_ms: Option<u64>,
    min_notional: Option<String>,
    side: Option<exchange_core::Side>,
    min_rate_ppm: Option<i32>,
    max_rate_ppm: Option<i32>,
}

impl Args {
    fn validate(&self) -> Result<Option<i128>, ApiError> {
        let invalid = |message| api_error(StatusCode::BAD_REQUEST, message);
        if self.from_start && self.after_command_seq.is_some() {
            return Err(invalid("conflicting history cursors"));
        }
        if self.limit.is_some_and(|limit| !(1..=500).contains(&limit)) {
            return Err(invalid("limit must be 1-500 scanned commands"));
        }
        if self
            .start_market_time_ms
            .zip(self.end_market_time_ms)
            .is_some_and(|(a, b)| a > b)
        {
            return Err(invalid("invalid market time range"));
        }
        if (self.min_notional.is_some() || self.side.is_some())
            && self.event_type != Some(Kind::Liquidation)
        {
            return Err(invalid(
                "notional/side filters require event_type=liquidation",
            ));
        }
        if (self.min_rate_ppm.is_some() || self.max_rate_ppm.is_some())
            && self.event_type != Some(Kind::Funding)
        {
            return Err(invalid("rate filters require event_type=funding"));
        }
        if self
            .min_rate_ppm
            .zip(self.max_rate_ppm)
            .is_some_and(|(a, b)| a > b)
        {
            return Err(invalid("invalid signed funding rate range"));
        }
        self.min_notional
            .as_ref()
            .map(|value| {
                value
                    .parse::<i128>()
                    .ok()
                    .filter(|n| *n >= 0)
                    .ok_or_else(|| {
                        invalid("min_notional must be a nonnegative i128 decimal integer")
                    })
            })
            .transpose()
    }
}

pub(super) fn project(e: &RoomExecutionSummary, instrument: &str) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    if e.instrument_id.as_deref() == Some(instrument) {
        for mut event in stream_output::public_liquidations(e) {
            // price_tick is already in quote units; tick_size is an increment.
            // Encode wide amounts as decimal strings for lossless MCP clients.
            let notional = i128::from(event["price_tick"].as_i64().unwrap())
                * i128::from(event["qty"].as_u64().unwrap());
            event["notional"] = notional.to_string().into();
            events.push(event);
        }
        events.extend(stream_output::public_trades(e));
    }
    if let Some(settlement) = &e.funding_settlement
        && settlement.instrument_id == instrument
    {
        let mut event = serde_json::to_value(settlement).expect("public funding receipt");
        event["type"] = "FundingSettlement".into();
        event["total_transfer"] = settlement.total_transfer.to_string().into();
        events.push(event);
    }
    for price in &e.price_updates {
        if price.instrument_id == instrument {
            let mut event = serde_json::to_value(price).expect("public price snapshot");
            event["type"] = "PerpPriceUpdated".into();
            if let Some(last) = price
                .funding
                .as_ref()
                .and_then(|f| f.last_settlement.as_ref())
            {
                event["funding"]["last_settlement"]["total_transfer"] =
                    last.total_transfer.to_string().into();
            }
            events.push(event);
        }
    }
    events
}

fn matches(event: &serde_json::Value, q: &Args, min_notional: Option<i128>) -> bool {
    let kind = match event["type"].as_str() {
        Some("LiquidationTrade") => Kind::Liquidation,
        Some("FundingSettlement") => Kind::Funding,
        Some("PerpPriceUpdated") => Kind::PriceUpdate,
        Some("TradePrinted") => Kind::Trade,
        _ => return false,
    };
    if q.event_type.is_some_and(|filter| filter != kind) {
        return false;
    }
    if min_notional.is_some_and(|min| {
        event["notional"]
            .as_str()
            .and_then(|s| s.parse::<i128>().ok())
            .is_none_or(|n| n < min)
    }) {
        return false;
    }
    if let Some(side) = q.side
        && event["side"] != serde_json::to_value(side).unwrap()
    {
        return false;
    }
    if q.min_rate_ppm.is_some_and(|min| {
        event["rate_ppm"]
            .as_i64()
            .is_none_or(|n| n < i64::from(min))
    }) || q.max_rate_ppm.is_some_and(|max| {
        event["rate_ppm"]
            .as_i64()
            .is_none_or(|n| n > i64::from(max))
    }) {
        return false;
    }
    true
}

pub(super) async fn read(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, instrument)): Path<(String, String)>,
    Query(q): Query<Args>,
) -> ApiResult<serde_json::Value> {
    let min_notional = q.validate()?;
    let auth = authorize_room_read(&state, &headers, &room, RoomReadAccess::Room).await?;
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
            q.limit.unwrap_or(100),
        )
        .await
        .map_err(api_error_from_journal)?;
    validate_event_cursor(&room, q.after_command_seq, page.latest_command_seq)?;
    let mut events = Vec::new();
    for e in &page.executions {
        if (q.start_market_time_ms.is_some() || q.end_market_time_ms.is_some())
            && e.market_time_ms.is_none_or(|time| {
                q.start_market_time_ms.is_some_and(|start| time < start)
                    || q.end_market_time_ms.is_some_and(|end| time > end)
            })
        {
            continue;
        }
        for (index, event) in project(e, &instrument).into_iter().enumerate() {
            if matches(&event, &q, min_notional) {
                events.push(
                    serde_json::json!({"event_id":format!("{}:{index}", e.command_seq),
                    "command_seq":e.command_seq,"market_time_ms":e.market_time_ms,"event":event}),
                );
            }
        }
    }
    let next = page
        .executions
        .last()
        .map(|e| e.command_seq)
        .or(q.after_command_seq);
    Ok(Json(
        serde_json::json!({"room_id":room,"instrument_id":instrument,"events":events,
        "next_after_command_seq":next,"latest_command_seq":page.latest_command_seq,"has_more":page.has_more,
        "limit_scope":"scanned room commands; empty filtered pages still advance",
        "notional_unit":"native quote units (price_tick * qty), per actual liquidation fill"}),
    ))
}
