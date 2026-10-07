mod admin;
mod catalog;
mod classifier;
mod config;
mod providers;
mod proxy;
mod router;
mod state;
mod ui;

use axum::routing::{get, post};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "llm2=info,tower_http=warn".to_string()),
        ))
        .init();

    let config_path = config::default_config_path();
    let cfg = config::load_config(&config_path);
    let host = std::env::var("HOST").unwrap_or_else(|_| cfg.server.host.clone());
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(cfg.server.port);
    let st = state::new_shared(cfg, config_path);

    // initial catalog load (background; serving starts immediately)
    let st_bg = st.clone();
    tokio::spawn(async move {
        admin::refresh_now(&st_bg).await;
        tracing::info!("catalog loaded");
    });
    // periodic refresh every 12h
    let st_bg2 = st.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(12 * 3600)).await;
            admin::refresh_now(&st_bg2).await;
            tracing::info!("catalog refreshed");
        }
    });

    let app = axum::Router::new()
        .route("/healthz", get(proxy::health))
        .route("/v1/chat/completions", post(proxy::chat))
        .route("/v1/embeddings", post(proxy::embeddings))
        .route("/v1/models", get(proxy::models))
        .route("/v1/decisions", post(proxy::decisions))
        .route("/v1/systemone", post(proxy::decisions))
        .route("/api/alpha/decisions", post(proxy::decisions))
        .route("/api/me", get(admin::me))
        .route("/api/stats", get(admin::stats))
        .route("/api/providers", get(admin::list_providers))
        .route("/api/providers/{id}", axum::routing::put(admin::put_provider))
        .route("/api/aliases", get(admin::list_aliases))
        .route("/api/aliases/{name}", axum::routing::put(admin::put_alias).delete(admin::del_alias))
        .route("/api/catalog", get(admin::catalog_view))
        .route("/api/reload", post(admin::reload))
        .route("/ui", get(ui::index))
        .route("/ui/", get(ui::index))
        .route("/ui/app.js", get(ui::app_js))
        .route("/favicon.ico", get(ui::favicon))
        .route("/", get(|| async { axum::response::Redirect::temporary("/ui/") }))
        .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(tower::limit::ConcurrencyLimitLayer::new(4096))
        .with_state(st);

    let addr = format!("{host}:{port}");
    tracing::info!("llm proxy listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}
