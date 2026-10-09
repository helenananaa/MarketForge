//! Room entry and role-scoped current views. Write authorization stays in the durable gateway.
use super::*;

pub(super) async fn identity(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<serde_json::Value> {
    let app = lock_state(&state).await?;
    let user = current_user_id(&headers, &app.auth_policy)?;
    let profile = app.platform.users.get(&user);
    Ok(Json(
        serde_json::json!({"user_id":user,"auth_mode":app.auth_policy.mode(),"display_name":profile.map(|p|&p.display_name),"username":profile.map(|p|&p.username)}),
    ))
}

pub(super) fn public_observation(observation: &mut ParticipantObservation, account_id: AccountId) {
    if account_id == 0 {
        observation.own_account = None;
        observation.own_orders.clear();
        observation.bot_market_data = None;
        for related in &mut observation.related_markets {
            public_observation(related, 0);
        }
    }
}

#[derive(Clone, Serialize)]
struct Capabilities {
    read_all_accounts: bool,
    trade: bool,
    manage_bots: bool,
    manage_members: bool,
    control_room: bool,
}
#[derive(Clone, Serialize)]
pub(super) struct RoomContext {
    room_id: String,
    user_id: String,
    display_name: String,
    role: String,
    capabilities: Capabilities,
    visible_account_ids: Vec<AccountId>,
    trade_account_ids: Vec<AccountId>,
    instruments: Vec<String>,
}
fn account_ids(accounts: &AccountSnapshots) -> Vec<AccountId> {
    match accounts {
        AccountSnapshots::Spot(accounts) => accounts.iter().map(|a| a.account_id).collect(),
        AccountSnapshots::Perp(accounts) => accounts.iter().map(|a| a.account_id).collect(),
    }
}
async fn resolve(
    state: &SharedState,
    headers: &HeaderMap,
    room: &str,
) -> Result<RoomContext, ApiError> {
    let auth = authorize_room_read(state, headers, room, RoomReadAccess::Room).await?;
    let role = auth
        .journal
        .user_room_role(&auth.user_id, room)
        .await
        .map_err(api_error_from_journal)?
        .ok_or_else(|| api_error(StatusCode::FORBIDDEN, "Room membership is required"))?;
    let admin = matches!(role.as_str(), "owner" | "admin");
    let all = admin
        || (role == "spectator"
            && competition::spectator_can_view_accounts(&*lock_state(state).await?, room));
    let (instruments, ids) = {
        let app = lock_state(state).await?;
        let instruments = app
            .rooms
            .simulation_room(room)
            .map_err(api_error_from_room)?
            .instrument_ids();
        let mut ids = BTreeSet::new();
        for instrument in &instruments {
            ids.extend(account_ids(
                &app.rooms
                    .account_snapshots_for(room, instrument)
                    .map_err(api_error_from_room)?,
            ));
        }
        (instruments, ids)
    };
    let mut visible = Vec::new();
    let mut trade = Vec::new();
    for id in ids {
        // Zero is reserved for the public observation in the workbench protocol.
        if id == 0 {
            continue;
        }
        let owned = auth
            .journal
            .user_can_access_account(&auth.user_id, room, id)
            .await
            .map_err(api_error_from_journal)?;
        if all || owned {
            visible.push(id);
        }
        let app = lock_state(state).await?;
        if owned && competition::guard_order(&app, room, &auth.user_id, id).is_ok() {
            trade.push(id);
        }
    }
    let display_name = lock_state(state)
        .await?
        .platform
        .users
        .get(&auth.user_id)
        .map(|u| u.display_name.clone())
        .unwrap_or_else(|| auth.user_id.clone());
    Ok(RoomContext {
        room_id: room.into(),
        user_id: auth.user_id,
        display_name,
        role,
        capabilities: Capabilities {
            read_all_accounts: all,
            trade: !trade.is_empty(),
            manage_bots: admin,
            manage_members: admin,
            control_room: admin,
        },
        visible_account_ids: visible,
        trade_account_ids: trade,
        instruments,
    })
}
pub(super) async fn context(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<RoomContext> {
    resolve(&state, &headers, &room).await.map(Json)
}
pub(super) async fn members(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<serde_json::Value> {
    let auth = authorize_room_read(&state, &headers, &room, RoomReadAccess::Admin).await?;
    let members = auth
        .journal
        .list_room_members(&room)
        .await
        .map_err(api_error_from_journal)?;
    let app = lock_state(&state).await?;
    let profiles = members
        .keys()
        .filter_map(|id| {
            app.platform.users.get(id).map(|u| {
                (
                    id.clone(),
                    serde_json::json!({"display_name":u.display_name,"username":u.username}),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    Ok(Json(
        serde_json::json!({"members":members,"profiles":profiles}),
    ))
}
pub(super) async fn overview(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<serde_json::Value> {
    let context = resolve(&state, &headers, &room).await?;
    let app = lock_state(&state).await?;
    let mut markets = Vec::new();
    for instrument in &context.instruments {
        let mut accounts = app
            .rooms
            .account_snapshots_for(&room, instrument)
            .map_err(api_error_from_room)?;
        match &mut accounts {
            AccountSnapshots::Spot(accounts) => {
                accounts.retain(|a| context.visible_account_ids.contains(&a.account_id))
            }
            AccountSnapshots::Perp(accounts) => {
                accounts.retain(|a| context.visible_account_ids.contains(&a.account_id))
            }
        }
        let mut orders = Vec::new();
        for id in &context.visible_account_ids {
            orders.extend(
                app.rooms
                    .resting_orders_for_account(&room, instrument, *id)
                    .map_err(api_error_from_room)?,
            );
        }
        markets.push(
            serde_json::json!({"instrument_id":instrument,"accounts":accounts,"orders":orders}),
        );
    }
    // Publish status and templates, never process credentials or private bot runtime state.
    let bots = if context.capabilities.read_all_accounts {
        let status = agent_status_for_room(&app, &room);
        let mut agents = serde_json::to_value(
            app.schedulers
                .get(&room)
                .map(|s| {
                    s.agents
                        .iter()
                        .map(|a| a.template.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
        .map_err(api_error_from_json)?;
        if !context.capabilities.manage_bots {
            redact_credentials(&mut agents);
        }
        Some(
            serde_json::json!({"status":{"lifecycle":status.lifecycle,"running":status.running,
            "market_running":status.market_running,"interval_ms":status.interval_ms},"agents":agents}),
        )
    } else {
        None
    };
    Ok(Json(
        serde_json::json!({"context":context,"markets":markets,"bots":bots,"status":app.rooms.status(&room).map_err(api_error_from_room)?,"competition":competition::view(&app,&room,&context.user_id,matches!(context.role.as_str(),"owner"|"admin"))}),
    ))
}

fn redact_credentials(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                let sensitive = [
                    "token",
                    "api_key",
                    "password",
                    "secret",
                    "secrets",
                    "authorization",
                    "credential",
                    "credentials",
                ];
                if sensitive
                    .iter()
                    .any(|name| key == *name || key.ends_with(&format!("_{name}")))
                {
                    *value = "[redacted]".into();
                } else {
                    redact_credentials(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_credentials(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
