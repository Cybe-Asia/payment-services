use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post, put},
    Router,
};

use crate::handlers::{fee_handler, payment_handler};
use crate::AppState;

const PROOF_UPLOAD_MAX_BYTES: usize = 10 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/payments/methods",
            get(payment_handler::get_payment_settings_handler),
        )
        .route(
            "/api/v1/payments/invoice",
            post(payment_handler::create_invoice_handler),
        )
        .route(
            "/api/v1/payments/doku/checkout",
            post(payment_handler::create_doku_checkout_handler),
        )
        .route(
            "/api/v1/payments/offers/:offer_id/methods",
            get(payment_handler::get_offer_payment_methods_handler),
        )
        .route(
            "/api/v1/payments/offers/:offer_id/manual/methods",
            get(payment_handler::get_offer_manual_payment_methods_handler),
        )
        .route(
            "/api/v1/payments/offers/:offer_id/manual",
            post(payment_handler::create_offer_manual_payment_handler),
        )
        .route(
            "/api/v1/payments/manual",
            post(payment_handler::create_manual_payment_handler),
        )
        // Register the literal `preview` path BEFORE `:payment_id` so axum's
        // router can't mistake "preview" for a payment id.
        .route(
            "/api/v1/payments/preview",
            get(payment_handler::preview_invoice_handler),
        )
        .route(
            "/api/v1/payments/webhook/xendit",
            post(payment_handler::xendit_webhook_handler),
        )
        .route(
            "/api/v1/payments/webhook/doku",
            post(payment_handler::doku_webhook_handler),
        )
        .route(
            "/api/v1/payments/:payment_id/proofs",
            post(payment_handler::upload_manual_proof_handler)
                .layer(DefaultBodyLimit::max(PROOF_UPLOAD_MAX_BYTES)),
        )
        .route(
            "/api/v1/payments/proofs/:proof_id/download",
            get(payment_handler::download_payment_proof_handler),
        )
        .route(
            "/api/v1/payments/admin/methods",
            get(payment_handler::admin_get_payment_settings_handler)
                .put(payment_handler::admin_update_payment_settings_handler),
        )
        .route(
            "/api/v1/payments/admin/reviews",
            get(payment_handler::admin_payment_reviews_handler),
        )
        .route(
            "/api/v1/payments/admin/reviews/:payment_id",
            get(payment_handler::admin_payment_review_detail_handler),
        )
        .route(
            "/api/v1/payments/admin/reviews/:payment_id/review",
            post(payment_handler::admin_review_manual_payment_handler),
        )
        .route(
            "/api/v1/payments/admin/payments/:payment_id/reconcile/doku",
            post(payment_handler::reconcile_doku_payment_handler),
        )
        // Marketing-assisted manual payment: staff open a manual payment for
        // a lead and upload the transfer proof the parent sent them over
        // WhatsApp. Staff can only SUBMIT — approval stays on the
        // /admin/reviews routes above (finance).
        .route(
            "/api/v1/payments/admin/leads/:lead_id/manual",
            post(payment_handler::admin_assist_manual_payment_handler),
        )
        .route(
            "/api/v1/payments/admin/payments/:payment_id/proofs",
            post(payment_handler::admin_assist_proof_handler)
                .layer(DefaultBodyLimit::max(PROOF_UPLOAD_MAX_BYTES)),
        )
        .route(
            "/api/v1/payments/:payment_id",
            get(payment_handler::get_payment_handler),
        )
        .route(
            "/api/v1/payments/fees/:school_code",
            get(fee_handler::get_fee_handler),
        )
        .route(
            "/api/v1/payments/fees",
            put(fee_handler::update_fee_handler),
        )
}
