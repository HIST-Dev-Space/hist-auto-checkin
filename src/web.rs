use crate::{
    AppState,
    auth::{self, AuthUser},
    error::{AppError, Result},
};
use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use std::sync::LazyLock;
use uuid::Uuid;

fn render_page(template: &str, current_path: &str) -> String {
    let header = include_str!("../web/partials/header.html");
    let header = if current_path == "/community" {
        header.replace(
            "href=\"/community\"",
            "href=\"/community\" aria-current=\"page\"",
        )
    } else {
        header.to_owned()
    };
    template
        .replace("{{site-header}}", &header)
        .replace(
            "{{site-footer}}",
            include_str!("../web/partials/footer.html"),
        )
        .replace(
            "{{theme-toggle}}",
            include_str!("../web/partials/theme-toggle.html"),
        )
}

static INDEX: LazyLock<String> =
    LazyLock::new(|| render_page(include_str!("../web/index.html"), "/"));
static COMMUNITY: LazyLock<String> =
    LazyLock::new(|| render_page(include_str!("../web/community.html"), "/community"));
static LOGIN: LazyLock<String> =
    LazyLock::new(|| render_page(include_str!("../web/login.html"), "/login"));
static TASKS: LazyLock<String> =
    LazyLock::new(|| render_page(include_str!("../web/tasks.html"), "/tasks"));
static NOTIFICATIONS: LazyLock<String> =
    LazyLock::new(|| render_page(include_str!("../web/notifications.html"), "/notifications"));
static ACCOUNT: LazyLock<String> =
    LazyLock::new(|| render_page(include_str!("../web/account.html"), "/account"));

pub async fn index() -> Html<&'static str> {
    Html(INDEX.as_str())
}
pub async fn community() -> Html<&'static str> {
    Html(COMMUNITY.as_str())
}
async fn signed_in_destination(state: &AppState, user: Uuid) -> Result<&'static str> {
    let configured: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM checkin_schedules WHERE user_id=$1)")
            .bind(user)
            .fetch_one(&state.db)
            .await?;
    Ok(if configured { "/account" } else { "/tasks" })
}

pub async fn start(State(state): State<AppState>, session: Result<AuthUser>) -> Result<Redirect> {
    let destination = match session {
        Ok(AuthUser(user)) => signed_in_destination(&state, user).await?,
        Err(AppError::Unauthorized) => "/login",
        Err(error) => return Err(error),
    };
    Ok(Redirect::to(destination))
}

#[derive(Deserialize)]
pub struct LoginQuery {
    reconnect: Option<String>,
}

pub async fn login(
    State(state): State<AppState>,
    Query(query): Query<LoginQuery>,
    session: Result<AuthUser>,
) -> Result<Response> {
    if query.reconnect.is_none() {
        match session {
            Ok(AuthUser(user)) => {
                return Ok(Redirect::to(signed_in_destination(&state, user).await?).into_response());
            }
            Err(AppError::Unauthorized) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Html(LOGIN.as_str()).into_response())
}
pub async fn tasks() -> Html<&'static str> {
    Html(TASKS.as_str())
}
pub async fn notifications() -> Html<&'static str> {
    Html(NOTIFICATIONS.as_str())
}
pub async fn account() -> Html<&'static str> {
    Html(ACCOUNT.as_str())
}
pub async fn locations() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        include_str!("../web/assets/locations.json"),
    )
        .into_response()
}

pub async fn asset(Path(name): Path<String>) -> Response {
    let (mime, content): (&str, &[u8]) = match name.as_str() {
        "site.css" => (
            "text/css; charset=utf-8",
            include_str!("../web/assets/site.css").as_bytes(),
        ),
        "site.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/site.js").as_bytes(),
        ),
        "common/api.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/common/api.js").as_bytes(),
        ),
        "common/dom.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/common/dom.js").as_bytes(),
        ),
        "common/polling.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/common/polling.js").as_bytes(),
        ),
        "components/celebration.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/components/celebration.js").as_bytes(),
        ),
        "components/qr-code.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/components/qr-code.js").as_bytes(),
        ),
        "components/select.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/components/select.js").as_bytes(),
        ),
        "pages/account.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/pages/account.js").as_bytes(),
        ),
        "pages/login.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/pages/login.js").as_bytes(),
        ),
        "pages/notifications.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/pages/notifications.js").as_bytes(),
        ),
        "pages/tasks.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/pages/tasks.js").as_bytes(),
        ),
        "theme.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/theme.js").as_bytes(),
        ),
        "qrcode.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../web/assets/vendor/qrcode.js").as_bytes(),
        ),
        "app-icon.png" => ("image/png", include_bytes!("../web/assets/app-icon.png")),
        "app-icon.svg" => (
            "image/svg+xml",
            include_bytes!("../web/assets/app-icon.svg"),
        ),
        // The supplied .jpg file contains PNG data.
        "hist-group.jpg" => ("image/png", include_bytes!("../hist-group.jpg")),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        content,
    )
        .into_response()
}

pub async fn security(request: Request<Body>, next: Next) -> Result<Response> {
    let headers = request.headers();
    if request.uri().path().starts_with("/api/")
        && !["GET", "HEAD", "OPTIONS"].contains(&request.method().as_str())
        && auth::cookie_value(headers, "hist_session").is_some()
        && !headers.contains_key("authorization")
        && headers.get("x-hist-request").and_then(|h| h.to_str().ok()) != Some("1")
    {
        return Err(AppError::Forbidden);
    }
    let private = request.uri().path().starts_with("/api/")
        || matches!(request.uri().path(), "/start" | "/login");
    let mut response = next.run(request).await;
    let headers: &mut HeaderMap = response.headers_mut();
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "same-origin".parse().unwrap());
    headers.insert("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    if private {
        headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    Ok(response)
}
