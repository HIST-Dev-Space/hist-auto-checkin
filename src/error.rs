use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("需要登录或登录已过期")]
    Unauthorized,
    #[error("请求来源无效，请刷新页面后重试")]
    Forbidden,
    #[error("资源不存在")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("请求过于频繁，请稍后重试")]
    RateLimited,
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    SchoolSessionExchange(String),
    #[error("内部服务错误")]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(ref e) = error
            && e.is_unique_violation()
        {
            return Self::Conflict("记录已存在，或该账号已被绑定".into());
        }
        Self::Internal(error.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::SchoolSessionExchange(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(e) => {
                tracing::error!(error = %e, "internal operation failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = if matches!(&self, Self::SchoolSessionExchange(_)) {
            json!({"error":self.to_string(),"code":"school_session_exchange_failed","scan_confirmed":true})
        } else {
            json!({"error":self.to_string()})
        };
        (status, Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
