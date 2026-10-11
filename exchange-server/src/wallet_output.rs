//! Durable transfer wallet history, reconstructed with the room on recovery.
use super::*;

#[derive(Deserialize)]
pub(super) struct QueryArgs {
    #[serde(default)]
    wallet_cursor: u64,
    limit: Option<usize>,
}

pub(super) async fn page(
    shared: &SharedState,
    journal: &JournalCoordinator,
    room: &str,
    user: &str,
    cursor: u64,
    limit: usize,
) -> Result<serde_json::Value, ApiError> {
    if !journal
        .user_can_access_room(user, room)
        .await
        .map_err(api_error_from_journal)?
    {
        return Err(api_error(StatusCode::FORBIDDEN, "room access revoked"));
    }
    let app = shared.app.read_room(room).await;
    let simulation = app
        .rooms
        .simulation_room(room)
        .map_err(api_error_from_room)?;
    let latest = simulation.wallet_event_count() as u64;
    if cursor > latest {
        return Err(api_error(
            StatusCode::CONFLICT,
            "wallet cursor is ahead of room",
        ));
    }
    let raw = simulation
        .wallet_events_from(cursor as usize)
        .take(limit.clamp(1, 500))
        .cloned()
        .collect::<Vec<_>>();
    let next = raw.last().map_or(cursor, |event| event.wallet_seq + 1);
    drop(app);
    let mut permissions = BTreeMap::new();
    let mut events = Vec::new();
    for event in raw {
        let allowed = match permissions.get(&event.account_id) {
            Some(allowed) => *allowed,
            None => {
                let allowed = journal
                    .user_can_access_account(user, room, event.account_id)
                    .await
                    .map_err(api_error_from_journal)?;
                permissions.insert(event.account_id, allowed);
                allowed
            }
        };
        if allowed {
            events.push(event);
        }
    }
    Ok(
        serde_json::json!({"room_id":room,"events":events,"next_wallet_cursor":next,"latest_wallet_cursor":latest,"has_more":next < latest}),
    )
}

pub(super) async fn read(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
    Query(q): Query<QueryArgs>,
) -> ApiResult<serde_json::Value> {
    let auth = authorize_room_read(&shared, &headers, &room, RoomReadAccess::Room).await?;
    page(
        &shared,
        &auth.journal,
        &room,
        &auth.user_id,
        q.wallet_cursor,
        q.limit.unwrap_or(100),
    )
    .await
    .map(Json)
}
