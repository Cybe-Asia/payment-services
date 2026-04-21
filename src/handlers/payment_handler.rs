use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Serialize;
use tracing::{error, info};
use utoipa::ToSchema;

use crate::clients::xendit::verify_webhook;
use crate::dto::create_invoice_request::CreateInvoiceRequest;
use crate::dto::webhook::XenditInvoiceWebhook;
use crate::services::payment_service::{self, PaymentContext};
use crate::utils::response::ApiResponse;
use crate::AppState;

#[derive(Serialize, ToSchema)]
pub struct CreateInvoiceResponseData {
    #[serde(rename = "paymentId")]
    pub payment_id: String,
    #[serde(rename = "hostedInvoiceUrl")]
    pub hosted_invoice_url: String,
    pub amount: i64,
    pub currency: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

#[utoipa::path(post, path = "/api/v1/payments/invoice")]
pub async fn create_invoice_handler(
    State(state): State<AppState>,
    Json(payload): Json<CreateInvoiceRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let ctx = PaymentContext {
        graph,
        xendit: &state.xendit,
        tenant_id: &state.tenant_id,
        default_currency: &state.default_currency,
        default_due_hours: state.default_due_hours,
    };
    match payment_service::create_invoice(ctx, &payload.admission_id, &payload.payment_type).await {
        Ok(outcome) => {
            let data = CreateInvoiceResponseData {
                payment_id: outcome.payment_id,
                hosted_invoice_url: outcome.hosted_invoice_url,
                amount: outcome.amount,
                currency: outcome.currency,
                expires_at: outcome.expires_at,
            };
            (StatusCode::OK, Json(serde_json::to_value(ApiResponse::success(data)).unwrap()))
        }
        Err(e) => {
            error!("create_invoice failed: {e}");
            let status = if e.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            fail(status, &e)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/payments/{payment_id}")]
pub async fn get_payment_handler(
    State(state): State<AppState>,
    Path(payment_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    match payment_service::fetch_payment_refreshed(&graph, &state.xendit, &payment_id).await {
        Ok(Some(p)) => (StatusCode::OK, Json(serde_json::to_value(ApiResponse::success(p)).unwrap())),
        Ok(None) => fail(StatusCode::NOT_FOUND, "Payment not found"),
        Err(e) => {
            error!("fetch_payment failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/payments/webhook/xendit")]
pub async fn xendit_webhook_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<XenditInvoiceWebhook>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Xendit webhook auth = static "x-callback-token" header matching
    // our configured webhook token.
    let provided = headers
        .get("x-callback-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !verify_webhook(provided, &state.xendit_webhook_token) {
        error!("xendit webhook: bad token");
        return fail(StatusCode::UNAUTHORIZED, "Invalid callback token");
    }

    info!(external_id=%payload.external_id, status=%payload.status, "xendit webhook received");

    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    match payment_service::handle_webhook(
        &graph,
        &payload.external_id,
        &payload.status,
        payload.payment_method.as_deref(),
        payload.payment_id.as_deref(),
    )
    .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "responseCode": 200, "responseMessage": "ok" }))),
        Err(e) => {
            error!("webhook handling failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

fn fail(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    let body: ApiResponse<serde_json::Value> = ApiResponse::<serde_json::Value>::error(msg);
    let mut value = serde_json::to_value(body).unwrap();
    value["responseCode"] = serde_json::json!(status.as_u16());
    (status, Json(value))
}
