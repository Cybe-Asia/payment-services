use std::sync::Arc;

use axum::{routing::get, Router};
use dotenv::{dotenv, from_filename};
use neo4rs::Graph;
use serde_json::json;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

mod clients;
mod config;
mod database;
mod dto;
mod handlers;
mod models;
mod repositories;
mod routes;
mod services;
mod utils;

use clients::xendit::XenditClient;
use config::config::load;
use database::neo4j::create_graph;
use repositories::seed::seed_fees;

#[derive(Clone)]
pub struct AppState {
    pub graph: Option<Arc<Graph>>,
    pub xendit: XenditClient,
    pub tenant_id: String,
    pub xendit_webhook_token: String,
    pub default_currency: String,
    pub default_due_hours: i64,
}

fn load_env() {
    let app_env = std::env::var("APP_ENV").unwrap_or_else(|_| "local".to_string());
    let dotenv_file = match app_env.as_str() {
        "prod" => ".env.production".to_string(),
        other => format!(".env.{}", other),
    };
    if let Ok(custom) = std::env::var("DOTENV_FILE") {
        let _ = from_filename(custom);
        return;
    }
    if from_filename(&dotenv_file).is_err() {
        let _ = dotenv();
    }
}

async fn health_check() -> axum::response::Json<serde_json::Value> {
    axum::response::Json(json!({ "status": "ok" }))
}

#[tokio::main]
async fn main() {
    load_env();
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let cfg = load();

    let graph = match create_graph(&cfg.neo4j_uri, &cfg.neo4j_user, &cfg.neo4j_password).await {
        Ok(g) => {
            let arc = Arc::new(g);
            if let Err(e) = seed_fees(&arc, &cfg.tenant_id).await {
                warn!("seed failed (continuing): {e}");
            }
            Some(arc)
        }
        Err(e) => {
            warn!("neo4j connection failed: {e}");
            None
        }
    };

    let xendit = XenditClient::new(
        &cfg.xendit_api_url,
        &cfg.xendit_api_key,
        &cfg.xendit_success_redirect_url,
        &cfg.xendit_failure_redirect_url,
    );

    let state = AppState {
        graph,
        xendit,
        tenant_id: cfg.tenant_id.clone(),
        xendit_webhook_token: cfg.xendit_webhook_token.clone(),
        default_currency: cfg.default_fee_currency.clone(),
        default_due_hours: cfg.default_fee_due_hours,
    };

    let app: Router = Router::new()
        .route("/api/v1/payments/health", get(health_check))
        .merge(routes::payment_routes::routes())
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let addr = format!("0.0.0.0:{}", cfg.server_port);
    info!("starting payment-service on {}", addr);
    let listener = TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
