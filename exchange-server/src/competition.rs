//! One authoritative match per room; the room journal supplies market/account truth.
use super::*;
use crate::platform::{self, now_ms};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Preparation,
    Countdown,
    Starting,
    Running,
    Finishing,
    Finished,
    Aborted,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub account_id: AccountId,
    pub display_name: String,
    pub ready: bool,
    pub initial_equity: String,
}
impl Player {
    pub fn new(account_id: AccountId, display_name: String) -> Self {
        Self {
            account_id,
            display_name,
            ready: false,
            initial_equity: String::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Competition {
    pub room_id: String,
    pub title: String,
    pub phase: Phase,
    pub seats: Vec<AccountId>,
    pub duration_seconds: u64,
    pub countdown_seconds: u64,
    pub bot_interval_ms: u64,
    pub spectator_accounts: bool,
    pub instrument_id: String,
    pub initial_mark_tick: i64,
    pub starts_at_ms: Option<u64>,
    pub ends_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    #[serde(default)]
    pub settlement_mark_tick: Option<i64>,
    #[serde(default)]
    pub settlement_command_cursor: Option<u64>,
    pub players: BTreeMap<String, Player>,
    pub results: Vec<Standing>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Standing {
    pub rank: usize,
    pub user_id: String,
    pub display_name: String,
    pub account_id: AccountId,
    pub initial_equity: String,
    pub final_equity: String,
    pub pnl: String,
    pub return_ppm: String,
}
pub(super) fn administrative_frozen(app: &AppState, room: &str) -> bool {
    app.platform.competitions.contains_key(room)
}
pub(super) fn guard_management(app: &AppState, room: &str) -> Result<(), ApiError> {
    if administrative_frozen(app, room) {
        Err(api_error(
            StatusCode::CONFLICT,
            "比赛房间已锁定，请使用比赛控制；更改规则需新建房间",
        ))
    } else {
        Ok(())
    }
}
pub(super) fn market_allowed(app: &AppState, room: &str) -> bool {
    app.platform
        .competitions
        .get(room)
        .is_none_or(|c| c.phase == Phase::Running && c.ends_at_ms.is_some_and(|end| now_ms() < end))
}
pub(super) fn guard_order(
    app: &AppState,
    room: &str,
    user: &str,
    account: AccountId,
) -> Result<(), ApiError> {
    if let Some(c) = app.platform.competitions.get(room) {
        if !market_allowed(app, room) {
            return Err(api_error(
                StatusCode::CONFLICT,
                "比赛尚未开始或已截止，不能下单",
            ));
        }
        if !c.players.get(user).is_some_and(|p| p.account_id == account) {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "仅参赛者能操作自己的比赛账户",
            ));
        }
    }
    Ok(())
}
pub(super) fn spectator_can_view_accounts(app: &AppState, room: &str) -> bool {
    app.platform
        .competitions
        .get(room)
        .is_none_or(|c| c.spectator_accounts || c.phase == Phase::Finished)
}
pub(crate) fn validate_invitation(
    app: &AppState,
    room: &str,
    role: &str,
    account: Option<AccountId>,
) -> Result<(), ApiError> {
    if let Some(c) = app.platform.competitions.get(room) {
        if role == "trader"
            && (c.phase != Phase::Preparation
                || !account.is_some_and(|id| {
                    c.seats.contains(&id) && !c.players.values().any(|p| p.account_id == id)
                }))
        {
            return Err(api_error(
                StatusCode::CONFLICT,
                "参赛席位无效、已占用或报名已关闭",
            ));
        }
        if role == "admin" {
            return Err(api_error(
                StatusCode::CONFLICT,
                "比赛创建后不能增加管理权限",
            ));
        }
    } else if let Some(id) = account
        && app.schedulers.get(room).is_some_and(|s| {
            s.agents
                .iter()
                .any(|a| a.template.config().account_id == id)
        })
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Bot 账户不能分配给参赛者",
        ));
    }
    Ok(())
}
fn mark(
    app: &AppState,
    room: &str,
    instrument: &str,
    fallback: Option<i64>,
) -> Result<i64, ApiError> {
    if let Some(price) = app
        .rooms
        .simulation_room(room)
        .map_err(api_error_from_room)?
        .perp_price_snapshot(instrument)
        .map_err(|e| api_error(StatusCode::CONFLICT, format!("{e:?}")))?
    {
        return Ok(price.mark_price_tick);
    }
    let book = app
        .rooms
        .book_snapshot_for(room, instrument)
        .map_err(api_error_from_room)?;
    if let (Some(bid), Some(ask)) = (book.bids.first(), book.asks.first()) {
        let mid = ((i128::from(bid.price_tick) + i128::from(ask.price_tick)) / 2) as i64;
        if mid > 0 {
            return Ok(mid);
        }
    }
    let ticker = app
        .rooms
        .ticker(room, instrument)
        .map_err(api_error_from_room)?;
    ticker
        .mid_tick
        .or(ticker.last_trade_tick)
        .or(fallback)
        .filter(|p| *p > 0)
        .ok_or_else(|| api_error(StatusCode::CONFLICT, "市场尚无有效参考价格"))
}
fn equities(
    app: &AppState,
    c: &Competition,
    ids: &[AccountId],
) -> Result<BTreeMap<AccountId, (i128, i128, i128)>, ApiError> {
    let snapshot = app
        .rooms
        .account_snapshots_for(&c.room_id, &c.instrument_id)
        .map_err(api_error_from_room)?;
    let quote = mark(app, &c.room_id, &c.instrument_id, Some(c.initial_mark_tick))?;
    let mut values = BTreeMap::new();
    match snapshot {
        AccountSnapshots::Spot(accounts) => {
            for a in accounts.into_iter().filter(|a| ids.contains(&a.account_id)) {
                let equity = a
                    .position_qty
                    .checked_mul(i128::from(quote))
                    .and_then(|value| value.checked_add(a.cash_balance))
                    .ok_or_else(|| api_error(StatusCode::CONFLICT, "账户估值超出范围"))?;
                values.insert(a.account_id, (equity, a.cash_balance, a.position_qty));
            }
        }
        AccountSnapshots::Perp(accounts) => {
            for a in accounts.into_iter().filter(|a| ids.contains(&a.account_id)) {
                values.insert(a.account_id, (a.equity, a.cash_balance, a.position_qty));
            }
        }
    }
    if values.len() != ids.len() {
        return Err(api_error(StatusCode::BAD_REQUEST, "存在未配置的参赛账户"));
    }
    Ok(values)
}
fn validate_seats(
    app: &AppState,
    c: &Competition,
) -> Result<BTreeMap<AccountId, (i128, i128, i128)>, ApiError> {
    let values = equities(app, c, &c.seats)?;
    let snapshots = app
        .rooms
        .account_snapshots_for(&c.room_id, &c.instrument_id)
        .map_err(api_error_from_room)?;
    let equal = match snapshots {
        AccountSnapshots::Spot(a) => {
            let accounts = a
                .into_iter()
                .filter(|a| c.seats.contains(&a.account_id))
                .map(|mut a| {
                    a.account_id = 0;
                    a
                })
                .collect::<Vec<_>>();
            accounts.iter().all(|a| a == &accounts[0])
        }
        AccountSnapshots::Perp(a) => {
            let accounts = a
                .into_iter()
                .filter(|a| c.seats.contains(&a.account_id))
                .map(|mut a| {
                    a.account_id = 0;
                    a
                })
                .collect::<Vec<_>>();
            accounts.iter().all(|a| a == &accounts[0])
        }
    };
    if !equal {
        return Err(api_error(
            StatusCode::CONFLICT,
            "参赛账户的初始持仓、费用和保证金状态必须一致",
        ));
    }
    let first = values.values().next().unwrap();
    if first.0 <= 0 || values.values().any(|v| v != first) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "所有参赛账户必须具有相同且为正的初始权益、资金和持仓",
        ));
    }
    let worth = app
        .rooms
        .net_worth_snapshot(&c.room_id)
        .map_err(api_error_from_room)?;
    if worth
        .accounts
        .iter()
        .filter(|a| c.seats.contains(&a.account_id))
        .any(|a| {
            a.assets
                .iter()
                .any(|b| b.portfolio_total != 0 || b.portfolio_reserved != 0)
        })
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "当前比赛评分只支持单一市场的场内账户，请先清空场外资产",
        ));
    }
    for id in &c.seats {
        if app.schedulers.get(&c.room_id).is_some_and(|s| {
            s.agents
                .iter()
                .any(|a| a.template.config().account_id == *id)
        }) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "参赛账户不能被 Bot 使用",
            ));
        }
        if !app
            .rooms
            .resting_orders_for_account(&c.room_id, &c.instrument_id, *id)
            .map_err(api_error_from_room)?
            .is_empty()
        {
            return Err(api_error(
                StatusCode::CONFLICT,
                "参赛账户开始前必须没有挂单",
            ));
        }
    }
    Ok(values)
}
pub(super) fn view(app: &AppState, room: &str, user: &str, admin: bool) -> serde_json::Value {
    let Some(c) = app.platform.competitions.get(room) else {
        return serde_json::Value::Null;
    };
    let mut value = serde_json::to_value(c).unwrap();
    // Initial equity is common across players; current balances remain in role-scoped views.
    value["server_time_ms"] = now_ms().into();
    value["scoring_version"] = "equity-return.v1".into();
    value["scoring_rule"]="单市场净权益收益率；现货按收盘盘口中价估值，无双边盘口时用最后成交价，再回退开赛参考价；永续用清算引擎标记价权益；手续费已扣除；同收益并列".into();
    value["my_account_id"] = c
        .players
        .get(user)
        .map(|p| serde_json::json!(p.account_id))
        .unwrap_or(serde_json::Value::Null);
    value["can_manage"] = admin.into();
    value
}
#[derive(Deserialize)]
pub(super) struct Setup {
    title: String,
    seats: Vec<AccountId>,
    duration_seconds: u64,
    #[serde(default = "countdown_default")]
    countdown_seconds: u64,
    #[serde(default = "interval_default")]
    bot_interval_ms: u64,
    #[serde(default)]
    spectator_accounts: bool,
}
fn countdown_default() -> u64 {
    5
}
fn interval_default() -> u64 {
    1000
}
pub(super) async fn setup(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
    Json(request): Json<Setup>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(&state, &headers, &room, RoomReadAccess::Admin).await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        platform::ensure_admin(&app, &headers, &room).await?;
        if !app.auth_policy.is_accounts() {
            return Err(api_error(StatusCode::CONFLICT, "多人比赛需要启用账号登录"));
        }
        if app.platform.competitions.contains_key(&room) {
            return Err(api_error(
                StatusCode::CONFLICT,
                "房间已经有比赛，请为下一场比赛新建房间",
            ));
        }
        if !(10..=86400).contains(&request.duration_seconds)
            || !(3..=60).contains(&request.countdown_seconds)
            || !(50..=60000).contains(&request.bot_interval_ms)
            || !(2..=64).contains(&request.seats.len())
            || request.title.trim().is_empty()
            || request.title.chars().count() > 100
            || request.seats.contains(&0)
            || request.seats.iter().collect::<BTreeSet<_>>().len() != request.seats.len()
        {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "需要 2–64 个不同账户、10–86400 秒赛程和 3–60 秒倒计时",
            ));
        }
        let instruments = app
            .rooms
            .simulation_room(&room)
            .map_err(api_error_from_room)?
            .instrument_ids();
        if instruments.len() != 1 {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "当前比赛评分支持单一市场，请使用单市场比赛房间",
            ));
        }
        if app.rooms.status(&room).map_err(api_error_from_room)? != MarketStatus::Paused {
            return Err(api_error(StatusCode::CONFLICT, "请先暂停市场，再设置比赛"));
        }
        if app.training_runs.values().any(|r| r.spec.room_id == room) {
            return Err(api_error(
                StatusCode::CONFLICT,
                "训练房间不能同时运行多人比赛",
            ));
        }
        let mut c = Competition {
            room_id: room.clone(),
            title: request.title.trim().into(),
            phase: Phase::Preparation,
            seats: request.seats,
            duration_seconds: request.duration_seconds,
            countdown_seconds: request.countdown_seconds,
            bot_interval_ms: request.bot_interval_ms,
            spectator_accounts: request.spectator_accounts,
            instrument_id: instruments[0].clone(),
            initial_mark_tick: mark(&app, &room, &instruments[0], None)?,
            starts_at_ms: None,
            ends_at_ms: None,
            finished_at_ms: None,
            settlement_mark_tick: None,
            settlement_command_cursor: None,
            players: BTreeMap::new(),
            results: vec![],
            error: None,
        };
        validate_seats(&app, &c)?;
        // Members invited before the match was configured keep their assigned seat.
        for (user, role) in app
            .journal
            .list_room_members(&room)
            .await
            .map_err(api_error_from_journal)?
        {
            if role != "trader" {
                continue;
            }
            let mut assigned = Vec::new();
            for id in &c.seats {
                if app
                    .journal
                    .user_can_access_account(&user, &room, *id)
                    .await
                    .map_err(api_error_from_journal)?
                {
                    assigned.push(*id);
                }
            }
            if assigned.len() > 1 {
                return Err(api_error(
                    StatusCode::CONFLICT,
                    "一个选手不能占用多个比赛席位，请先调整账户分配",
                ));
            }
            if let Some(id) = assigned.first() {
                if c.players.values().any(|p| p.account_id == *id) {
                    return Err(api_error(
                        StatusCode::CONFLICT,
                        "同一比赛账户有多个所有者，请先调整账户分配",
                    ));
                }
                let profile =
                    app.platform.users.get(&user).ok_or_else(|| {
                        api_error(StatusCode::CONFLICT, "比赛成员必须使用注册账号")
                    })?;
                c.players
                    .insert(user, Player::new(*id, profile.display_name.clone()));
            }
        }

        let mut candidate = app.platform.clone();
        candidate.competitions.insert(room.clone(), c);
        platform::commit(&mut app, candidate, vec![]).await?;
        Ok(Json(view(&app, &room, "", true)))
    })
    .await
}
pub(super) async fn get(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<serde_json::Value> {
    let auth = authorize_room_read(&state, &headers, &room, RoomReadAccess::Room).await?;
    let admin = auth
        .journal
        .user_can_administer_room(&auth.user_id, &room)
        .await
        .map_err(api_error_from_journal)?;
    let app = lock_state(&state).await?;
    Ok(Json(view(&app, &room, &auth.user_id, admin)))
}
#[derive(Deserialize)]
pub(super) struct Ready {
    ready: bool,
}
pub(super) async fn ready(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
    Json(request): Json<Ready>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(&state, &headers, &room, RoomReadAccess::Room).await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        let user = current_user_id(&headers, &app.auth_policy)?;
        let mut candidate = app.platform.clone();
        let c = candidate
            .competitions
            .get_mut(&room)
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "没有比赛"))?;
        if c.phase != Phase::Preparation {
            return Err(api_error(StatusCode::CONFLICT, "比赛已经锁定准备状态"));
        }
        c.players
            .get_mut(&user)
            .ok_or_else(|| api_error(StatusCode::FORBIDDEN, "只有参赛者可以准备"))?
            .ready = request.ready;
        platform::commit(&mut app, candidate, vec![]).await?;
        Ok(Json(view(&app, &room, &user, false)))
    })
    .await
}
pub(super) async fn start(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(&state, &headers, &room, RoomReadAccess::Admin).await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        platform::ensure_admin(&app, &headers, &room).await?;
        let mut candidate = app.platform.clone();
        let c = candidate
            .competitions
            .get_mut(&room)
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "没有比赛"))?;
        if c.phase == Phase::Countdown || c.phase == Phase::Running {
            return Ok(Json(view(&app, &room, "", true)));
        }
        if c.phase != Phase::Preparation
            || c.players.len() != c.seats.len()
            || c.players.values().any(|p| !p.ready)
        {
            return Err(api_error(
                StatusCode::CONFLICT,
                "所有席位必须加入并准备完毕",
            ));
        }
        if app.rooms.status(&room).map_err(api_error_from_room)? != MarketStatus::Paused {
            return Err(api_error(StatusCode::CONFLICT, "市场必须暂停"));
        }
        let values = validate_seats(&app, c)?;
        for p in c.players.values_mut() {
            p.initial_equity = values[&p.account_id].0.to_string();
        }
        let begins = now_ms() + c.countdown_seconds * 1000;
        c.starts_at_ms = Some(begins);
        c.ends_at_ms = Some(begins + c.duration_seconds * 1000);
        c.phase = Phase::Countdown;
        platform::commit(&mut app, candidate, vec![]).await?;
        Ok(Json(view(&app, &room, "", true)))
    })
    .await
}
pub(super) async fn abort(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(&state, &headers, &room, RoomReadAccess::Admin).await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        platform::ensure_admin(&app, &headers, &room).await?;
        let mut candidate = app.platform.clone();
        let c = candidate
            .competitions
            .get_mut(&room)
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "没有比赛"))?;
        if matches!(c.phase, Phase::Finished | Phase::Finishing) {
            return Err(api_error(
                StatusCode::CONFLICT,
                "截止后的比赛不能作废或重算",
            ));
        }
        c.phase = Phase::Aborted;
        c.finished_at_ms = Some(now_ms());
        c.error = Some("管理员终止比赛，不生成正式排名".into());
        platform::commit(&mut app, candidate, vec![]).await?;
        set_status(&mut app, &room, MarketStatus::Closed).await?;
        Ok(Json(view(&app, &room, "", true)))
    })
    .await
}
async fn set_status(app: &mut AppState, room: &str, status: MarketStatus) -> Result<(), ApiError> {
    if app.rooms.status(room).map_err(api_error_from_room)? == status {
        return Ok(());
    }
    let mut rooms = app.rooms.clone();
    match status {
        MarketStatus::Running => rooms.resume_room(room),
        MarketStatus::Paused => rooms.pause_room(room),
        MarketStatus::Closed => rooms.close_room(room),
    }
    .map_err(api_error_from_room)?;
    let cursor = next_persisted_command_cursor(app, room).map_err(api_error_from_journal)?;
    app.append_room_mutation(
        &PendingJournalMutation::new(room, cursor, RoomMutation::StatusChanged { status }),
        &[],
        &[],
        None,
    )
    .await
    .map_err(api_error_from_journal)?;
    if let Some(worker) = app.agent_workers.get(room) {
        worker.control.epoch.fetch_add(1, Ordering::AcqRel);
        if status == MarketStatus::Closed {
            worker.request_stop();
        }
    }
    app.rooms = rooms;
    Ok(())
}
fn standings(app: &AppState, c: &Competition) -> Result<Vec<Standing>, ApiError> {
    let values = equities(app, c, &c.seats)?;
    let mut rows = Vec::new();
    for (user, p) in &c.players {
        let initial = p
            .initial_equity
            .parse::<i128>()
            .map_err(|_| api_error(StatusCode::CONFLICT, "初始权益记录无效"))?;
        let final_value = values[&p.account_id].0;
        let pnl = final_value
            .checked_sub(initial)
            .ok_or_else(|| api_error(StatusCode::CONFLICT, "收益溢出"))?;
        let ppm = pnl
            .checked_mul(1_000_000)
            .and_then(|v| v.checked_div(initial))
            .ok_or_else(|| api_error(StatusCode::CONFLICT, "收益率溢出"))?;
        rows.push((
            pnl,
            Standing {
                rank: 0,
                user_id: user.clone(),
                display_name: p.display_name.clone(),
                account_id: p.account_id,
                initial_equity: initial.to_string(),
                final_equity: final_value.to_string(),
                pnl: pnl.to_string(),
                return_ppm: ppm.to_string(),
            },
        ));
    }
    // Equal initial equity is required. Compare exact integer PnL, not rounded return.
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.user_id.cmp(&b.1.user_id)));
    let mut previous = None;
    let mut rank = 0;
    for (index, (pnl, row)) in rows.iter_mut().enumerate() {
        if previous != Some(*pnl) {
            rank = index + 1;
        }
        row.rank = rank;
        previous = Some(*pnl);
    }
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}
pub(super) async fn tick(state: &SharedState) -> Result<(), ApiError> {
    {
        let app = lock_state(state).await?;
        if !app.platform.competitions.values().any(|c| {
            matches!(
                c.phase,
                Phase::Countdown | Phase::Starting | Phase::Running | Phase::Finishing
            ) || (c.phase == Phase::Aborted
                && app.rooms.status(&c.room_id) != Ok(MarketStatus::Closed))
        }) {
            return Ok(());
        }
    }

    let shared = state.clone();
    run_durable_state_transaction(shared.clone(), async move {
        let mut app = lock_state(&shared).await?;
        let rooms = app
            .platform
            .competitions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for room in rooms {
            let c = app.platform.competitions[&room].clone();
            let now = now_ms();
            if c.phase == Phase::Aborted {
                set_status(&mut app, &room, MarketStatus::Closed).await?;
                continue;
            }
            if matches!(c.phase, Phase::Countdown | Phase::Starting)
                && c.starts_at_ms.is_some_and(|at| now >= at)
                && c.ends_at_ms.is_some_and(|at| now < at)
            {
                let mut candidate = app.platform.clone();
                candidate.competitions.get_mut(&room).unwrap().phase = Phase::Starting;
                platform::commit(&mut app, candidate, vec![]).await?;
                set_status(&mut app, &room, MarketStatus::Running).await?;
                let agents = app
                    .schedulers
                    .get(&room)
                    .map(|s| {
                        s.agents
                            .iter()
                            .map(|a| a.template.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                start_agent_worker_for_room(
                    &shared,
                    &mut app,
                    room.clone(),
                    StartAgentsRequest {
                        agents,
                        interval_ms: Some(c.bot_interval_ms),
                    },
                )
                .await?;
                let mut candidate = app.platform.clone();
                candidate.competitions.get_mut(&room).unwrap().phase = Phase::Running;
                platform::commit(&mut app, candidate, vec![]).await?;
            }
            let c = app.platform.competitions[&room].clone();
            if c.phase == Phase::Finishing
                || (matches!(c.phase, Phase::Countdown | Phase::Starting | Phase::Running)
                    && c.ends_at_ms.is_some_and(|at| now_ms() >= at))
            {
                if c.phase != Phase::Finishing {
                    let mut candidate = app.platform.clone();
                    candidate.competitions.get_mut(&room).unwrap().phase = Phase::Finishing;
                    platform::commit(&mut app, candidate, vec![]).await?;
                }
                set_status(&mut app, &room, MarketStatus::Closed).await?;
                let results = standings(&app, &c)?;
                let settlement_mark =
                    mark(&app, &room, &c.instrument_id, Some(c.initial_mark_tick))?;
                let cursor =
                    next_persisted_command_cursor(&app, &room).map_err(api_error_from_journal)?;
                let mut candidate = app.platform.clone();
                let result = candidate.competitions.get_mut(&room).unwrap();
                result.results = results;
                result.settlement_mark_tick = Some(settlement_mark);
                result.settlement_command_cursor = Some(cursor);
                result.finished_at_ms = Some(now_ms());
                result.phase = Phase::Finished;
                platform::commit(&mut app, candidate, vec![]).await?;
            }
        }
        Ok(Json(()))
    })
    .await
    .map(|_| ())
}
pub(super) async fn run(state: SharedState) {
    let mut shutdown = state.lifecycle.subscribe_shutdown();
    let mut timer = tokio::time::interval(Duration::from_millis(200));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {changed=shutdown.changed()=>{if changed.is_err()||*shutdown.borrow(){return;}},_=timer.tick()=>{if let Err((_,error))=tick(&state).await {eprintln!("competition transition failed: {}",error.error);}}}
    }
}
// Check every room mutation route, including deposits, clock controls and legacy APIs.
// Recheck the authoritative path under its lock for orders and scheduler work.
pub(super) async fn protect(
    State(state): State<SharedState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str())
        .unwrap_or(request.uri().path())
        .to_owned();
    let method = request.method().clone();
    let (mut parts, body) = request.into_parts();
    use axum::extract::FromRequestParts;
    let params = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &()).await;
    if method == Method::POST
        && !path.contains("/competition")
        && !path.ends_with("/invitations")
        && !path.ends_with("/orders")
        && !path.ends_with("/session")
        && let Ok(Path(params)) = params
        && let Some(room) = params.get("room_id")
    {
        let result = match lock_state(&state).await {
            Ok(app) => guard_management(&app, room),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            return error.into_response();
        }
    }
    let mut response = next
        .run(axum::extract::Request::from_parts(parts, body))
        .await;
    if path.starts_with("/auth/") || path.ends_with("/invitations") {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod tests;
