use crate::{
    AppState,
    error::{AppError, Result},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{StatusCode, request::Parts},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, SocketAddr};
use uuid::Uuid;

pub struct AuthUser(pub Uuid);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        let token = request_token(&parts.headers)
            .filter(|v| v.len() == 43)
            .ok_or(AppError::Unauthorized)?;
        let id = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM sessions WHERE token_hash=$1 AND expires_at>now()",
        )
        .bind(token_hash(token))
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;
        Ok(Self(id))
    }
}

#[derive(Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

fn validate(input: &Credentials) -> Result<String> {
    let username = input.username.trim().to_lowercase();
    if !(3..=64).contains(&username.len())
        || !username
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.@".contains(&c))
    {
        return Err(AppError::BadRequest(
            "用户名需为 3..64 位英文、数字或 _-.@".into(),
        ));
    }
    if !(10..=128).contains(&input.password.len()) {
        return Err(AppError::BadRequest("密码需为 10..128 字节".into()));
    }
    Ok(username)
}

pub async fn rate_limit(state: &AppState, key: &str, maximum: i32) -> Result<()> {
    let hits: i32 = sqlx::query_scalar("INSERT INTO auth_buckets(bucket_key,hits,expires_at) VALUES($1,1,now()+interval '10 minutes') ON CONFLICT(bucket_key) DO UPDATE SET hits=CASE WHEN auth_buckets.expires_at<=now() THEN 1 ELSE auth_buckets.hits+1 END, expires_at=CASE WHEN auth_buckets.expires_at<=now() THEN now()+interval '10 minutes' ELSE auth_buckets.expires_at END RETURNING hits")
        .bind(key).fetch_one(&state.db).await?;
    if hits > maximum {
        return Err(AppError::RateLimited);
    }
    Ok(())
}

pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(input): Json<Credentials>,
) -> Result<(StatusCode, Json<Value>)> {
    if !state.config.allow_registration {
        return Err(AppError::Conflict("注册已关闭".into()));
    }
    rate_limit(
        &state,
        &format!(
            "auth:{}",
            client_ip(peer.ip(), &headers, &state.config.trusted_proxies)
        ),
        state.config.auth_rate_limit,
    )
    .await?;
    let username = validate(&input)?;
    let permit = state
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::RateLimited)?;
    let hash = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        Argon2::default()
            .hash_password(input.password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|v| v.to_string())
            .map_err(|_| anyhow::anyhow!("password hashing failed"))
    })
    .await
    .map_err(anyhow::Error::from)??;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,username,password_hash) VALUES($1,$2,$3)")
        .bind(id)
        .bind(&username)
        .bind(hash)
        .execute(&state.db)
        .await?;
    Ok((
        StatusCode::CREATED,
        issue_session(&state, id, &username).await?,
    ))
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(input): Json<Credentials>,
) -> Result<Json<Value>> {
    rate_limit(
        &state,
        &format!(
            "auth:{}",
            client_ip(peer.ip(), &headers, &state.config.trusted_proxies)
        ),
        state.config.auth_rate_limit,
    )
    .await?;
    let username = validate(&input)?;
    rate_limit(&state, &format!("login-user:{username}"), 20).await?;
    let permit = state
        .password_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::RateLimited)?;
    let user: Option<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id,password_hash FROM users WHERE username=$1")
            .bind(&username)
            .fetch_optional(&state.db)
            .await?;
    // Do comparable password work even when the username does not exist.
    let hash = user.as_ref().and_then(|(_, h)| h.clone());
    let valid = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match hash {
            Some(hash) => PasswordHash::new(&hash).ok().is_some_and(|h| {
                Argon2::default()
                    .verify_password(input.password.as_bytes(), &h)
                    .is_ok()
            }),
            None => {
                let _ = Argon2::default()
                    .hash_password(input.password.as_bytes(), &SaltString::generate(&mut OsRng));
                false
            }
        }
    })
    .await
    .map_err(anyhow::Error::from)?;
    if !valid {
        return Err(AppError::Unauthorized);
    }
    issue_session(&state, user.ok_or(AppError::Unauthorized)?.0, &username).await
}

async fn issue_session(state: &AppState, id: Uuid, username: &str) -> Result<Json<Value>> {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let expiry = Utc::now() + Duration::days(30);
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM sessions WHERE user_id=$1 AND expires_at<=now()")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO sessions(token_hash,user_id,expires_at) VALUES($1,$2,$3)")
        .bind(token_hash(&token))
        .bind(id)
        .bind(expiry)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": id,"username":username,"token":token,"expires_at":expiry}),
    ))
}

pub async fn logout(
    AuthUser(_): AuthUser,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<(axum::http::HeaderMap, StatusCode)> {
    if let Some(token) = request_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token_hash=$1")
            .bind(token_hash(token))
            .execute(&state.db)
            .await?;
    }
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "set-cookie",
        "hist_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
            .parse()
            .unwrap(),
    );
    Ok((headers, StatusCode::NO_CONTENT))
}

pub fn cookie_value<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|pair| {
            pair.trim()
                .split_once('=')
                .filter(|(key, _)| *key == name)
                .map(|(_, value)| value)
        })
}

pub fn request_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    if let Some(header) = headers.get("authorization") {
        return header.to_str().ok()?.strip_prefix("Bearer ");
    }
    cookie_value(headers, "hist_session")
}

pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn client_ip(
    peer: IpAddr,
    headers: &axum::http::HeaderMap,
    trusted: &[ipnet::IpNet],
) -> IpAddr {
    if !trusted.iter().any(|net| net.contains(&peer)) {
        return peer;
    }
    let Some(value) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()) else {
        return peer;
    };
    // Walk from the actual peer backwards; never trust an attacker-supplied first entry.
    let chain: Option<Vec<IpAddr>> = value.split(',').map(|s| s.trim().parse().ok()).collect();
    chain
        .and_then(|chain| {
            chain
                .into_iter()
                .rev()
                .find(|ip| !trusted.iter().any(|net| net.contains(ip)))
        })
        .unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarded_ip_requires_a_trusted_proxy() {
        let peer = "127.0.0.1".parse().unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "1.2.3.4, 5.6.7.8, 127.0.0.1".parse().unwrap(),
        );
        assert_eq!(client_ip(peer, &headers, &[]), peer);
        assert_eq!(
            client_ip(peer, &headers, &["127.0.0.1/32".parse().unwrap()]),
            "5.6.7.8".parse::<IpAddr>().unwrap()
        );
    }
}
