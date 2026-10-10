pub mod api;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod error;
pub mod schedule;
pub mod school;
pub mod school_login;
pub mod web;
pub mod weixin;
pub mod workers;

use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<config::Config>,
    pub secrets: crypto::Secrets,
    pub weixin: weixin::WeixinClient,
    pub password_slots: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    pub fn new(db: PgPool, config: config::Config) -> anyhow::Result<Self> {
        Ok(Self {
            secrets: crypto::Secrets::new(&config.credential_key)?,
            weixin: weixin::WeixinClient::new(&config.weixin_base_url, &config.weixin_version)?,
            config: Arc::new(config),
            db,
            password_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        })
    }
}
