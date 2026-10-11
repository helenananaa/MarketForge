//! Ordinary HTTP orders may prepare independently by venue. Only the small
//! merge/journal/publication phase owns the room commit mutex. Coordination
//! barriers and fallback keep room-wide effects on the existing transaction path.
use super::*;

pub(super) async fn submit(
    shared: &SharedState,
    headers: &HeaderMap,
    room: &str,
    instrument: Option<&str>,
    request: &SubmitOrderRequest,
) -> Result<Option<OrderResponse>, ApiError> {
    if matches!(
        request.action,
        OrderAction::SetConditional { .. }
            | OrderAction::SetPositionProtection { .. }
            | OrderAction::PlaceBracket { .. }
    ) {
        return Ok(None);
    }
    let mut profile = write_profile::Profile::new("venue_order");
    profile.field("room", room);
    profile.field("allocates_id", order_action_allocates_id(&request.action));
    let instrument = instrument.or(request.instrument_id.as_deref());
    let Some(owner) = shared.app.venue_order(room, instrument).await else {
        return Ok(None);
    };
    profile.mark("venue_wait");
    let Some(mut candidate) = owner
        .predecessor
        .fork_venue_order(room, &owner.instrument)
        .map_err(api_error_from_room)?
    else {
        return Ok(None);
    };
    #[cfg(feature = "bench-execution-timing")]
    let prepare_started = Instant::now();
    let metadata = owner.metadata();
    let user = current_user_id(headers, &metadata.auth_policy)?;
    competition::guard_order(metadata, room, &user, request.account_id)?;
    // Directory ownership keeps membership and platform changes fenced. This
    // I/O does not occupy the room commit mutex.
    let admin = metadata
        .journal
        .user_can_administer_room(&user, room)
        .await
        .map_err(api_error_from_journal)?;
    let key = request_idempotency_key(headers)?;
    let fingerprint = serde_json::to_string(&(instrument, request)).map_err(|error| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to fingerprint order request: {error}"),
        )
    })?;
    let mut ids =
        metadata.reserve_order_ids(u64::from(order_action_allocates_id(&request.action)))?;
    let mut gateway = OrderGateway::new(&mut candidate, ids.start);
    profile.mark("prepare");
    #[cfg(feature = "bench-execution-timing")]
    bench_execution_timing::ORDER_PREPARE.record(prepare_started);
    #[cfg(feature = "bench-execution-timing")]
    let apply_started = Instant::now();
    let execution = gateway.submit_action(GatewayRequest {
        participant_id: request.participant_id.clone(),
        room_id: room.to_owned(),
        instrument_id: Some(owner.instrument.clone()),
        account_id: request.account_id,
        action: request.action.clone(),
    });
    let next_id = gateway.next_order_id();
    profile.mark("apply");
    #[cfg(feature = "bench-execution-timing")]
    {
        bench_execution_timing::ORDER_APPLY.record(apply_started);
        bench_execution_timing::VENUE_COMPUTE.record(apply_started);
    }

    let mut app = owner.commit_lock().await;
    profile.mark("room_commit_wait");
    // Competition deadlines use wall time, so check them again after queuing.
    competition::guard_order(&app, room, &user, request.account_id)?;
    if let Some(key) = key.as_deref()
        && let Some(existing) = app
            .journal
            .find_idempotent_execution(&user, room, key)
            .await
            .map_err(api_error_from_journal)?
    {
        if existing.request_fingerprint.as_deref() != Some(fingerprint.as_str()) {
            return Err(api_error(
                StatusCode::CONFLICT,
                format!("idempotency key {key:?} was already used for a different order request"),
            ));
        }
        return Ok(Some(OrderResponse::from_summary(
            request.participant_id.clone(),
            request.account_id,
            request.action.clone(),
            existing.execution,
        )));
    }
    let step = if admin {
        None
    } else {
        let step = app.rooms.clock(room).map_err(api_error_from_room)?.step();
        let count = app
            .journal
            .external_action_count(&user, room, step)
            .await
            .map_err(api_error_from_journal)?;
        if count >= EXTERNAL_ACTIONS_PER_STEP {
            return Err(api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "external action quota exceeded for this simulation step",
            ));
        }
        Some(step)
    };
    // Replay takes precedence over a speculative error (for example retrying a
    // cancel whose order is already gone). Only now can the candidate fail.
    let mut execution = execution.map_err(|error| api_error_from_room(error.into_room_error()))?;
    profile.mark("admission");
    #[cfg(feature = "bench-execution-timing")]
    let receipt_started = Instant::now();
    let mut merged = app.rooms.clone();
    let previous_len = merged
        .execution_history_len(room)
        .map_err(api_error_from_room)?;
    if !merged
        .merge_venue_order(&owner.predecessor, candidate, &mut execution.execution)
        .map_err(api_error_from_room)?
    {
        #[cfg(feature = "bench-execution-timing")]
        bench_execution_timing::VENUE_FALLBACK.record(receipt_started);
        // Nothing was journaled or installed. Drop both guards before the
        // caller enters the room writer barrier and repeats the full gateway.
        return Ok(None);
    }
    let response = OrderResponse::from_gateway_execution(
        request.participant_id.clone(),
        request.account_id,
        request.action.clone(),
        execution.execution.clone(),
    );
    let mut record = JournalExecution::submitted(
        execution.participant_id,
        execution.account_id,
        execution.command,
        execution.execution,
    );
    if let Some(key) = key {
        record = record.with_idempotency(user.clone(), key, fingerprint);
    }
    if let Some(step) = step {
        record = record.with_quota(user, step);
    }
    profile.mark("receipt");
    #[cfg(feature = "bench-execution-timing")]
    bench_execution_timing::ORDER_RECEIPT.record(receipt_started);
    #[cfg(feature = "bench-execution-timing")]
    let journal_started = Instant::now();
    journal_new_executions(&mut app, &merged, room, previous_len, record)
        .await
        .map_err(api_error_from_journal)?;
    profile.mark("journal");
    #[cfg(feature = "bench-execution-timing")]
    bench_execution_timing::ORDER_JOURNAL.record(journal_started);
    #[cfg(feature = "bench-execution-timing")]
    let publish_started = Instant::now();
    ids.commit(next_id);
    app.next_order_id = app.next_order_id.max(next_id);
    app.rooms = merged;
    profile.mark("install");
    #[cfg(feature = "bench-execution-timing")]
    {
        bench_execution_timing::ORDER_PUBLISH.record(publish_started);
        bench_execution_timing::VENUE_COMMIT.record(publish_started);
    }
    Ok(Some(response))
}
