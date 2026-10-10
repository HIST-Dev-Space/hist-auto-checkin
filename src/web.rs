use crate::{
    auth,
    error::{AppError, Result},
};
use axum::{
    body::Body,
    extract::{Path, Request},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};

pub async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}
pub async fn community() -> Html<&'static str> {
    Html(include_str!("../web/community.html"))
}
pub async fn login() -> Html<&'static str> {
    Html(include_str!("../web/login.html"))
}
pub async fn tasks() -> Html<&'static str> {
    Html(include_str!("../web/tasks.html"))
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
    let api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers: &mut HeaderMap = response.headers_mut();
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "same-origin".parse().unwrap());
    headers.insert("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    if api {
        headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    Ok(response)
}
