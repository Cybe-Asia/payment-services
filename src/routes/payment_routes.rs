use axum::{
    routing::{get, post, put},
    Router,
};

use crate::handlers::{fee_handler, payment_handler};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/payments/invoice", post(payment_handler::create_invoice_handler))
        .route("/api/v1/payments/webhook/xendit", post(payment_handler::xendit_webhook_handler))
        .route("/api/v1/payments/:payment_id", get(payment_handler::get_payment_handler))
        .route("/api/v1/payments/fees/:school_code", get(fee_handler::get_fee_handler))
        .route("/api/v1/payments/fees", put(fee_handler::update_fee_handler))
}
