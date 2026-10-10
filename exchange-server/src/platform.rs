//! Durable identity and match records. Tokens/passwords are never returned in views.
use super::*;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PlatformData {
    pub revision: u64,
    pub users: BTreeMap<String, User>,
    pub sessions: BTreeMap<String, Session>,
    pub invitations: BTreeMap<String, Invitation>,
    pub competitions: BTreeMap<String, crate::competition::Competition>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub created_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub user_id: String,
    pub expires_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invitation {
    pub room_id: String,
    pub role: String,
    pub account_id: Option<AccountId>,
    pub expires_at_ms: u64,
    pub redeemed_by: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemberGrant {
    pub room_id: String,
    pub user_id: String,
    pub role: String,
    pub account_id: Option<AccountId>,
}
impl PlatformData {
    pub fn records(&self) -> Result<BTreeMap<(String, String), serde_json::Value>, JournalError> {
        let value =
            serde_json::to_value(self).map_err(|e| JournalError::Recovery(e.to_string()))?;
        let mut records = BTreeMap::new();
        for kind in ["users", "sessions", "invitations", "competitions"] {
            for (id, body) in value[kind].as_object().unwrap() {
                records.insert((kind.to_owned(), id.clone()), body.clone());
            }
        }
        Ok(records)
    }
    pub fn from_records(
        revision: u64,
        rows: Vec<(String, String, serde_json::Value)>,
    ) -> Result<Self, JournalError> {
        let mut value = serde_json::json!({"revision":revision,"users":{},"sessions":{},"invitations":{},"competitions":{}});
        for (kind, id, body) in rows {
            let map = value
                .get_mut(&kind)
                .and_then(|v| v.as_object_mut())
                .ok_or_else(|| JournalError::Recovery("invalid platform record kind".into()))?;
            map.insert(id, body);
        }
        serde_json::from_value(value).map_err(|e| JournalError::Recovery(e.to_string()))
    }
}
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
pub(crate) fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub(crate) fn secret() -> Result<String, ApiError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secure randomness unavailable",
        )
    })?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) async fn ensure_admin(
    app: &AppState,
    headers: &HeaderMap,
    room: &str,
) -> Result<String, ApiError> {
    let user = current_user_id(headers, &app.auth_policy)?;
    if !app
        .journal
        .user_can_administer_room(&user, room)
        .await
        .map_err(api_error_from_journal)?
    {
        return Err(api_error(StatusCode::FORBIDDEN, "房间管理权限已失效"));
    }
    Ok(user)
}
pub(crate) async fn commit(
    app: &mut AppState,
    mut candidate: PlatformData,
    grants: Vec<MemberGrant>,
) -> Result<(), ApiError> {
    candidate.revision = app
        .platform
        .revision
        .checked_add(1)
        .ok_or_else(|| api_error(StatusCode::CONFLICT, "platform revision exhausted"))?;
    app.journal
        .commit_platform(app.platform.revision, candidate.clone(), grants)
        .await
        .map_err(api_error_from_journal)?;
    app.auth_policy.install_sessions(&candidate.sessions);
    app.platform = candidate;
    Ok(())
}
pub(super) async fn config(State(state): State<SharedState>) -> ApiResult<serde_json::Value> {
    let app = lock_state(&state).await?;
    Ok(Json(
        serde_json::json!({"mode":app.auth_policy.mode(),"registration_enabled":app.auth_policy.is_accounts()}),
    ))
}
#[derive(Deserialize)]
pub(super) struct Credentials {
    username: String,
    password: String,
    #[serde(default)]
    display_name: String,
}
fn username(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim().to_ascii_lowercase();
    if !(3..=32).contains(&name.len())
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "用户名需为 3–32 位字母、数字、下划线或短横线",
        ));
    }
    Ok(name)
}
fn require_accounts(app: &AppState) -> Result<(), ApiError> {
    if app.auth_policy.is_accounts() {
        Ok(())
    } else {
        Err(api_error(StatusCode::CONFLICT, "账号登录模式未启用"))
    }
}
// Bound expensive password work and failed attempts, including unknown usernames.
static HASH_SLOTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
static AUTH_ATTEMPTS: std::sync::LazyLock<Mutex<BTreeMap<String, (u64, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));
fn rate_limit(key: &str) -> Result<(), ApiError> {
    let now = now_ms();
    let mut entries = AUTH_ATTEMPTS.lock().unwrap();
    entries.retain(|_, (at, _)| now.saturating_sub(*at) < 60_000);
    if entries.len() > 4096 {
        return Err(api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "登录请求过多，请稍后重试",
        ));
    }
    let entry = entries.entry(key.to_owned()).or_insert((now, 0));
    entry.1 += 1;
    if entry.1 > if key.starts_with("peer:") { 120 } else { 12 } {
        Err(api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "登录请求过多，请一分钟后重试",
        ))
    } else {
        Ok(())
    }
}
async fn password_work(password: String, existing: Option<String>) -> Result<String, ApiError> {
    use argon2::{
        Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString,
    };
    let slot = HASH_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| api_error(StatusCode::TOO_MANY_REQUESTS, "登录繁忙，请稍后重试"))?;
    let result = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        if let Some(hash) = existing {
            let parsed = PasswordHash::new(&hash).map_err(|_| ())?;
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .map_err(|_| ())?;
            Ok(hash)
        } else {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| ())?;
            let salt = SaltString::encode_b64(&bytes).map_err(|_| ())?;
            Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map(|h| h.to_string())
                .map_err(|_| ())
        }
    })
    .await
    .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "密码验证服务异常"))?;
    result.map_err(|_| api_error(StatusCode::UNAUTHORIZED, "用户名或密码错误"))
}
fn profile(user: &User) -> serde_json::Value {
    serde_json::json!({"user_id":user.user_id,"username":user.username,"display_name":user.display_name})
}
async fn issue_session(
    app: &mut AppState,
    user: User,
    register: bool,
) -> ApiResult<serde_json::Value> {
    let token = secret()?;
    let expires_at_ms = now_ms() + 12 * 60 * 60 * 1000;
    let mut candidate = app.platform.clone();
    candidate.sessions.retain(|_, s| s.expires_at_ms > now_ms());
    if candidate
        .sessions
        .values()
        .filter(|s| s.user_id == user.user_id)
        .count()
        >= 32
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            "会话过多，请先退出其他设备",
        ));
    }
    if register {
        candidate.users.insert(user.user_id.clone(), user.clone());
    }
    candidate.sessions.insert(
        digest(&token),
        Session {
            user_id: user.user_id.clone(),
            expires_at_ms,
        },
    );
    commit(app, candidate, vec![]).await?;
    Ok(Json(
        serde_json::json!({"token":token,"expires_at_ms":expires_at_ms,"user":profile(&user)}),
    ))
}
pub(super) async fn register(
    State(state): State<SharedState>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    Json(request): Json<Credentials>,
) -> ApiResult<serde_json::Value> {
    rate_limit(&format!(
        "peer:{}",
        peer.map(|p| p.0.0.ip().to_string())
            .unwrap_or_else(|| "local-test".into())
    ))?;
    let name = username(&request.username)?;
    rate_limit(&format!("register:{name}"))?;
    if request.password.chars().count() < 12
        || request.password.len() > 512
        || request.display_name.chars().count() > 64
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "密码至少 12 个字符，昵称最多 64 个字符",
        ));
    }
    {
        let app = lock_state(&state).await?;
        require_accounts(&app)?;
    }
    let hash = password_work(request.password, None).await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        if app.platform.users.values().any(|u| u.username == name) {
            return Err(api_error(StatusCode::CONFLICT, "用户名已被使用"));
        }
        let user = User {
            user_id: format!("u-{}", secret()?),
            username: name.clone(),
            display_name: if request.display_name.trim().is_empty() {
                name
            } else {
                request.display_name.trim().into()
            },
            password_hash: hash,
            created_at_ms: now_ms(),
        };
        issue_session(&mut app, user, true).await
    })
    .await
}
pub(super) async fn login(
    State(state): State<SharedState>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    Json(request): Json<Credentials>,
) -> ApiResult<serde_json::Value> {
    rate_limit(&format!(
        "peer:{}",
        peer.map(|p| p.0.0.ip().to_string())
            .unwrap_or_else(|| "local-test".into())
    ))?;
    let name = username(&request.username)?;
    rate_limit(&format!("login:{name}"))?;
    if request.password.len() > 512 {
        return Err(api_error(StatusCode::UNAUTHORIZED, "用户名或密码错误"));
    }
    let user = {
        let app = lock_state(&state).await?;
        require_accounts(&app)?;
        app.platform
            .users
            .values()
            .find(|u| u.username == name)
            .cloned()
    };
    // Unknown users perform equivalent Argon2 work without creating an account.
    password_work(
        request.password,
        user.as_ref().map(|u| u.password_hash.clone()),
    )
    .await?;
    let user = user.ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "用户名或密码错误"))?;
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        issue_session(&mut app, user, false).await
    })
    .await
}
pub(super) async fn logout(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<serde_json::Value> {
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        require_accounts(&app)?;
        current_user_id(&headers, &app.auth_policy)?;
        let hash = digest(
            headers
                .get(AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or(""),
        );
        let mut candidate = app.platform.clone();
        candidate.sessions.remove(&hash);
        commit(&mut app, candidate, vec![]).await?;
        Ok(Json(serde_json::json!({"logged_out":true})))
    })
    .await
}
pub(super) async fn me(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<serde_json::Value> {
    let app = lock_state(&state).await?;
    let user = current_user_id(&headers, &app.auth_policy)?;
    Ok(Json(
        app.platform
            .users
            .get(&user)
            .map(profile)
            .unwrap_or_else(|| serde_json::json!({"user_id":user})),
    ))
}
#[derive(Deserialize)]
pub(super) struct InviteRequest {
    role: String,
    account_id: Option<AccountId>,
}
pub(super) async fn invite(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(room): Path<String>,
    Json(request): Json<InviteRequest>,
) -> ApiResult<serde_json::Value> {
    authorize_room_read(&state, &headers, &room, RoomReadAccess::Admin).await?;
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        require_accounts(&app)?;
        ensure_admin(&app, &headers, &room).await?;
        if !matches!(request.role.as_str(), "trader" | "spectator" | "admin")
            || (request.role == "trader") != request.account_id.is_some()
        {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "交易员邀请码必须绑定账户；其他角色不绑定账户",
            ));
        }
        crate::competition::validate_invitation(&app, &room, &request.role, request.account_id)?;
        if let Some(id) = request.account_id {
            let accounts = app
                .rooms
                .account_snapshots(&room)
                .map_err(api_error_from_room)?;
            let exists = match accounts {
                AccountSnapshots::Spot(a) => a.iter().any(|a| a.account_id == id),
                AccountSnapshots::Perp(a) => a.iter().any(|a| a.account_id == id),
            };
            if id == 0 || !exists {
                return Err(api_error(StatusCode::BAD_REQUEST, "账户不存在"));
            }
        }
        let code = secret()?;
        let expires_at_ms = now_ms() + 24 * 60 * 60 * 1000;
        let mut candidate = app.platform.clone();
        candidate.invitations.insert(
            digest(&code),
            Invitation {
                room_id: room.clone(),
                role: request.role,
                account_id: request.account_id,
                expires_at_ms,
                redeemed_by: None,
            },
        );
        commit(&mut app, candidate, vec![]).await?;
        Ok(Json(
            serde_json::json!({"code":code,"room_id":room,"expires_at_ms":expires_at_ms}),
        ))
    })
    .await
}
#[derive(Deserialize)]
pub(super) struct RedeemRequest {
    code: String,
}
pub(super) async fn redeem(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(request): Json<RedeemRequest>,
) -> ApiResult<serde_json::Value> {
    run_durable_state_transaction(state.clone(), async move {
        let mut app = lock_state(&state).await?;
        require_accounts(&app)?;
        let user = current_user_id(&headers, &app.auth_policy)?;
        let key = digest(request.code.trim());
        let invite = app
            .platform
            .invitations
            .get(&key)
            .cloned()
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "邀请码无效"))?;
        if invite.redeemed_by.as_deref() == Some(&user) {
            return Ok(Json(serde_json::json!({"room_id":invite.room_id})));
        }
        if invite.expires_at_ms <= now_ms() || invite.redeemed_by.is_some() {
            return Err(api_error(StatusCode::CONFLICT, "邀请码已过期或已使用"));
        }
        if app
            .journal
            .user_room_role(&user, &invite.room_id)
            .await
            .map_err(api_error_from_journal)?
            .is_some()
        {
            return Err(api_error(
                StatusCode::CONFLICT,
                "已经是房间成员，请管理员处理角色变更",
            ));
        }
        crate::competition::validate_invitation(
            &app,
            &invite.room_id,
            &invite.role,
            invite.account_id,
        )?;
        let mut candidate = app.platform.clone();
        if let Some(comp) = candidate.competitions.get_mut(&invite.room_id)
            && invite.role == "trader"
        {
            let id = invite.account_id.unwrap();
            let profile = app.platform.users.get(&user).unwrap();
            comp.players.insert(
                user.clone(),
                crate::competition::Player::new(id, profile.display_name.clone()),
            );
        }
        candidate.invitations.get_mut(&key).unwrap().redeemed_by = Some(user.clone());
        let grant = MemberGrant {
            room_id: invite.room_id.clone(),
            user_id: user,
            role: invite.role,
            account_id: invite.account_id,
        };
        commit(&mut app, candidate, vec![grant]).await?;
        Ok(Json(serde_json::json!({"room_id":invite.room_id})))
    })
    .await
}
