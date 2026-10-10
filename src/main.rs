use hist_auto_ckeckin::{AppState, api, config::Config, workers};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("hist_auto_ckeckin=info,tower_http=info")),
        )
        .init();
    let config = Config::from_env()?;
    let bind = config.bind_addr;
    let db = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!().run(&db).await?;
    let state = AppState::new(db.clone(), config)?;
    let cancel = CancellationToken::new();
    let background = workers::start(state.clone(), cancel.clone());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(address = %bind, "server started");
    let shutdown = cancel.clone();
    axum::serve(
        listener,
        api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        shutdown.cancel();
    })
    .await?;
    cancel.cancel();
    for task in background {
        let _ = task.await;
    }
    db.close().await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
