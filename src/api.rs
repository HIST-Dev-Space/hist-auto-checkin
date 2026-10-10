use crate::{
    AppState,
    auth::{self, AuthUser},
    error::{AppError, Result},
    schedule::next_run,
    school::{CookieInput, SchoolClient},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, types::Json as DbJson};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(crate::web::index))
        .route("/community", get(crate::web::community))
        .route("/start", get(crate::web::start))
        .route("/login", get(crate::web::login))
        .route("/tasks", get(crate::web::tasks))
        .route("/notifications", get(crate::web::notifications))
        .route("/account", get(crate::web::account))
        .route("/assets/{*name}", get(crate::web::asset))
        .route("/api/locations", get(crate::web::locations))
        .route("/api/school-login/start", post(crate::school_login::start))
        .route(
            "/api/school-login/captcha",
            post(crate::school_login::captcha),
        )
        .route(
            "/api/school-login/identity",
            post(crate::school_login::identity),
        )
        .route("/api/school-login/poll", post(crate::school_login::poll))
        .route("/health", get(health))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(me))
        .route("/api/dashboard", get(dashboard))
        .route("/api/auto-checkin", put(set_auto_checkin))
        .route("/api/school-accounts", get(accounts).post(create_account))
        .route(
            "/api/school-accounts/{id}/credentials",
            put(update_credentials),
        )
        .route("/api/school-accounts/{id}/courses", get(courses))
        .route("/api/schedules", get(schedules).post(create_schedule))
        .route("/api/schedules/{id}", put(update_schedule))
        .route("/api/schedules/{id}/run", post(run_schedule))
        .route("/api/jobs", get(jobs))
        .route("/api/weixin/login", post(start_weixin_login))
        .route("/api/weixin/login/{id}/poll", post(poll_weixin_login))
        .route(
            "/api/weixin/binding",
            get(weixin_binding).delete(unbind_weixin),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(crate::web::security))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Result<Json<Value>> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok(Json(
        json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}),
    ))
}

async fn me(AuthUser(user): AuthUser, State(state): State<AppState>) -> Result<Json<Value>> {
    let name: String = sqlx::query_scalar("SELECT username FROM users WHERE id=$1")
        .bind(user)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({"id":user,"username":name})))
}

#[derive(Deserialize)]
struct AccountInput {
    label: String,
    student_no: String,
    cookies: Vec<CookieInput>,
}

#[derive(Serialize, FromRow)]
struct AccountView {
    id: Uuid,
    label: String,
    student_no: String,
    credential_status: String,
}

async fn accounts(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<AccountView>>> {
    Ok(Json(sqlx::query_as("SELECT id,label,student_no,credential_status FROM school_accounts WHERE user_id=$1 ORDER BY created_at").bind(user).fetch_all(&state.db).await?))
}

async fn create_account(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(input): Json<AccountInput>,
) -> Result<(StatusCode, Json<Value>)> {
    auth::rate_limit(&state, &format!("school:{user}"), 10).await?;
    if input.label.trim().is_empty()
        || input.label.len() > 128
        || input.student_no.len() > 64
        || !numeric_id(&input.student_no)
    {
        return Err(AppError::BadRequest("请输入有效名称和数字学号".into()));
    }
    let cookies = verify_cookies(&state, &input.cookies).await?;
    let id = Uuid::new_v4();
    let encrypted = state.secrets.encrypt(
        &serde_json::to_string(&cookies).map_err(anyhow::Error::from)?,
        &format!("school:{user}:{id}"),
    )?;
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM school_accounts WHERE user_id=$1")
        .bind(user)
        .fetch_one(&mut *tx)
        .await?;
    if count >= 20 {
        return Err(AppError::Conflict("每个用户最多绑定 20 个学校账号".into()));
    }
    sqlx::query("INSERT INTO school_accounts(id,user_id,label,student_no,cookies_ciphertext) VALUES($1,$2,$3,$4,$5)").bind(id).bind(user).bind(input.label.trim()).bind(input.student_no).bind(encrypted).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":id,"credential_status":"active"})),
    ))
}

#[derive(Deserialize)]
struct CredentialsInput {
    cookies: Vec<CookieInput>,
}

async fn update_credentials(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<CredentialsInput>,
) -> Result<Json<Value>> {
    owned_account(&state, user, id).await?;
    auth::rate_limit(&state, &format!("school:{user}"), 10).await?;
    let cookies = verify_cookies(&state, &input.cookies).await?;
    let encrypted = state.secrets.encrypt(
        &serde_json::to_string(&cookies).map_err(anyhow::Error::from)?,
        &format!("school:{user}:{id}"),
    )?;
    let changed = sqlx::query("UPDATE school_accounts SET cookies_ciphertext=$1,credential_status='active' WHERE id=$2 AND user_id=$3 AND (execution_until IS NULL OR execution_until<=now())").bind(encrypted).bind(id).bind(user).execute(&state.db).await?.rows_affected();
    if changed == 0 {
        return Err(AppError::Conflict(
            "账号正在执行签到，请稍后更新凭证".into(),
        ));
    }
    Ok(Json(
        json!({"credential_status":"active","message":"凭证已更新；如规则曾因凭证失效停用，请重新启用"}),
    ))
}

async fn owned_account(state: &AppState, user: Uuid, id: Uuid) -> Result<(String, String)> {
    sqlx::query_as("SELECT cookies_ciphertext,credential_status FROM school_accounts WHERE id=$1 AND user_id=$2").bind(id).bind(user).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)
}

async fn verify_cookies(state: &AppState, cookies: &[CookieInput]) -> Result<Vec<CookieInput>> {
    let client = SchoolClient::new_with_proxy(
        &state.config.school_base_url,
        cookies,
        state.config.school_proxy_url.as_deref(),
    )
    .map_err(|_| AppError::BadRequest("Cookie 内容或过期时间无效".into()))?;
    client
        .verify()
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(client.refreshed_cookies())
}

async fn courses(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let (ciphertext, _) = owned_account(&state, user, id).await?;
    auth::rate_limit(&state, &format!("school:{user}"), 10).await?;
    let cookies: Vec<CookieInput> = serde_json::from_str(
        &state
            .secrets
            .decrypt(&ciphertext, &format!("school:{user}:{id}"))?,
    )
    .map_err(anyhow::Error::from)?;
    let client = SchoolClient::new_with_proxy(
        &state.config.school_base_url,
        &cookies,
        state.config.school_proxy_url.as_deref(),
    )?;
    let html = client
        .verify()
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let courses = crate::school::course_items(&html);
    Ok(Json(
        json!({"course_ids":courses.iter().map(|c|&c.id).collect::<Vec<_>>(),"courses":courses}),
    ))
}

fn numeric_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|c| c.is_ascii_digit())
}
fn default_timezone() -> String {
    "Asia/Shanghai".into()
}
fn default_weekdays() -> Vec<i32> {
    (1..=7).collect()
}
fn default_enabled() -> bool {
    true
}

async fn dashboard(AuthUser(user): AuthUser, State(state): State<AppState>) -> Result<Json<Value>> {
    let value: DbJson<Value> = sqlx::query_scalar(
        r#"WITH day AS (
            SELECT date_trunc('day', now() AT TIME ZONE 'Asia/Shanghai') AT TIME ZONE 'Asia/Shanghai' AS start
        ), checkins AS (
            SELECT count(*) AS total,
                count(*) FILTER (WHERE r.updated_at >= day.start AND r.updated_at < day.start + interval '1 day') AS today
            FROM checkin_records r JOIN school_accounts a ON a.id=r.school_account_id CROSS JOIN day
            WHERE a.user_id=$1 AND r.status='succeeded'
        ), today_jobs AS (
            SELECT count(*) FILTER (WHERE status='succeeded') AS succeeded,
                count(*) FILTER (WHERE status IN ('pending','running')) AS running,
                count(*) FILTER (WHERE status='failed') AS failed
            FROM checkin_jobs CROSS JOIN day
            WHERE user_id=$1 AND scheduled_at >= day.start AND scheduled_at < day.start + interval '1 day'
        ), settings AS (
            SELECT count(*) > 0 AS configured, COALESCE(bool_or(enabled), false) AS enabled
            FROM checkin_schedules WHERE user_id=$1
        ), school AS (
            SELECT EXISTS(SELECT 1 FROM school_accounts WHERE user_id=$1 AND credential_status='active') AS active
        )
        SELECT jsonb_build_object(
            'total_successful_checkins',checkins.total,
            'today_successful_checkins',checkins.today,
            'today_status',CASE
                WHEN checkins.today > 0 OR today_jobs.succeeded > 0 THEN 'success'
                WHEN today_jobs.running > 0 THEN 'running'
                WHEN NOT school.active THEN 'login_required'
                WHEN NOT settings.configured THEN 'unconfigured'
                WHEN NOT settings.enabled THEN 'paused'
                WHEN today_jobs.failed > 0 THEN 'failed'
                ELSE 'waiting' END,
            'auto_checkin_enabled',settings.enabled,
            'has_schedule',settings.configured,
            'has_active_account',school.active,
            'account_label',COALESCE((SELECT label FROM school_accounts WHERE user_id=$1 ORDER BY (credential_status='active') DESC,created_at DESC LIMIT 1),'我的账号')
        ) FROM checkins,today_jobs,settings,school"#,
    )
    .bind(user)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(value.0))
}

#[derive(Deserialize)]
struct AutoCheckinInput {
    enabled: bool,
}

async fn set_auto_checkin(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(input): Json<AutoCheckinInput>,
) -> Result<Json<Value>> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let rules: Vec<ScheduleRow> = sqlx::query_as(
        "SELECT * FROM checkin_schedules WHERE user_id=$1 ORDER BY created_at FOR UPDATE",
    )
    .bind(user)
    .fetch_all(&mut *tx)
    .await?;
    if rules.is_empty() {
        return Err(AppError::Conflict("请先设置签到时间和地点".into()));
    }
    if input.enabled {
        let active: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM school_accounts WHERE user_id=$1 AND credential_status='active'",
        )
        .bind(user)
        .fetch_all(&mut *tx)
        .await?;
        if !rules
            .iter()
            .any(|rule| active.contains(&rule.school_account_id))
        {
            return Err(AppError::Conflict("请先重新登录学校账号".into()));
        }
        for rule in rules
            .iter()
            .filter(|rule| active.contains(&rule.school_account_id))
        {
            let next = next_run(rule.local_time, &rule.timezone, &rule.weekdays, Utc::now())?;
            sqlx::query("UPDATE checkin_schedules SET enabled=true,next_run_at=$1 WHERE id=$2")
                .bind(next)
                .bind(rule.id)
                .execute(&mut *tx)
                .await?;
        }
    } else {
        sqlx::query("UPDATE checkin_schedules SET enabled=false WHERE user_id=$1")
            .bind(user)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE checkin_jobs SET status='cancelled',finished_at=now(),result_message='自动签到已关闭' WHERE user_id=$1 AND status='pending'")
            .bind(user).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    dashboard(AuthUser(user), State(state)).await
}

#[derive(Deserialize)]
struct ScheduleInput {
    school_account_id: Uuid,
    // An omitted or empty list discovers the current school task at execution time.
    #[serde(default)]
    course_ids: Vec<String>,
    local_time: String,
    #[serde(default = "default_timezone")]
    timezone: String,
    #[serde(default = "default_weekdays")]
    weekdays: Vec<i32>,
    latitude: f64,
    longitude: f64,
    location_name: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

#[derive(Clone, Serialize, FromRow)]
pub struct ScheduleRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub school_account_id: Uuid,
    pub course_ids: Vec<String>,
    pub local_time: NaiveTime,
    pub timezone: String,
    pub weekdays: Vec<i32>,
    pub latitude: f64,
    pub longitude: f64,
    pub location_name: String,
    pub enabled: bool,
    pub next_run_at: DateTime<Utc>,
}

fn validate_schedule(input: &ScheduleInput) -> Result<(NaiveTime, DateTime<Utc>)> {
    if input.course_ids.len() > 20
        || !input.course_ids.iter().all(|id| numeric_id(id))
        || input.weekdays.is_empty()
        || input.weekdays.len() > 7
        || !input.weekdays.iter().all(|d| (1..=7).contains(d))
    {
        return Err(AppError::BadRequest("课程或重复星期无效".into()));
    }
    if !input.latitude.is_finite()
        || !input.longitude.is_finite()
        || !(-90.0..=90.0).contains(&input.latitude)
        || !(-180.0..=180.0).contains(&input.longitude)
        || input.location_name.trim().is_empty()
        || input.location_name.len() > 128
    {
        return Err(AppError::BadRequest("地点无效".into()));
    }
    let time = NaiveTime::parse_from_str(&input.local_time, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(&input.local_time, "%H:%M:%S"))
        .map_err(|_| AppError::BadRequest("时间应为 HH:MM 或 HH:MM:SS".into()))?;
    let next = next_run(time, &input.timezone, &input.weekdays, Utc::now())
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok((time, next))
}

async fn schedules(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<ScheduleRow>>> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM checkin_schedules WHERE user_id=$1 ORDER BY created_at")
            .bind(user)
            .fetch_all(&state.db)
            .await?,
    ))
}

async fn create_schedule(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(input): Json<ScheduleInput>,
) -> Result<(StatusCode, Json<Value>)> {
    let (time, next) = validate_schedule(&input)?;
    let (_, status) = owned_account(&state, user, input.school_account_id).await?;
    if status != "active" && input.enabled {
        return Err(AppError::Conflict("请先更新学校凭证".into()));
    }
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM checkin_schedules WHERE user_id=$1")
        .bind(user)
        .fetch_one(&mut *tx)
        .await?;
    if count >= 100 {
        return Err(AppError::Conflict("每个用户最多创建 100 个签到规则".into()));
    }
    sqlx::query("INSERT INTO checkin_schedules(id,user_id,school_account_id,course_ids,local_time,timezone,weekdays,latitude,longitude,location_name,enabled,next_run_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(id).bind(user).bind(input.school_account_id).bind(input.course_ids).bind(time).bind(input.timezone).bind(input.weekdays).bind(input.latitude).bind(input.longitude).bind(input.location_name.trim()).bind(input.enabled).bind(next).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":id,"next_run_at":next})),
    ))
}

async fn update_schedule(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<ScheduleInput>,
) -> Result<Json<Value>> {
    let (time, next) = validate_schedule(&input)?;
    let (_, status) = owned_account(&state, user, input.school_account_id).await?;
    if status != "active" && input.enabled {
        return Err(AppError::Conflict("请先更新学校凭证".into()));
    }
    let mut tx = state.db.begin().await?;
    let current: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM checkin_schedules WHERE id=$1 AND user_id=$2 FOR UPDATE",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    if current.is_none() {
        return Err(AppError::NotFound);
    }
    let running: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM checkin_jobs WHERE schedule_id=$1 AND status='running' AND lease_until>now())").bind(id).fetch_one(&mut *tx).await?;
    if running {
        return Err(AppError::Conflict("该规则正在执行，请稍后修改".into()));
    }
    sqlx::query("UPDATE checkin_schedules SET school_account_id=$1,course_ids=$2,local_time=$3,timezone=$4,weekdays=$5,latitude=$6,longitude=$7,location_name=$8,enabled=$9,next_run_at=$10 WHERE id=$11")
        .bind(input.school_account_id).bind(input.course_ids).bind(time).bind(input.timezone).bind(input.weekdays).bind(input.latitude).bind(input.longitude).bind(input.location_name.trim()).bind(input.enabled).bind(next).bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE checkin_jobs SET status='cancelled',finished_at=now(),result_message='规则已修改' WHERE schedule_id=$1 AND status='pending'").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"next_run_at":next})))
}

async fn run_schedule(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>)> {
    auth::rate_limit(&state, &format!("run:{user}"), 10).await?;
    let mut tx = state.db.begin().await?;
    let schedule: ScheduleRow = sqlx::query_as("SELECT s.* FROM checkin_schedules s JOIN school_accounts a ON a.id=s.school_account_id WHERE s.id=$1 AND s.user_id=$2 AND s.enabled AND a.credential_status='active' FOR UPDATE OF s").bind(id).bind(user).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound)?;
    let job = Uuid::new_v4();
    let now = Utc::now();
    sqlx::query("INSERT INTO checkin_jobs(id,schedule_id,user_id,school_account_id,scheduled_at,available_at) VALUES($1,$2,$3,$4,$5,$5)").bind(job).bind(id).bind(user).bind(schedule.school_account_id).bind(now).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({"job_id":job}))))
}

#[derive(Deserialize)]
struct JobQuery {
    limit: Option<i64>,
    before: Option<DateTime<Utc>>,
}
async fn jobs(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Query(query): Query<JobQuery>,
) -> Result<Json<Value>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let values: Vec<DbJson<Value>> = sqlx::query_scalar("SELECT jsonb_build_object('id',j.id,'schedule_id',j.schedule_id,'scheduled_at',j.scheduled_at,'status',j.status,'attempts',j.attempts,'message',j.result_message,'finished_at',j.finished_at,'notification_status',n.status,'notification_error',n.last_error,'records',COALESCE((SELECT jsonb_agg(jsonb_build_object('course_id',r.course_id,'punch_id',r.punch_id,'title',r.task_title,'status',r.status,'message',r.message,'latitude',r.latitude,'longitude',r.longitude)) FROM checkin_records r WHERE r.job_id=j.id),'[]'::jsonb)) FROM checkin_jobs j LEFT JOIN notification_outbox n ON n.job_id=j.id WHERE j.user_id=$1 AND ($2::timestamptz IS NULL OR j.scheduled_at<$2) ORDER BY j.scheduled_at DESC LIMIT $3").bind(user).bind(query.before).bind(limit).fetch_all(&state.db).await?;
    Ok(Json(
        json!({"jobs":values.into_iter().map(|v|v.0).collect::<Vec<_>>()}),
    ))
}

async fn start_weixin_login(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<Value>)> {
    auth::rate_limit(&state, &format!("weixin-login:{user}"), 5).await?;
    let existing: Option<String> =
        sqlx::query_scalar("SELECT token_ciphertext FROM weixin_bindings WHERE user_id=$1")
            .bind(user)
            .fetch_optional(&state.db)
            .await?;
    let tokens = existing
        .map(|s| state.secrets.decrypt(&s, &format!("weixin-token:{user}")))
        .transpose()?
        .into_iter()
        .collect();
    let qr = state
        .weixin
        .qr_code(tokens)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let id = Uuid::new_v4();
    let expires = Utc::now() + Duration::minutes(5);
    let ciphertext = state
        .secrets
        .encrypt(&qr.qrcode, &format!("weixin-qr:{user}:{id}"))?;
    sqlx::query("INSERT INTO weixin_login_sessions(id,user_id,qrcode_ciphertext,base_url,expires_at) VALUES($1,$2,$3,$4,$5)").bind(id).bind(user).bind(ciphertext).bind(state.weixin.default_base()).bind(expires).execute(&state.db).await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"session_id":id,"qrcode_img_content":qr.qrcode_img_content,"expires_at":expires}),
        ),
    ))
}

#[derive(Deserialize)]
struct PollInput {
    verify_code: Option<String>,
}

async fn poll_weixin_login(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<PollInput>,
) -> Result<Json<Value>> {
    if input.verify_code.as_ref().is_some_and(|s| {
        s.len() > 16 || s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric())
    }) {
        return Err(AppError::BadRequest("验证码无效".into()));
    }
    auth::rate_limit(&state, &format!("weixin-poll:{user}"), 100).await?;
    let session: Option<(String,String,String,DateTime<Utc>)> = sqlx::query_as("SELECT qrcode_ciphertext,base_url,status,expires_at FROM weixin_login_sessions WHERE id=$1 AND user_id=$2").bind(id).bind(user).fetch_optional(&state.db).await?;
    let (qrcode, base, status, expires) = session.ok_or(AppError::NotFound)?;
    if status == "confirmed" {
        return Ok(Json(json!({"status":"confirmed"})));
    }
    if expires <= Utc::now() {
        return Ok(Json(json!({"status":"expired"})));
    }
    let lease = Uuid::new_v4();
    if sqlx::query("UPDATE weixin_login_sessions SET poll_token=$1,poll_until=now()+interval '45 seconds' WHERE id=$2 AND user_id=$3 AND (poll_until IS NULL OR poll_until<=now())").bind(lease).bind(id).bind(user).execute(&state.db).await?.rows_affected()==0 { return Err(AppError::RateLimited); }
    let result = complete_weixin_poll(
        &state,
        user,
        id,
        lease,
        &qrcode,
        &base,
        input.verify_code.as_deref(),
    )
    .await;
    sqlx::query("UPDATE weixin_login_sessions SET poll_until=NULL,poll_token=NULL WHERE id=$1 AND poll_token=$2").bind(id).bind(lease).execute(&state.db).await?;
    result
}

async fn complete_weixin_poll(
    state: &AppState,
    user: Uuid,
    id: Uuid,
    lease: Uuid,
    qrcode: &str,
    base: &str,
    verify: Option<&str>,
) -> Result<Json<Value>> {
    let qr = state
        .secrets
        .decrypt(qrcode, &format!("weixin-qr:{user}:{id}"))?;
    let result = state
        .weixin
        .qr_status(base, &qr, verify)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let mut tx = state.db.begin().await?;
    let owned: Option<Uuid> = sqlx::query_scalar("SELECT id FROM weixin_login_sessions WHERE id=$1 AND user_id=$2 AND poll_token=$3 AND expires_at>now() FOR UPDATE").bind(id).bind(user).bind(lease).fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Err(AppError::Conflict("扫码会话已过期，请重新扫码".into()));
    }
    if result.status == "confirmed" {
        let token = result
            .bot_token
            .filter(|s| !s.is_empty() && s.len() <= 16384 && !s.contains(['\r', '\n']))
            .ok_or_else(|| AppError::Upstream("微信未返回有效 Bot 凭证".into()))?;
        let bot = result
            .ilink_bot_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::Upstream("微信未返回 Bot ID".into()))?;
        let recipient = result
            .ilink_user_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::Upstream("微信未返回扫码用户 ID".into()))?;
        let base_url = result.baseurl.unwrap_or_else(|| base.into());
        state
            .weixin
            .validated_base(&base_url)
            .map_err(|e| AppError::Upstream(e.to_string()))?;
        let encrypted = state
            .secrets
            .encrypt(&token, &format!("weixin-token:{user}"))?;
        sqlx::query("INSERT INTO weixin_bindings(user_id,bot_id,recipient_id,token_ciphertext,base_url) VALUES($1,$2,$3,$4,$5) ON CONFLICT(user_id) DO UPDATE SET bot_id=EXCLUDED.bot_id,recipient_id=EXCLUDED.recipient_id,token_ciphertext=EXCLUDED.token_ciphertext,base_url=EXCLUDED.base_url,context_ciphertext=NULL,context_updated_at=NULL,cursor_ciphertext=NULL,status='active',poll_token=NULL,poll_until=NULL,poll_available_at=now(),last_error=NULL,updated_at=now()")
            .bind(user).bind(bot).bind(recipient).bind(encrypted).bind(base_url).execute(&mut *tx).await?;
    } else if result.status == "scaned_but_redirect" {
        let host = result
            .redirect_host
            .ok_or_else(|| AppError::Upstream("微信重定向地址缺失".into()))?;
        let base_url = format!("https://{host}");
        state
            .weixin
            .validated_base(&base_url)
            .map_err(|e| AppError::Upstream(e.to_string()))?;
        sqlx::query("UPDATE weixin_login_sessions SET base_url=$1 WHERE id=$2")
            .bind(base_url)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE weixin_login_sessions SET status=$1 WHERE id=$2")
        .bind(&result.status)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"status":result.status,"message":if result.status=="confirmed" { "绑定成功，请向微信 Bot 发送一条消息以建立通知会话" } else if result.status=="binded_redirect" { "Bot 已绑定；请查看当前绑定状态，必要时重新获取二维码" } else { "按微信页面提示完成扫码或验证" }}),
    ))
}

async fn weixin_binding(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Value>> {
    let value: Option<DbJson<Value>> = sqlx::query_scalar("SELECT jsonb_build_object('status',status,'has_context',context_ciphertext IS NOT NULL,'context_updated_at',context_updated_at,'last_error',last_error,'updated_at',updated_at) FROM weixin_bindings WHERE user_id=$1").bind(user).fetch_optional(&state.db).await?;
    Ok(Json(value.map(|v| v.0).unwrap_or_else(
        || json!({"status":"unbound","has_context":false}),
    )))
}

async fn unbind_weixin(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
) -> Result<StatusCode> {
    sqlx::query("DELETE FROM weixin_bindings WHERE user_id=$1")
        .bind(user)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
