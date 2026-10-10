//! Durable, account-scoped risk event pages; cursor advances over filtered rows.
use super::*;

#[derive(Deserialize)]
pub(super) struct QueryArgs {
    account_id: AccountId,
    after_command_seq: Option<u64>,
    #[serde(default)]
    from_start: bool,
    limit: Option<usize>,
}

pub(super) async fn read(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path((room, instrument)): Path<(String, String)>,
    Query(q): Query<QueryArgs>,
) -> ApiResult<serde_json::Value> {
    if q.from_start && q.after_command_seq.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "from_start and after_command_seq conflict",
        ));
    }
    let auth = authorize_room_read(
        &state,
        &headers,
        &room,
        RoomReadAccess::Account(q.account_id),
    )
    .await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let page = auth
        .journal
        .query_executions(&room, q.after_command_seq, q.from_start, limit)
        .await
        .map_err(api_error_from_journal)?;
    validate_event_cursor(&room, q.after_command_seq, page.latest_command_seq)?;
    let mut events = Vec::new();
    for e in &page.executions {
        if e.instrument_id.as_deref() != Some(&instrument) {
            continue;
        }
        for c in &e.clearing_events {
            let id = match c {
                ClearingEventSummary::PerpMarginStatusChanged { account_id, .. }
                | ClearingEventSummary::PerpLiquidationSettled { account_id, .. }
                | ClearingEventSummary::PerpFundingSettled { account_id, .. } => *account_id,
                _ => continue,
            };
            if id != q.account_id {
                continue;
            }
            let mut data = serde_json::to_value(c)
                .map_err(|err| api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
            // Loss-sharing counterpart allocations are unrelated private accounts.
            if let Some(o) = data.as_object_mut() {
                o.remove("auto_deleveraging_allocations");
                o.remove("socialized_loss_allocations");
            }
            events.push(serde_json::json!({"command_seq":e.command_seq,"market_time_ms":e.market_time_ms,"event":data}));
        }
    }
    let next = page
        .executions
        .last()
        .map(|e| e.command_seq)
        .or(q.after_command_seq);
    Ok(Json(
        serde_json::json!({"room_id":room,"instrument_id":instrument,"account_id":q.account_id,"events":events,"next_after_command_seq":next,"latest_command_seq":page.latest_command_seq,"has_more":page.has_more}),
    ))
}
