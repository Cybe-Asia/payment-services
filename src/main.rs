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

use clients::doku::DokuClient;
use clients::minio::MinioClient;
use clients::xendit::XenditClient;
use config::config::load;
use database::neo4j::create_graph;
use repositories::payment_settings_repository::PaymentSettingsSeed;
use repositories::seed::seed_fees;

#[derive(Clone)]
pub struct AppState {
    pub graph: Option<Arc<Graph>>,
    pub xendit: XenditClient,
    pub doku: DokuClient,
    pub doku_client_id: String,
    pub doku_secret_key: String,
    pub legacy_parent_payments_enabled: bool,
    pub http_client: reqwest::Client,
    pub tenant_id: String,
    pub xendit_webhook_token: String,
    pub default_currency: String,
    pub default_due_hours: i64,
    pub jwt_secret: String,
    pub notification_service_url: String,
    pub frontend_url: String,
    pub minio: Option<MinioClient>,
    pub payment_settings_seed: PaymentSettingsSeed,
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
            if let Err(e) = repositories::payment_repository::init_doku_indexes(&arc).await {
                warn!("DOKU idempotency index initialization failed (continuing): {e}");
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
    let doku = DokuClient::new(
        &cfg.doku_api_url,
        &cfg.doku_client_id,
        &cfg.doku_secret_key,
        &cfg.doku_return_url,
        &cfg.doku_notification_url,
        cfg.doku_payment_method_types.clone(),
    );

    let minio = match (
        cfg.minio_endpoint.as_str(),
        cfg.minio_bucket.as_str(),
        cfg.minio_access_key.as_str(),
        cfg.minio_secret_key.as_str(),
    ) {
        (endpoint, bucket, access, secret)
            if !endpoint.is_empty()
                && !bucket.is_empty()
                && !access.is_empty()
                && !secret.is_empty() =>
        {
            info!(endpoint=%endpoint, bucket=%bucket, "minio client configured for payment proofs");
            Some(MinioClient::new(endpoint, &cfg.minio_region, access, secret, bucket).await)
        }
        _ => {
            info!("minio client NOT configured (manual proof upload returns 503)");
            None
        }
    };

    let payment_settings_seed = PaymentSettingsSeed {
        tenant_id: cfg.tenant_id.clone(),
        bank_name: cfg.manual_transfer_bank_name.clone(),
        bank_account_name: cfg.manual_transfer_account_name.clone(),
        bank_account_number: cfg.manual_transfer_account_number.clone(),
        instructions: cfg.manual_transfer_instructions.clone(),
    };

    let state = AppState {
        graph,
        xendit,
        doku,
        doku_client_id: cfg.doku_client_id.clone(),
        doku_secret_key: cfg.doku_secret_key.clone(),
        legacy_parent_payments_enabled: cfg.legacy_parent_payments_enabled,
        http_client: reqwest::Client::new(),
        tenant_id: cfg.tenant_id.clone(),
        xendit_webhook_token: cfg.xendit_webhook_token.clone(),
        default_currency: cfg.default_fee_currency.clone(),
        default_due_hours: cfg.default_fee_due_hours,
        jwt_secret: cfg.jwt_secret.clone(),
        notification_service_url: cfg.notification_service_url.clone(),
        frontend_url: cfg.frontend_url.clone(),
        minio,
        payment_settings_seed,
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
