use crate::{
    AppState, auth,
    error::{AppError, Result},
    school::CookieInput,
};
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chrono::{Duration, Utc};
use cookie_store::{CookieExpiration, CookieStore};
use rand::{RngCore, rngs::OsRng};
use regex::Regex;
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::FromRow;
use std::{
    sync::{Arc, LazyLock, Mutex},
    time::Duration as StdDuration,
};
use uuid::Uuid;

struct LoginJar(Mutex<CookieStore>);
impl reqwest::cookie::CookieStore for LoginJar {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let cookies = headers
            .filter_map(|h| h.to_str().ok())
            .filter_map(|h| cookie::Cookie::parse(h.to_owned()).ok());
        if let Ok(mut store) = self.0.lock() {
            store.store_response_cookies(cookies, url);
        }
    }
    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        let store = self.0.lock().ok()?;
        let header = store
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if header.is_empty() {
            None
        } else {
            header.parse().ok()
        }
    }
}
impl LoginJar {
    fn restore(raw: &str) -> Result<Self> {
        let store = cookie_store::serde::json::load(raw.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid stored login cookies"))?;
        Ok(Self(Mutex::new(store)))
    }
    fn snapshot(&self) -> Result<String> {
        let store = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("login cookie lock failed"))?;
        let mut bytes = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&store, &mut bytes)
            .map_err(|_| anyhow::anyhow!("login cookie serialization failed"))?;
        Ok(String::from_utf8(bytes).map_err(anyhow::Error::from)?)
    }
    fn school_cookies(&self, base: &Url) -> Result<Vec<CookieInput>> {
        let store = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("login cookie lock failed"))?;
        Ok(store
            .iter_unexpired()
            .filter(|c| c.domain.matches(base))
            .map(|c| CookieInput {
                name: c.name().into(),
                value: c.value().into(),
                path: c.path.to_string(),
                expires_at: match c.expires {
                    CookieExpiration::AtUtc(dt) => Some(dt.unix_timestamp()),
                    CookieExpiration::SessionEnd => None,
                },
            })
            .collect())
    }
}

struct LoginClient {
    client: Client,
    no_redirect: Client,
    jar: Arc<LoginJar>,
    login: Url,
    school: Url,
}
impl LoginClient {
    fn new(state: &AppState, raw: &str) -> Result<Self> {
        let jar = Arc::new(LoginJar::restore(raw)?);
        let login = Url::parse(&state.config.school_login_base_url).map_err(anyhow::Error::from)?;
        let school = Url::parse(&state.config.school_base_url).map_err(anyhow::Error::from)?;
        let allowed = [login.origin(), school.origin()];
        let mut builder=Client::builder().cookie_provider(jar.clone()).timeout(StdDuration::from_secs(25)).connect_timeout(StdDuration::from_secs(10))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36")
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len()>=6 {attempt.error("too many redirects")}
                else if allowed.contains(&attempt.url().origin()) {attempt.follow()} else {attempt.stop()}
            }));
        if let Some(proxy) = crate::config::school_proxy(state.config.school_proxy_url.as_deref())?
        {
            builder = builder.proxy(proxy);
        }
        let client = builder.build().map_err(anyhow::Error::from)?;
        let mut builder = Client::builder()
            .cookie_provider(jar.clone())
            .timeout(StdDuration::from_secs(25))
            .user_agent("Mozilla/5.0 HistAutoCheckin/0.1")
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = crate::config::school_proxy(state.config.school_proxy_url.as_deref())?
        {
            builder = builder.proxy(proxy);
        }
        let no_redirect = builder.build().map_err(anyhow::Error::from)?;
        Ok(Self {
            client,
            no_redirect,
            jar,
            login,
            school,
        })
    }
    fn login_url(&self, path: &str) -> Result<Url> {
        self.login
            .join(path)
            .map_err(|_| AppError::Upstream("学校登录地址无效".into()))
    }
    async fn captcha(&self) -> Result<String> {
        let response = self
            .client
            .get(self.login_url("/captcha/default")?)
            .query(&[("r", Uuid::new_v4().to_string())])
            .send()
            .await
            .map_err(|_| upstream())?;
        if !response.status().is_success() {
            return Err(upstream());
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("image/png")
            .split(';')
            .next()
            .unwrap_or("image/png")
            .to_string();
        if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime.as_str()) {
            return Err(upstream());
        }
        let bytes = response.bytes().await.map_err(|_| upstream())?;
        if bytes.len() > 1024 * 1024 {
            return Err(upstream());
        }
        Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
    }
    async fn qr(&self) -> Result<String> {
        let response = self
            .client
            .get(self.login_url("/qr/weixin/student/43")?)
            .send()
            .await
            .map_err(|_| upstream())?;
        if !response.status().is_success() {
            return Err(upstream());
        }
        let html = response.text().await.map_err(|_| upstream())?;
        static QR: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?:var|let|const)\s+qrurl\s*=\s*("(?:[^"\\]|\\.)*"|'[^']*')"#).unwrap()
        });
        let captured = QR
            .captures(&html)
            .and_then(|c| c.get(1))
            .ok_or_else(upstream)?
            .as_str();
        let content = if captured.starts_with('"') {
            serde_json::from_str::<String>(captured).map_err(|_| upstream())?
        } else {
            captured[1..captured.len() - 1].to_owned()
        };
        if content.is_empty() || content.len() > 4096 {
            return Err(upstream());
        }
        Ok(content.replace("&amp;", "&"))
    }
    async fn finish(&self, redirect: &str) -> Result<(String, String, Vec<CookieInput>)> {
        let (uid, tm, sign) = signed_identity(&self.school, redirect).map_err(|_| {
            exchange_failed(
                "callback_validation",
                "学校返回的登录地址或签名格式无法识别",
            )
        })?;
        let response = self
            .client
            .get(
                self.school
                    .join("/student/uidlogin")
                    .map_err(anyhow::Error::from)?,
            )
            .query(&[
                ("uid", uid.as_str()),
                ("tm", tm.as_str()),
                ("sign", sign.as_str()),
                ("option", ""),
            ])
            .send()
            .await
            .map_err(|error| exchange_network_error("school_uid_login", &error))?;
        if response.status().is_redirection() {
            // Some school responses advertise an HTTP Location after issuing cookies.
            // Keep HTTPS and verify the fixed student URL using the issued cookies.
            let same_school = response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .and_then(|target| self.school.join(target).ok())
                .is_some_and(|target| target.host_str() == self.school.host_str());
            if !same_school {
                return Err(exchange_failed(
                    "school_uid_login",
                    "学校登录接口返回了不符合配置的跳转地址",
                ));
            }
        } else if !response.status().is_success() {
            return Err(exchange_http_error("school_uid_login", response.status()));
        }
        let response = self
            .client
            .get(self.school.join("/student").map_err(anyhow::Error::from)?)
            .send()
            .await
            .map_err(|error| exchange_network_error("student_verification", &error))?;
        if !response.status().is_success() {
            return Err(exchange_http_error(
                "student_verification",
                response.status(),
            ));
        }
        let html = response
            .text()
            .await
            .map_err(|error| exchange_network_error("student_page_read", &error))?;
        if (html.contains("登录") && html.contains("学生身份验证"))
            || (!html.contains("用户中心") && !html.contains("我加入的"))
        {
            return Err(exchange_failed(
                "student_verification",
                "未取得有效的学校登录状态，请重新获取二维码",
            ));
        }
        let cookies = self.jar.school_cookies(&self.school)?;
        if !cookies
            .iter()
            .any(|c| c.name.starts_with("remember_student"))
        {
            return Err(exchange_failed(
                "remember_cookie_check",
                "学校未返回可保存的登录凭证，请重新登录",
            ));
        }
        Ok((uid, html, cookies))
    }
}

fn upstream() -> AppError {
    AppError::Upstream("微学工登录暂时不可用，请稍后重试".into())
}

fn exchange_failed(stage: &'static str, reason: &str) -> AppError {
    tracing::warn!(stage, reason, "school session exchange failed");
    AppError::SchoolSessionExchange(format!("学校身份已确认，但登录凭证获取失败：{reason}"))
}
fn exchange_network_error(stage: &'static str, error: &reqwest::Error) -> AppError {
    // Never log reqwest's Display/Debug: its URL may include uid/tm/sign.
    let reason = if error.is_timeout() {
        "连接签到系统超时，服务器暂时无法完成学校登录"
    } else if error.is_connect() {
        "无法连接签到系统，请稍后重试"
    } else {
        "读取签到系统响应失败，请稍后重试"
    };
    exchange_failed(stage, reason)
}
fn exchange_http_error(stage: &'static str, status: reqwest::StatusCode) -> AppError {
    exchange_failed(
        stage,
        &format!("签到系统返回 HTTP {}，尚未取得登录凭证", status.as_u16()),
    )
}

pub fn signed_identity(school: &Url, redirect: &str) -> Result<(String, String, String)> {
    let callback = school.join(redirect).map_err(|_| upstream())?;
    if callback.host_str() != school.host_str()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || !["http", "https"].contains(&callback.scheme())
        || !["/student", "/student/uidlogin"].contains(&callback.path())
    {
        return Err(upstream());
    }
    let params: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
    let uid = params
        .get("uid")
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|c| c.is_ascii_digit()))
        .ok_or_else(upstream)?
        .clone();
    let tm = params
        .get("tm")
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|c| c.is_ascii_digit()))
        .ok_or_else(upstream)?
        .clone();
    let sign = params
        .get("sign")
        .filter(|s| (8..=128).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(upstream)?
        .clone();
    Ok((uid, tm, sign))
}

fn web_request(headers: &HeaderMap) -> Result<()> {
    if headers.get("x-hist-request").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err(AppError::Forbidden);
    }
    Ok(())
}
fn response(value: Value, cookies: Vec<String>) -> Response {
    let mut result = Json(value).into_response();
    for cookie in cookies {
        if let Ok(cookie) = cookie.parse() {
            result.headers_mut().append("set-cookie", cookie);
        }
    }
    result
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    result
}
fn session_cookie(state: &AppState, name: &str, value: &str, max_age: u32) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if state.config.cookie_secure {
            "; Secure"
        } else {
            ""
        }
    )
}
fn random_secret() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Deserialize)]
pub struct StartInput {
    pub method: String,
}
pub async fn start(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<StartInput>,
) -> Result<Response> {
    web_request(&headers)?;
    if !["wechat", "identity"].contains(&input.method.as_str()) {
        return Err(AppError::BadRequest("请选择有效登录方式".into()));
    }
    auth::rate_limit(
        &state,
        &format!(
            "school-login:{}",
            auth::client_ip(peer.ip(), &headers, &state.config.trusted_proxies)
        ),
        60,
    )
    .await?;
    let client = LoginClient::new(&state, "[]")?;
    let mut payload = json!({"method":input.method,"expires_at":Utc::now()+Duration::minutes(5)});
    if input.method == "wechat" {
        payload["qr_content"] = json!(client.qr().await?);
    } else {
        payload["captcha_data_url"] = json!(client.captcha().await?);
    }
    let id = Uuid::new_v4();
    let secret = random_secret();
    let jar = state
        .secrets
        .encrypt(&client.jar.snapshot()?, &format!("school-login:{id}"))?;
    sqlx::query("INSERT INTO school_login_sessions(id,secret_hash,jar_ciphertext,method,expires_at) VALUES($1,$2,$3,$4,$5)")
        .bind(id).bind(auth::token_hash(&secret)).bind(jar).bind(input.method).bind(Utc::now()+Duration::minutes(5)).execute(&state.db).await?;
    Ok(response(
        payload,
        vec![session_cookie(
            &state,
            "hist_school_login",
            &format!("{id}.{secret}"),
            300,
        )],
    ))
}

#[derive(FromRow)]
struct LoginLease {
    id: Uuid,
    jar_ciphertext: String,
    method: String,
    lease_token: Option<Uuid>,
    callback_ciphertext: Option<String>,
}
async fn claim(state: &AppState, headers: &HeaderMap) -> Result<(LoginLease, LoginClient)> {
    web_request(headers)?;
    let cookie = auth::cookie_value(headers, "hist_school_login").ok_or(AppError::Unauthorized)?;
    let (id, secret) = cookie.split_once('.').ok_or(AppError::Unauthorized)?;
    let id: Uuid = id.parse().map_err(|_| AppError::Unauthorized)?;
    let lease=sqlx::query_as::<_,LoginLease>("UPDATE school_login_sessions SET lease_token=$1,lease_until=now()+interval '120 seconds' WHERE id=$2 AND secret_hash=$3 AND expires_at>now() AND (lease_until IS NULL OR lease_until<=now()) RETURNING *")
        .bind(Uuid::new_v4()).bind(id).bind(auth::token_hash(secret)).fetch_optional(&state.db).await?.ok_or_else(||AppError::Conflict("登录已过期或正在处理，请稍后刷新重试".into()))?;
    let client = LoginClient::new(
        state,
        &state
            .secrets
            .decrypt(&lease.jar_ciphertext, &format!("school-login:{}", lease.id))?,
    )?;
    Ok((lease, client))
}
async fn persist(state: &AppState, lease: &LoginLease, client: &LoginClient) -> Result<()> {
    let jar = state.secrets.encrypt(
        &client.jar.snapshot()?,
        &format!("school-login:{}", lease.id),
    )?;
    sqlx::query("UPDATE school_login_sessions SET jar_ciphertext=$1,lease_token=NULL,lease_until=NULL WHERE id=$2 AND lease_token=$3").bind(jar).bind(lease.id).bind(lease.lease_token).execute(&state.db).await?;
    Ok(())
}

pub async fn captcha(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let (lease, client) = claim(&state, &headers).await?;
    auth::rate_limit(&state, &format!("captcha:{}", lease.id), 20).await?;
    let result = client.captcha().await;
    persist(&state, &lease, &client).await?;
    Ok(response(json!({"captcha_data_url":result?}), vec![]))
}

#[derive(Deserialize)]
pub struct IdentityInput {
    pub name: String,
    pub student_no: String,
    pub captcha_code: String,
}
pub async fn identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<IdentityInput>,
) -> Result<Response> {
    if input.name.trim().is_empty()
        || input.name.len() > 128
        || input.student_no.is_empty()
        || input.student_no.len() > 64
        || !input.student_no.bytes().all(|c| c.is_ascii_digit())
        || input.captcha_code.is_empty()
        || input.captcha_code.len() > 16
        || !input
            .captcha_code
            .bytes()
            .all(|c| c.is_ascii_alphanumeric())
    {
        return Err(AppError::BadRequest("请输入姓名、学号和图片验证码".into()));
    }
    let (lease, client) = claim(&state, &headers).await?;
    auth::rate_limit(&state, &format!("identity:{}", lease.id), 10).await?;
    let result = identity_inner(&state, &lease, &client, &input).await;
    persist(&state, &lease, &client).await?;
    result
}

async fn identity_inner(
    state: &AppState,
    lease: &LoginLease,
    client: &LoginClient,
    input: &IdentityInput,
) -> Result<Response> {
    if lease.method != "identity" {
        return Err(AppError::BadRequest("请先选择姓名学号登录".into()));
    }
    let response = client
        .no_redirect
        .post(client.login_url("/fields/login/student/43")?)
        .form(&[
            ("name", input.name.trim()),
            ("no", input.student_no.as_str()),
            ("checkcode", input.captcha_code.as_str()),
        ])
        .send()
        .await
        .map_err(|_| upstream())?;
    let redirect = response
        .headers()
        .get("location")
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let is_redirect = response.status().is_redirection();
    let text = response.text().await.map_err(|_| upstream())?;
    if text.contains("已认证过并绑定了微信") || text.contains("选择其他登录方式")
    {
        let qr = client.qr().await?;
        sqlx::query(
            "UPDATE school_login_sessions SET method='wechat' WHERE id=$1 AND lease_token=$2",
        )
        .bind(lease.id)
        .bind(lease.lease_token)
        .execute(&state.db)
        .await?;
        return Ok(super::school_login::response(
            json!({"status":"need_wechat","qr_content":qr,"message":"该学校账号已绑定微信，请扫码完成验证"}),
            vec![],
        ));
    }
    if text.contains("验证码错误") {
        return Err(AppError::BadRequest(
            "验证码错误，请刷新验证码后重试".into(),
        ));
    }
    if text.contains("姓名或学号") {
        return Err(AppError::BadRequest("姓名或学号错误".into()));
    }
    if !is_redirect {
        return Err(upstream());
    }
    let (uid, html, cookies) = client.finish(&redirect.ok_or_else(upstream)?).await?;
    complete(
        state,
        lease,
        &uid,
        &html,
        cookies,
        Some((&input.name, &input.student_no)),
    )
    .await
}

pub async fn poll(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let (lease, client) = claim(&state, &headers).await?;
    auth::rate_limit(&state, &format!("school-qr-poll:{}", lease.id), 200).await?;
    let result = poll_inner(&state, &lease, &client).await;
    persist(&state, &lease, &client).await?;
    result
}
async fn poll_inner(
    state: &AppState,
    lease: &LoginLease,
    client: &LoginClient,
) -> Result<Response> {
    if lease.method != "wechat" {
        return Err(AppError::BadRequest("请先选择微信扫码登录".into()));
    }
    let scope = format!("school-login-callback:{}", lease.id);
    let redirect = if let Some(ciphertext) = &lease.callback_ciphertext {
        state.secrets.decrypt(ciphertext, &scope)?
    } else {
        let response = client
            .client
            .get(client.login_url("/qr/weixin/student/43")?)
            .query(&[("op", "checklogin")])
            .send()
            .await
            .map_err(|_| upstream())?;
        if !response.status().is_success() {
            return Err(upstream());
        }
        let status: Value = response.json().await.map_err(|_| upstream())?;
        if status.get("status").and_then(Value::as_bool) != Some(true) {
            return Ok(super::school_login::response(
                json!({"status":"waiting"}),
                vec![],
            ));
        }
        let redirect = status
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(upstream)?
            .to_owned();
        signed_identity(&client.school, &redirect).map_err(|_| {
            exchange_failed(
                "callback_validation",
                "学校返回的登录地址或签名格式无法识别",
            )
        })?;
        let ciphertext = state.secrets.encrypt(&redirect, &scope)?;
        sqlx::query("UPDATE school_login_sessions SET callback_ciphertext=$1 WHERE id=$2 AND lease_token=$3").bind(ciphertext).bind(lease.id).bind(lease.lease_token).execute(&state.db).await?;
        tracing::info!(session=%lease.id,stage="qr_confirmed","school QR login confirmed");
        redirect
    };
    let (uid, html, cookies) = client.finish(&redirect).await?;
    complete(state, lease, &uid, &html, cookies, None).await
}

async fn complete(
    state: &AppState,
    lease: &LoginLease,
    uid: &str,
    html: &str,
    cookies: Vec<CookieInput>,
    identity: Option<(&str, &str)>,
) -> Result<Response> {
    let mut tx = state.db.begin().await?;
    let owned: Option<Uuid>=sqlx::query_scalar("SELECT id FROM school_login_sessions WHERE id=$1 AND lease_token=$2 AND expires_at>now() FOR UPDATE").bind(lease.id).bind(lease.lease_token).fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Err(AppError::Unauthorized);
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("hist-school-43:{uid}"))
        .execute(&mut *tx)
        .await?;
    let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT user_id,school_account_id FROM school_identities WHERE school_uid=$1",
    )
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await?;
    let profile = extract_profile(html);
    let supplied_label = profile
        .0
        .or_else(|| identity.map(|(name, _)| name.to_owned()));
    let supplied_no = profile.1.or_else(|| identity.map(|(_, no)| no.to_owned()));
    let mut label = supplied_label
        .clone()
        .unwrap_or_else(|| "微学工账号".into());
    let mut student_no = supplied_no.clone().unwrap_or_else(|| format!("uid:{uid}"));
    let (user, account) = if let Some(existing) = existing {
        let (busy,previous_label,previous_no): (bool,String,String)=sqlx::query_as("SELECT COALESCE(execution_until>now(),false),label,student_no FROM school_accounts WHERE id=$1 FOR UPDATE").bind(existing.1).fetch_one(&mut *tx).await?;
        if busy {
            return Err(AppError::Conflict(
                "账号正在执行签到，请稍后重新登录".into(),
            ));
        }
        if supplied_label.is_none() {
            label = previous_label;
        }
        if supplied_no.is_none() {
            student_no = previous_no;
        }
        existing
    } else {
        if !state.config.allow_registration {
            return Err(AppError::Conflict("服务暂未开放新用户".into()));
        }
        let user = Uuid::new_v4();
        let account = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,username) VALUES($1,$2)")
            .bind(user)
            .bind(format!("wxg-{}", user.simple()))
            .execute(&mut *tx)
            .await?;
        let encrypted = state.secrets.encrypt(
            &serde_json::to_string(&cookies).map_err(anyhow::Error::from)?,
            &format!("school:{user}:{account}"),
        )?;
        sqlx::query("INSERT INTO school_accounts(id,user_id,label,student_no,cookies_ciphertext) VALUES($1,$2,$3,$4,$5)").bind(account).bind(user).bind(&label).bind(&student_no).bind(encrypted).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO school_identities(school_uid,user_id,school_account_id) VALUES($1,$2,$3)",
        )
        .bind(uid)
        .bind(user)
        .bind(account)
        .execute(&mut *tx)
        .await?;
        (user, account)
    };
    let encrypted = state.secrets.encrypt(
        &serde_json::to_string(&cookies).map_err(anyhow::Error::from)?,
        &format!("school:{user}:{account}"),
    )?;
    sqlx::query("UPDATE school_accounts SET label=$1,student_no=$2,cookies_ciphertext=$3,credential_status='active' WHERE id=$4").bind(label).bind(student_no).bind(encrypted).bind(account).execute(&mut *tx).await?;
    let token = random_secret();
    sqlx::query("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES($1,$2,$3)")
        .bind(auth::token_hash(&token))
        .bind(user)
        .bind(Utc::now() + Duration::days(30))
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM school_login_sessions WHERE id=$1")
        .bind(lease.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(response(
        json!({"status":"authenticated","school_account_id":account}),
        vec![
            session_cookie(state, "hist_session", &token, 30 * 86400),
            session_cookie(state, "hist_school_login", "", 0),
        ],
    ))
}

fn extract_profile(html: &str) -> (Option<String>, Option<String>) {
    let document = scraper::Html::parse_document(html);
    let name_selector = scraper::Selector::parse("[data-student-name]").unwrap();
    let no_selector = scraper::Selector::parse("[data-student-no]").unwrap();
    let name = document
        .select(&name_selector)
        .find_map(|e| e.value().attr("data-student-name"))
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned);
    let no = document
        .select(&no_selector)
        .find_map(|e| e.value().attr("data-student-no"))
        .filter(|s| !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_digit()))
        .map(str::to_owned);
    (name, no)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_is_fixed_to_school_host_and_signed_fields() {
        let base = Url::parse("https://school.example").unwrap();
        assert!(
            signed_identity(
                &base,
                "http://school.example/student?uid=12&tm=34&sign=abcdef0123456789"
            )
            .is_ok()
        );
        for url in [
            "https://evil.example/student?uid=12&tm=34&sign=abcdef0123456789",
            "https://school.example/student?uid=12",
            "https://school.example/private?uid=12&tm=34&sign=abcdef0123456789",
        ] {
            assert!(signed_identity(&base, url).is_err());
        }
    }
    #[test]
    fn temporary_cookie_snapshot_preserves_session_and_host_isolation() {
        use reqwest::cookie::CookieStore as _;
        let jar = LoginJar::restore("[]").unwrap();
        let origin = Url::parse("https://login.example").unwrap();
        let values = [HeaderValue::from_static(
            "captcha_session=private; Path=/; HttpOnly",
        )];
        jar.set_cookies(&mut values.iter(), &origin);
        let restored = LoginJar::restore(&jar.snapshot().unwrap()).unwrap();
        assert_eq!(
            restored.cookies(&origin).unwrap(),
            "captcha_session=private"
        );
        assert!(
            restored
                .cookies(&Url::parse("https://school.example").unwrap())
                .is_none()
        );
    }
}
