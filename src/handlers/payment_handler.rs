use axum::body::Bytes;
use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::BytesMut;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::clients::doku::{verify_non_snap_webhook, DokuWebhook};
use crate::clients::xendit::verify_webhook;
use crate::dto::create_invoice_request::CreateInvoiceRequest;
use crate::dto::webhook::XenditInvoiceWebhook;
use crate::repositories::payment_repository::CreateProofInput;
use crate::repositories::payment_settings_repository::UpdatePaymentSettings;
use crate::services::payment_service::{self, PaymentContext, ReviewManualPaymentRequest};
use crate::utils::auth;
use crate::utils::response::ApiResponse;
use crate::AppState;

#[derive(Serialize, ToSchema)]
pub struct CreateInvoiceResponseData {
    #[serde(rename = "paymentId")]
    pub payment_id: String,
    #[serde(rename = "hostedInvoiceUrl")]
    pub hosted_invoice_url: String,
    pub amount: i64,
    #[serde(rename = "grossAmount")]
    pub gross_amount: i64,
    #[serde(rename = "discountAmount")]
    pub discount_amount: i64,
    #[serde(rename = "netAmount")]
    pub net_amount: i64,
    #[serde(rename = "promotionCode", skip_serializing_if = "Option::is_none")]
    pub promotion_code: Option<String>,
    #[serde(rename = "promotionRuleId", skip_serializing_if = "Option::is_none")]
    pub promotion_rule_id: Option<String>,
    #[serde(rename = "lineItems")]
    pub line_items: Vec<payment_service::PaymentLineItem>,
    pub currency: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct ManualPaymentResponseData {
    pub payment: crate::models::payment::Payment,
    pub settings: crate::repositories::payment_settings_repository::PaymentSettings,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDokuCheckoutBody {
    pub offer_id: String,
    #[serde(default = "default_doku_attempt")]
    pub attempt: u32,
}

fn default_doku_attempt() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOfferManualPaymentBody {
    pub manual_bank_account_id: Option<String>,
}

pub async fn create_doku_checkout_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateDokuCheckoutBody>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
        Ok(value) => value,
        Err((status, message)) => return fail(status, &message),
    };
    match payment_service::create_doku_checkout(
        &graph,
        &state.doku,
        &state.tenant_id,
        &payload.offer_id,
        &parent.lead_ids,
        payload.attempt,
        &state.payment_settings_seed,
    )
    .await
    {
        Ok(outcome) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(outcome)).unwrap()),
        ),
        Err(message) => {
            let status = if message.contains("not found") {
                StatusCode::NOT_FOUND
            } else if message.contains("prerequisites") || message.contains("disabled") {
                StatusCode::SERVICE_UNAVAILABLE
            } else if message.contains("snapshot")
                || message.contains("attempt")
                || message.contains("payment method")
            {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_GATEWAY
            };
            fail(status, &message)
        }
    }
}

pub async fn get_offer_payment_methods_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(offer_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
        Ok(value) => value,
        Err((status, message)) => return fail(status, &message),
    };
    match payment_service::offer_payment_settings(
        &graph,
        &state.tenant_id,
        &offer_id,
        &parent.lead_ids,
        &state.payment_settings_seed,
    )
    .await
    {
        Ok(settings) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(settings)).unwrap()),
        ),
        Err(message) if message.contains("not found") => fail(StatusCode::NOT_FOUND, &message),
        Err(message) => fail(StatusCode::CONFLICT, &message),
    }
}

pub async fn get_offer_manual_payment_methods_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(offer_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
        Ok(value) => value,
        Err((status, message)) => return fail(status, &message),
    };
    match payment_service::offer_manual_payment_settings(
        &graph,
        &state.tenant_id,
        &offer_id,
        &parent.lead_ids,
        &state.payment_settings_seed,
    )
    .await
    {
        Ok(settings) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(settings)).unwrap()),
        ),
        Err(message) => {
            let status = if message.contains("not found") {
                StatusCode::NOT_FOUND
            } else if message.contains("disabled") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::CONFLICT
            };
            fail(status, &message)
        }
    }
}

pub async fn create_offer_manual_payment_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(offer_id): Path<String>,
    Json(payload): Json<CreateOfferManualPaymentBody>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
        Ok(value) => value,
        Err((status, message)) => return fail(status, &message),
    };
    match payment_service::create_offer_manual_payment(
        &graph,
        &state.tenant_id,
        &offer_id,
        &parent.lead_ids,
        payload.manual_bank_account_id.as_deref(),
        state.default_due_hours,
        &state.payment_settings_seed,
    )
    .await
    {
        Ok(outcome) => {
            let data = ManualPaymentResponseData {
                payment: outcome.payment,
                settings: outcome.settings,
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(data)).unwrap()),
            )
        }
        Err(message) => {
            let status = if message.contains("not found") {
                StatusCode::NOT_FOUND
            } else if message.contains("disabled") || message.contains("not configured") {
                StatusCode::SERVICE_UNAVAILABLE
            } else if message.contains("snapshot")
                || message.contains("payment method")
                || message.contains("terminal")
            {
                StatusCode::CONFLICT
            } else if message.contains("bank account") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            fail(status, &message)
        }
    }
}

pub async fn doku_webhook_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    };
    let client_id = header("Client-Id");
    let request_id = header("Request-Id");
    let timestamp = header("Request-Timestamp");
    let signature = header("Signature");
    if client_id != state.doku_client_id
        || request_id.is_empty()
        || !verify_non_snap_webhook(
            signature,
            client_id,
            request_id,
            timestamp,
            "/api/v1/payments/webhook/doku",
            &body,
            &state.doku_secret_key,
        )
    {
        return fail(StatusCode::UNAUTHORIZED, "Invalid DOKU signature");
    }
    let received_at = match DateTime::parse_from_rfc3339(timestamp) {
        Ok(value) => value.with_timezone(&Utc),
        Err(_) => return fail(StatusCode::UNAUTHORIZED, "Invalid DOKU timestamp"),
    };
    if (Utc::now() - received_at).num_seconds().abs() > 300 {
        return fail(StatusCode::UNAUTHORIZED, "Expired DOKU timestamp");
    }
    let webhook: DokuWebhook = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return fail(StatusCode::BAD_REQUEST, "Invalid DOKU callback body"),
    };
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    match payment_service::handle_doku_webhook(&graph, request_id, &webhook).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"responseCode": 200, "responseMessage": "ok"})),
        ),
        Err(message) if message.contains("mismatch") => fail(StatusCode::CONFLICT, &message),
        Err(message) if message.contains("not found") => fail(StatusCode::NOT_FOUND, &message),
        Err(message) => fail(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
}

pub async fn reconcile_doku_payment_handler(
    State(state): State<AppState>,
    Path(payment_id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    if let Err((status, message)) =
        auth::require_finance(&graph, &headers, &state.jwt_secret, true).await
    {
        return fail(status, &message);
    }
    match payment_service::reconcile_doku_payment(
        &graph,
        &state.doku,
        &state.tenant_id,
        &payment_id,
    )
    .await
    {
        Ok(payment) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(payment)).unwrap()),
        ),
        Err(message) if message.contains("not found") => fail(StatusCode::NOT_FOUND, &message),
        Err(message) if message.contains("mismatch") || message.contains("missing") => {
            fail(StatusCode::CONFLICT, &message)
        }
        Err(message) if message.contains("prerequisites") => {
            fail(StatusCode::SERVICE_UNAVAILABLE, &message)
        }
        Err(message) => fail(StatusCode::BAD_GATEWAY, &message),
    }
}

#[utoipa::path(post, path = "/api/v1/payments/invoice")]
pub async fn create_invoice_handler(
    State(state): State<AppState>,
    Json(payload): Json<CreateInvoiceRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !state.legacy_parent_payments_enabled {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Legacy parent payment providers are disabled; use an accepted-offer DOKU checkout",
        );
    }
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let ctx = PaymentContext {
        graph,
        xendit: &state.xendit,
        tenant_id: &state.tenant_id,
        default_due_hours: state.default_due_hours,
        settings_seed: state.payment_settings_seed.clone(),
    };
    match payment_service::create_invoice(ctx, &payload.admission_id, &payload.payment_type).await {
        Ok(outcome) => {
            let data = CreateInvoiceResponseData {
                payment_id: outcome.payment_id,
                hosted_invoice_url: outcome.hosted_invoice_url,
                amount: outcome.amount,
                gross_amount: outcome.gross_amount,
                discount_amount: outcome.discount_amount,
                net_amount: outcome.net_amount,
                promotion_code: outcome.promotion_code,
                promotion_rule_id: outcome.promotion_rule_id,
                line_items: outcome.line_items,
                currency: outcome.currency,
                expires_at: outcome.expires_at,
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(data)).unwrap()),
            )
        }
        Err(e) => {
            error!("create_invoice failed: {e}");
            let status = if e.contains("not found") {
                StatusCode::NOT_FOUND
            } else if e.contains("no students registered") || e.contains("disabled") {
                // Caller should have students attached before they can pay.
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            fail(status, &e)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/payments/manual")]
pub async fn create_manual_payment_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateInvoiceRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !state.legacy_parent_payments_enabled {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Legacy parent payment providers are disabled; use an accepted-offer DOKU checkout",
        );
    }
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };

    let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
        Ok(auth) => auth,
        Err((status, msg)) => return fail(status, &msg),
    };
    if payload.admission_id.starts_with("LEAD-")
        && !parent
            .lead_ids
            .iter()
            .any(|lead_id| lead_id == &payload.admission_id)
    {
        return fail(
            StatusCode::FORBIDDEN,
            "Admission does not belong to the current session",
        );
    }

    let ctx = PaymentContext {
        graph: graph.clone(),
        xendit: &state.xendit,
        tenant_id: &state.tenant_id,
        default_due_hours: state.default_due_hours,
        settings_seed: state.payment_settings_seed.clone(),
    };

    match payment_service::create_manual_payment(
        ctx,
        &payload.admission_id,
        &payload.payment_type,
        payload.manual_bank_account_id.as_deref(),
    )
    .await
    {
        Ok(outcome) => {
            if !auth::owns_lead(&parent, outcome.payment.lead_id.as_deref()) {
                return fail(
                    StatusCode::FORBIDDEN,
                    "Payment does not belong to the current session",
                );
            }
            let data = ManualPaymentResponseData {
                payment: outcome.payment,
                settings: outcome.settings,
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(data)).unwrap()),
            )
        }
        Err(e) => {
            error!("create_manual_payment failed: {e}");
            let status = if e.contains("not found") {
                StatusCode::NOT_FOUND
            } else if e.contains("disabled")
                || e.contains("no students registered")
                || e.contains("bank account")
                || e.contains("not configured")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            fail(status, &e)
        }
    }
}

pub async fn upload_manual_proof_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    multipart: Multipart,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let Some(minio) = state.minio.clone() else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Object storage not configured",
        );
    };

    let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
        Ok(auth) => auth,
        Err((status, msg)) => return fail(status, &msg),
    };

    let payment = match payment_service::fetch_payment(&graph, &payment_id).await {
        Ok(Some(payment)) => payment,
        Ok(None) => return fail(StatusCode::NOT_FOUND, "Payment not found"),
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    if !auth::owns_lead(&parent, payment.lead_id.as_deref()) {
        return fail(
            StatusCode::FORBIDDEN,
            "Payment does not belong to the current session",
        );
    }
    if payment.tenant_id != state.tenant_id {
        return fail(StatusCode::NOT_FOUND, "Payment not found");
    }

    let uploaded_by = parent
        .email
        .clone()
        .unwrap_or_else(|| parent.subject.clone());
    process_manual_proof_upload(
        &graph,
        minio,
        payment,
        multipart,
        uploaded_by,
        state.tenant_id,
    )
    .await
}

/// Shared core of the manual-proof upload: status guards, multipart parsing,
/// duplicate detection, MinIO write, and the `pending_verification`
/// transition. Callers do auth first — the parent path checks lead
/// ownership, the assisted path checks staff roles — then hand over here so
/// both proofs land in the finance review queue byte-identically.
async fn process_manual_proof_upload(
    graph: &neo4rs::Graph,
    minio: crate::clients::minio::MinioClient,
    payment: crate::models::payment::Payment,
    mut multipart: Multipart,
    uploaded_by: String,
    tenant_id: String,
) -> (StatusCode, Json<serde_json::Value>) {
    let payment_id = payment.payment_id.clone();
    if payment.payment_method.as_deref() != Some("manual_transfer") {
        return fail(StatusCode::BAD_REQUEST, "Payment is not a manual transfer");
    }
    if !crate::repositories::payment_repository::manual_proof_upload_allowed(&payment.status) {
        return fail(
            StatusCode::CONFLICT,
            "Payment no longer accepts transfer proof",
        );
    }

    let mut amount_submitted: Option<i64> = None;
    let mut paid_at: Option<String> = None;
    let mut payer_name: Option<String> = None;
    let mut payer_bank: Option<String> = None;
    let mut reference_number: Option<String> = None;
    let mut file_name: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut buffer = BytesMut::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "amountSubmitted" | "amount_submitted" => {
                amount_submitted = field
                    .text()
                    .await
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok());
            }
            "paidAt" | "paid_at" => {
                paid_at = field.text().await.ok().filter(|s| !s.trim().is_empty())
            }
            "payerName" | "payer_name" => {
                payer_name = field.text().await.ok().filter(|s| !s.trim().is_empty())
            }
            "payerBank" | "payer_bank" => {
                payer_bank = field.text().await.ok().filter(|s| !s.trim().is_empty())
            }
            "referenceNumber" | "reference_number" => {
                reference_number = field.text().await.ok().filter(|s| !s.trim().is_empty());
            }
            "file" => {
                file_name = field.file_name().map(|s| s.to_string());
                mime_type = field.content_type().map(|s| s.to_string());
                match field.bytes().await {
                    Ok(bytes) => buffer.extend_from_slice(&bytes),
                    Err(e) => {
                        return fail(StatusCode::BAD_REQUEST, &format!("file read failed: {e}"))
                    }
                }
            }
            _ => {}
        }
    }

    let Some(amount_submitted) = amount_submitted else {
        return fail(StatusCode::BAD_REQUEST, "amountSubmitted is required");
    };
    if amount_submitted <= 0 {
        return fail(
            StatusCode::BAD_REQUEST,
            "amountSubmitted must be greater than zero",
        );
    }
    // Every field on the transfer-proof form is mandatory: an incomplete
    // proof can't be reconciled by finance, so reject it at the boundary
    // rather than let a half-filled record into the review queue. The
    // frontend enforces the same set — these are the server-side backstop.
    if paid_at.is_none() {
        return fail(StatusCode::BAD_REQUEST, "paidAt is required");
    }
    if payer_name.is_none() {
        return fail(StatusCode::BAD_REQUEST, "payerName is required");
    }
    if payer_bank.is_none() {
        return fail(StatusCode::BAD_REQUEST, "payerBank is required");
    }
    if reference_number.is_none() {
        return fail(StatusCode::BAD_REQUEST, "referenceNumber is required");
    }
    if buffer.is_empty() {
        return fail(StatusCode::BAD_REQUEST, "file field missing or empty");
    }
    let mime = mime_type.unwrap_or_else(|| "application/octet-stream".to_string());
    if !is_allowed_proof_mime(&mime) {
        return fail(
            StatusCode::BAD_REQUEST,
            "proof file must be PDF, JPEG, PNG, or WebP",
        );
    }

    let bytes = buffer.freeze();
    let size_bytes = bytes.len() as i64;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = hex::encode(hasher.finalize());
    match crate::repositories::payment_repository::has_duplicate_proof_hash(
        graph,
        &payment_id,
        &hash,
    )
    .await
    {
        Ok(true) => return fail(StatusCode::CONFLICT, "This proof file was already uploaded"),
        Ok(false) => {}
        Err(e) => {
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("duplicate check failed: {e}"),
            )
        }
    }

    let proof_id = format!("PPROOF-{}", Uuid::new_v4());
    let file_name = file_name.unwrap_or_else(|| "payment-proof".to_string());
    let object_key = format!("school-test/payments/{}/{}", payment_id, proof_id);
    if minio
        .put_encrypted_document(&object_key, bytes)
        .await
        .is_err()
    {
        return fail(StatusCode::BAD_GATEWAY, "Payment proof upload failed");
    }

    let input = CreateProofInput {
        payment_proof_id: &proof_id,
        payment_id: &payment_id,
        amount_submitted,
        paid_at: paid_at.as_deref(),
        payer_name: payer_name.as_deref(),
        payer_bank: payer_bank.as_deref(),
        reference_number: reference_number.as_deref(),
        object_key: &object_key,
        file_name: &file_name,
        mime_type: &mime,
        size_bytes,
        document_hash: &hash,
        uploaded_by: &uploaded_by,
        tenant_id: &tenant_id,
        lead_id: payment.lead_id.as_deref().unwrap_or(""),
    };

    match payment_service::record_manual_proof(graph, input).await {
        Ok(proof) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(proof)).unwrap()),
        ),
        Err(e) => {
            let _ = minio.delete_object(&object_key).await;
            error!("record_manual_proof failed: {e}");
            let status = if e.contains("no longer accepts") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            fail(status, &e)
        }
    }
}

// ---------- Marketing-assisted payment (staff submits, finance verifies) ----
//
// The Indonesian manual-transfer reality: the parent wires the money to the
// school's account and WhatsApps the receipt photo to their marketing
// contact. These endpoints let that staffer submit the evidence on the
// family's behalf. Crucially they can only SUBMIT — the proof lands in the
// same `pending_verification` finance queue as a parent upload, and only
// finance (require_admin) can approve. Maker-checker preserved.

#[derive(Deserialize, ToSchema)]
pub struct AssistManualPaymentRequest {
    #[serde(rename = "paymentType", default = "default_payment_type")]
    pub payment_type: String,
    #[serde(rename = "manualBankAccountId")]
    pub manual_bank_account_id: Option<String>,
}

/// POST /api/v1/payments/admin/leads/{lead_id}/manual — staff opens (or
/// resumes) a manual-transfer payment for a lead, mirroring what the parent's
/// own "pay by bank transfer" click does.
#[utoipa::path(post, path = "/api/v1/payments/admin/leads/{lead_id}/manual")]
pub async fn admin_assist_manual_payment_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lead_id): Path<String>,
    Json(payload): Json<AssistManualPaymentRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let staff = match auth::require_staff(&graph, &headers, &state.jwt_secret).await {
        Ok(auth) => auth,
        Err((status, msg)) => return fail(status, &msg),
    };

    let ctx = PaymentContext {
        graph: graph.clone(),
        xendit: &state.xendit,
        tenant_id: &state.tenant_id,
        default_due_hours: state.default_due_hours,
        settings_seed: state.payment_settings_seed.clone(),
    };
    match payment_service::create_manual_payment(
        ctx,
        &lead_id,
        &payload.payment_type,
        payload.manual_bank_account_id.as_deref(),
    )
    .await
    {
        Ok(outcome) => {
            emit_assist_audit(
                graph,
                staff.email,
                "payment.manual.assisted",
                lead_id.clone(),
            );
            let data = ManualPaymentResponseData {
                payment: outcome.payment,
                settings: outcome.settings,
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(data)).unwrap()),
            )
        }
        Err(e) => {
            error!("assisted create_manual_payment failed: {e}");
            let status = if e.contains("not found") {
                StatusCode::NOT_FOUND
            } else if e.contains("disabled")
                || e.contains("no students registered")
                || e.contains("bank account")
                || e.contains("not configured")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            fail(status, &e)
        }
    }
}

/// POST /api/v1/payments/admin/payments/{payment_id}/proofs — staff uploads
/// the transfer receipt the parent sent them. Same guards, dedupe, storage,
/// and `pending_verification` transition as the parent upload; `uploaded_by`
/// records the staff email so finance sees who submitted it.
pub async fn admin_assist_proof_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    multipart: Multipart,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let Some(minio) = state.minio.clone() else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Object storage not configured",
        );
    };
    let staff = match auth::require_staff(&graph, &headers, &state.jwt_secret).await {
        Ok(auth) => auth,
        Err((status, msg)) => return fail(status, &msg),
    };

    let payment = match payment_service::fetch_payment(&graph, &payment_id).await {
        Ok(Some(payment)) => payment,
        Ok(None) => return fail(StatusCode::NOT_FOUND, "Payment not found"),
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    if payment.tenant_id != state.tenant_id {
        return fail(StatusCode::NOT_FOUND, "Payment not found");
    }
    let lead_id = payment.lead_id.clone().unwrap_or_default();

    let response = process_manual_proof_upload(
        &graph,
        minio,
        payment,
        multipart,
        staff.email.clone(),
        state.tenant_id.clone(),
    )
    .await;
    if response.0 == StatusCode::OK {
        emit_assist_audit(graph, staff.email, "payment.proof.assisted", lead_id);
    }
    response
}

/// Fire-and-forget AuditEvent write onto the shared graph — the same node
/// shape admission-services' `audit_repository::emit_staff` creates, so
/// assisted payment actions show up in the admin audit log alongside every
/// other staff mutation. A failed write never blocks the payment flow.
fn emit_assist_audit(
    graph: std::sync::Arc<neo4rs::Graph>,
    actor_email: String,
    action: &'static str,
    target_id: String,
) {
    tokio::spawn(async move {
        let q = neo4rs::Query::new(
            "CREATE (:AuditEvent { \
                event_id: $id, actor_lead_id: '', actor_email: $actor_email, \
                action: $action, target_type: 'lead', target_id: $tid, \
                diff: '', created_at: datetime() \
             })"
            .to_string(),
        )
        .param("id", format!("AUDIT-{}", Uuid::new_v4()))
        .param("actor_email", actor_email)
        .param("action", action.to_string())
        .param("tid", target_id);
        if let Err(e) = graph.run(q).await {
            warn!("assist audit write failed: {e}");
        }
    });
}

/// Query params for the invoice preview endpoint.
#[derive(Deserialize)]
pub struct PreviewQuery {
    #[serde(rename = "admissionId")]
    pub admission_id: String,
    #[serde(rename = "paymentType", default = "default_payment_type")]
    pub payment_type: String,
}

fn default_payment_type() -> String {
    "application_fee".to_string()
}

fn is_allowed_proof_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/pdf" | "image/jpeg" | "image/jpg" | "image/png" | "image/webp"
    )
}

/// GET /api/v1/payments/preview?admissionId=X&paymentType=application_fee
///
/// Returns the exact amount this Lead would be charged right now — the
/// per-student fee × the number of students registered under the Lead.
/// The frontend uses this to render the "Rp 1.000.000 × 2 students =
/// Rp 2.000.000" breakdown before the parent clicks "Pay".
#[utoipa::path(get, path = "/api/v1/payments/preview")]
pub async fn preview_invoice_handler(
    State(state): State<AppState>,
    Query(q): Query<PreviewQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    match payment_service::preview_invoice(
        &graph,
        &state.tenant_id,
        &q.admission_id,
        &q.payment_type,
    )
    .await
    {
        Ok(preview) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(preview)).unwrap()),
        ),
        Err(e) => {
            error!("preview_invoice failed: {e}");
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
        Ok(Some(p)) => {
            if p.status == "paid" {
                queue_payment_status_notification(&state, &graph, &p, "payment_approved").await;
            }
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(p)).unwrap()),
            )
        }
        Ok(None) => fail(StatusCode::NOT_FOUND, "Payment not found"),
        Err(e) => {
            error!("fetch_payment failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

pub async fn get_payment_settings_handler(
    State(state): State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    match payment_service::get_payment_settings(&graph, &state.payment_settings_seed).await {
        Ok(mut settings) => {
            if !state.legacy_parent_payments_enabled {
                settings.xendit_enabled = false;
                settings.manual_transfer_enabled = false;
                settings.qris_enabled = false;
                settings.qris_image_url.clear();
                settings.manual_bank_accounts.clear();
            }
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(settings)).unwrap()),
            )
        }
        Err(e) => {
            error!("get_payment_settings failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

pub async fn admin_get_payment_settings_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    if let Err((status, msg)) = auth::require_admin(&graph, &headers, &state.jwt_secret).await {
        return fail(status, &msg);
    }
    match payment_service::get_payment_settings(&graph, &state.payment_settings_seed).await {
        Ok(settings) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(settings)).unwrap()),
        ),
        Err(e) => {
            error!("admin_get_payment_settings failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

pub async fn admin_update_payment_settings_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdatePaymentSettings>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let admin = match auth::require_admin(&graph, &headers, &state.jwt_secret).await {
        Ok(admin) => admin,
        Err((status, msg)) => return fail(status, &msg),
    };
    match payment_service::update_payment_settings(
        &graph,
        &state.payment_settings_seed,
        payload,
        &admin.email,
    )
    .await
    {
        Ok(settings) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(settings)).unwrap()),
        ),
        Err(e) => {
            error!("admin_update_payment_settings failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQueueQuery {
    #[serde(default = "default_review_status")]
    pub status: String,
    #[serde(default)]
    pub school: String,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub date_from: String,
    #[serde(default)]
    pub date_to: String,
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_review_status() -> String {
    "pending_verification".to_string()
}

fn default_limit() -> i64 {
    50
}

pub async fn admin_payment_reviews_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReviewQueueQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    // View gate: finance roles (+ admissions managers) work this queue from
    // their role alone — no ADMIN_EMAILS entry needed.
    if let Err((status, msg)) =
        auth::require_finance(&graph, &headers, &state.jwt_secret, false).await
    {
        return fail(status, &msg);
    }

    let limit = q.limit.clamp(1, 200);
    let offset = q.offset.max(0);
    let date_from = match normalize_optional_rfc3339(&q.date_from) {
        Ok(v) => v,
        Err(msg) => return fail(StatusCode::BAD_REQUEST, msg),
    };
    let date_to = match normalize_optional_rfc3339(&q.date_to) {
        Ok(v) => v,
        Err(msg) => return fail(StatusCode::BAD_REQUEST, msg),
    };
    if date_from.is_none() ^ date_to.is_none() {
        return fail(
            StatusCode::BAD_REQUEST,
            "dateFrom and dateTo must be provided together",
        );
    }
    let filters = payment_service::PaymentReviewFilters {
        status: &q.status,
        school: &q.school,
        search: &q.search,
        date_from: date_from.as_deref().unwrap_or(""),
        date_to: date_to.as_deref().unwrap_or(""),
        sort: &q.sort,
        sort_dir: &q.dir,
    };
    match payment_service::list_manual_review_rows(&graph, &state.tenant_id, filters, limit, offset)
        .await
    {
        Ok(payload) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(payload)).unwrap()),
        ),
        Err(e) => {
            error!("admin_payment_reviews failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

fn normalize_optional_rfc3339(raw: &str) -> Result<Option<String>, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(trimmed)
        .map(|dt| Some(dt.with_timezone(&Utc).to_rfc3339()))
        .map_err(|_| "dateFrom/dateTo must be RFC3339 datetimes")
}

pub async fn admin_payment_review_detail_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    if let Err((status, msg)) =
        auth::require_finance(&graph, &headers, &state.jwt_secret, false).await
    {
        return fail(status, &msg);
    }
    match payment_service::get_manual_review_detail(&graph, &state.tenant_id, &payment_id).await {
        Ok(Some(detail)) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(detail)).unwrap()),
        ),
        Ok(None) => fail(StatusCode::NOT_FOUND, "Payment not found"),
        Err(e) => {
            error!("admin_payment_review_detail failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
}

pub async fn admin_review_manual_payment_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    Json(payload): Json<ReviewManualPaymentRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    // Approve gate: strictly finance + full-admin-like roles. Marketing and
    // admissions staff can submit evidence but never confirm money.
    let admin = match auth::require_finance(&graph, &headers, &state.jwt_secret, true).await {
        Ok(admin) => admin,
        Err((status, msg)) => return fail(status, &msg),
    };
    let decision = payload.decision.to_lowercase();
    match payment_service::review_manual_payment(
        &graph,
        &state.tenant_id,
        &payment_id,
        payload,
        &admin.email,
    )
    .await
    {
        Ok(payment) => {
            queue_payment_review_notification(&state, &graph, &payment, &decision).await;
            queue_staff_review_notification(&state, &graph, &payment, &decision).await;
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(payment)).unwrap()),
            )
        }
        Err(e) => {
            error!("admin_review_manual_payment failed: {e}");
            let status = if e.contains("not found") {
                StatusCode::NOT_FOUND
            } else if e.contains("required")
                || e.contains("lower")
                || e.contains("greater")
                || e.contains("covers")
                || e.contains("decision")
                || e.contains("already")
                || e.contains("not waiting")
                || e.contains("conflicted")
                || e.contains("not a manual")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            fail(status, &e)
        }
    }
}

pub async fn download_payment_proof_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(proof_id): Path<String>,
) -> Response {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response();
    };
    let Some(minio) = state.minio.clone() else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Payment proof storage is unavailable",
        )
        .into_response();
    };

    let proof = match crate::repositories::payment_repository::find_payment_proof_object(
        &graph,
        &proof_id,
        &state.tenant_id,
    )
    .await
    {
        Ok(Some(proof)) => proof,
        Ok(None) => return fail(StatusCode::NOT_FOUND, "Payment proof not found").into_response(),
        Err(_) => {
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Payment proof could not be loaded",
            )
            .into_response()
        }
    };

    let is_finance_viewer = auth::require_finance(&graph, &headers, &state.jwt_secret, false)
        .await
        .is_ok();
    if !is_finance_viewer {
        let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
            Ok(parent) => parent,
            Err((status, msg)) => return fail(status, &msg).into_response(),
        };
        if !auth::owns_lead(&parent, proof.3.as_deref()) {
            return fail(
                StatusCode::FORBIDDEN,
                "Payment proof does not belong to the current session",
            )
            .into_response();
        }
    }

    let bytes = match minio.get_decrypted_document(&proof.0).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "Payment proof is temporarily unavailable",
            )
            .into_response()
        }
    };
    let mut response = (StatusCode::OK, bytes).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&proof.2) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    let disposition = format!(
        "attachment; filename=\"{}\"",
        safe_evidence_filename(&proof.1)
    );
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

fn safe_evidence_filename(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                Some(character)
            } else if character.is_whitespace() {
                Some('_')
            } else {
                None
            }
        })
        .take(120)
        .collect();
    if sanitized.is_empty() {
        "payment-proof".to_string()
    } else {
        sanitized
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
        Ok(()) => {
            if let Ok(Some(payment)) =
                payment_service::fetch_payment(&graph, &payload.external_id).await
            {
                if payment.status == "paid" {
                    queue_payment_status_notification(&state, &graph, &payment, "payment_approved")
                        .await;
                }
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({ "responseCode": 200, "responseMessage": "ok" })),
            )
        }
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

async fn queue_payment_review_notification(
    state: &AppState,
    graph: &std::sync::Arc<neo4rs::Graph>,
    payment: &crate::models::payment::Payment,
    decision: &str,
) {
    let event = match decision {
        "approve" => "payment_approved",
        "underpaid" => "payment_underpaid",
        "reject" => "payment_rejected",
        _ => return,
    };
    queue_payment_status_notification(state, graph, payment, event).await;
}

/// Staff-side counterpart of the review notification. After finance decides,
/// email the people walking this family: the proof uploader when it was an
/// assisted (staff) upload, and the lead's assigned staffer. The parent is
/// excluded — they get their own notification above. Rejected/underpaid is
/// the case marketing needs most: they're the ones who chase the parent.
/// Fire-and-forget; a failure never blocks the review.
async fn queue_staff_review_notification(
    state: &AppState,
    graph: &std::sync::Arc<neo4rs::Graph>,
    payment: &crate::models::payment::Payment,
    decision: &str,
) {
    let q = neo4rs::Query::new(
        "MATCH (l:Lead)-[:MADE_PAYMENT]->(p:Payment {payment_id: $id}) \
         OPTIONAL MATCH (p)-[:HAS_PROOF]->(proof:PaymentProof) \
         WITH l, proof ORDER BY proof.uploaded_at DESC LIMIT 1 \
         RETURN l.email AS parent_email, l.parent_name AS parent_name, \
                coalesce(l.assigned_admin_email, '') AS assigned_email, \
                coalesce(proof.uploaded_by, '') AS uploaded_by"
            .to_string(),
    )
    .param("id", payment.payment_id.clone());

    let row = match graph.execute(q).await {
        Ok(mut rs) => match rs.next().await {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(err) => {
                warn!(payment_id=%payment.payment_id, error=%err, "staff review notification row failed");
                return;
            }
        },
        Err(err) => {
            warn!(payment_id=%payment.payment_id, error=%err, "staff review notification lookup failed");
            return;
        }
    };

    let parent_email = row.get::<String>("parent_email").unwrap_or_default();
    let parent_name = row.get::<String>("parent_name").unwrap_or_default();
    let assigned = row.get::<String>("assigned_email").unwrap_or_default();
    let uploaded_by = row.get::<String>("uploaded_by").unwrap_or_default();

    // Staff recipients: uploader + assigned staffer, deduped, never the
    // parent (a parent-uploaded proof has uploaded_by == parent email).
    let mut recipients: Vec<String> = Vec::new();
    for candidate in [uploaded_by, assigned] {
        let c = candidate.trim().to_lowercase();
        if c.is_empty() || c == parent_email.trim().to_lowercase() || recipients.contains(&c) {
            continue;
        }
        recipients.push(c);
    }
    if recipients.is_empty() {
        return;
    }

    let who = if parent_name.trim().is_empty() {
        "keluarga ini".to_string()
    } else {
        parent_name.trim().to_string()
    };
    let fee_label = payment_type_label(&payment.payment_type);
    let amount = format_idr(payment.amount);
    let (status_word, follow_up) = match decision {
        "approve" => (
            "disetujui",
            "Keluarga ini sudah bisa melanjutkan ke tahap berikutnya (booking jadwal tes).",
        ),
        "underpaid" => (
            "ditandai kurang bayar",
            "Mohon hubungi orang tua untuk melunasi kekurangan pembayarannya.",
        ),
        "reject" => (
            "ditolak",
            "Mohon hubungi orang tua dan unggah ulang bukti transfer yang benar.",
        ),
        _ => return,
    };

    let subject = format!("Pembayaran {who} {status_word}");
    let body = format!(
        "Halo,\n\nPembayaran {fee_label} atas nama {who} sebesar {amount} telah {status_word} oleh finance.\n\n{follow_up}\n\nBuka detail lead di portal admin untuk menindaklanjuti.\n\n— Digital Schools",
    );

    for recipient in recipients {
        dispatch_email_notification(state, recipient, subject.clone(), body.clone());
    }
}

async fn queue_payment_status_notification(
    state: &AppState,
    graph: &std::sync::Arc<neo4rs::Graph>,
    payment: &crate::models::payment::Payment,
    event: &str,
) {
    let context = match payment_service::payment_notification_context(graph, payment).await {
        Ok(Some(context)) => context,
        Ok(None) => {
            warn!(
                payment_id=%payment.payment_id,
                "Skipping payment notification because context was not found"
            );
            return;
        }
        Err(err) => {
            warn!(
                payment_id=%payment.payment_id,
                error=%err,
                "Failed to load payment notification context"
            );
            return;
        }
    };

    let channels =
        match payment_service::resolve_notification_channels(graph, &context.school, event).await {
            Ok(channels) if !channels.is_empty() => channels,
            Ok(_) => vec![payment_service::NOTIFICATION_CHANNEL_EMAIL.to_string()],
            Err(err) => {
                warn!(
                    payment_id=%payment.payment_id,
                    event=%event,
                    error=%err,
                    "Failed to resolve payment notification channels; using email"
                );
                vec![payment_service::NOTIFICATION_CHANNEL_EMAIL.to_string()]
            }
        };

    for channel in channels {
        // The durable paid receipt replaces the old fire-and-forget approval email.
        if event == "payment_approved" && payment.payment_type == "application_fee"
            && channel == payment_service::NOTIFICATION_CHANNEL_EMAIL
            && std::env::var("INVOICE_EMAIL_ENABLED").as_deref() == Ok("true") {
            continue;
        }
        if channel == payment_service::NOTIFICATION_CHANNEL_EMAIL && context.email.trim().is_empty()
        {
            warn!(
                payment_id=%payment.payment_id,
                event=%event,
                "Skipping payment email notification because parent email is empty"
            );
            continue;
        }
        if channel == payment_service::NOTIFICATION_CHANNEL_WHATSAPP
            && context.whatsapp.trim().is_empty()
        {
            warn!(
                payment_id=%payment.payment_id,
                event=%event,
                "Skipping payment WhatsApp notification because parent phone is empty"
            );
            continue;
        }

        match payment_service::mark_payment_notification_queued(
            graph,
            &payment.payment_id,
            event,
            &channel,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                info!(
                    payment_id=%payment.payment_id,
                    event=%event,
                    channel=%channel,
                    "Payment notification already queued"
                );
                continue;
            }
            Err(err) => {
                warn!(
                    payment_id=%payment.payment_id,
                    event=%event,
                    channel=%channel,
                    error=%err,
                    "Failed to mark payment notification"
                );
                continue;
            }
        }

        let body = payment_notification_body(payment, event, &context, &state.frontend_url);
        if channel == payment_service::NOTIFICATION_CHANNEL_EMAIL {
            dispatch_email_notification(
                state,
                context.email.clone(),
                payment_email_subject(payment, event, &context),
                body,
            );
        } else if channel == payment_service::NOTIFICATION_CHANNEL_WHATSAPP {
            dispatch_whatsapp_notification(state, context.whatsapp.clone(), event, body);
        }
    }
}

fn payment_notification_body(
    payment: &crate::models::payment::Payment,
    event: &str,
    context: &payment_service::PaymentNotificationContext,
    frontend_url: &str,
) -> String {
    let parent_name = display_or_parent(&context.parent_name);
    let fee_label = payment_type_label(&payment.payment_type);
    let amount = format_idr(payment.amount);
    let portal_url = parent_dashboard_url(frontend_url);
    let school = school_label(&context.school);

    match event {
        "payment_approved" => {
            let next_step = if payment.payment_type == "application_fee" {
                "Silakan login ke portal untuk booking jadwal tes masuk."
            } else {
                "Silakan login ke portal untuk melanjutkan proses pendaftaran."
            };
            format!(
                "Halo Bapak/Ibu {},\n\nPembayaran {} {} sebesar {} sudah disetujui/diterima.\n\n{}\n\n{}",
                parent_name, fee_label, school, amount, next_step, portal_url
            )
        }
        "payment_underpaid" => {
            let short_amount = payment
                .short_amount
                .unwrap_or_else(|| (payment.amount - payment.amount_verified.unwrap_or(0)).max(0));
            format!(
                "Halo Bapak/Ibu {},\n\nPembayaran {} {} sudah dicek, namun masih kurang {}. Mohon transfer kekurangan lalu unggah bukti pembayaran baru di portal.\n\n{}",
                parent_name,
                fee_label,
                school,
                format_idr(short_amount),
                portal_url
            )
        }
        "payment_rejected" => {
            let note = payment
                .rejection_reason
                .as_deref()
                .or(payment.review_note.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("-");
            format!(
                "Halo Bapak/Ibu {},\n\nBukti pembayaran {} {} belum dapat diverifikasi. Mohon unggah ulang bukti pembayaran yang benar/jelas di portal.\n\nCatatan: {}\n\n{}",
                parent_name, fee_label, school, note, portal_url
            )
        }
        _ => String::new(),
    }
}

fn payment_email_subject(
    payment: &crate::models::payment::Payment,
    event: &str,
    context: &payment_service::PaymentNotificationContext,
) -> String {
    let fee_label = payment_type_label(&payment.payment_type);
    let school = school_label(&context.school);
    match event {
        "payment_approved" => format!("Pembayaran {} {} diterima", fee_label, school),
        "payment_underpaid" => format!("Pembayaran {} {} masih kurang", fee_label, school),
        "payment_rejected" => format!("Bukti pembayaran {} {} perlu diperbaiki", fee_label, school),
        _ => "Pembaruan pembayaran Digital School".to_string(),
    }
}

fn dispatch_email_notification(state: &AppState, email: String, subject: String, body: String) {
    if email.trim().is_empty() || body.trim().is_empty() {
        return;
    }
    let html = body_to_html(&body);
    let client = state.http_client.clone();
    let url = format!(
        "{}/api/email/v1/send",
        state.notification_service_url.trim_end_matches('/')
    );
    tokio::spawn(async move {
        let response = client
            .post(url)
            .json(&serde_json::json!({
                "email": email,
                "subject": subject,
                "body": body,
                "html": html,
            }))
            .send()
            .await;
        match response {
            Ok(resp) if resp.status().is_success() => {
                info!("Queued email payment notification");
            }
            Ok(resp) => {
                warn!(
                    "Notification service rejected email payment notification with {}",
                    resp.status()
                );
            }
            Err(err) => warn!("Failed to queue email payment notification: {}", err),
        }
    });
}

fn dispatch_whatsapp_notification(state: &AppState, to: String, event: &str, body: String) {
    if to.trim().is_empty() || body.trim().is_empty() {
        return;
    }
    let client = state.http_client.clone();
    let url = format!(
        "{}/api/whatsapp/v1/send",
        state.notification_service_url.trim_end_matches('/')
    );
    let event = event.to_string();
    tokio::spawn(async move {
        let response = client
            .post(url)
            .json(&serde_json::json!({
                "to": to,
                "body": body,
                "event": event,
            }))
            .send()
            .await;
        match response {
            Ok(resp) if resp.status().is_success() => {
                info!("Queued WhatsApp payment notification");
            }
            Ok(resp) => {
                warn!(
                    "Notification service rejected WhatsApp payment notification with {}",
                    resp.status()
                );
            }
            Err(err) => warn!("Failed to queue WhatsApp payment notification: {}", err),
        }
    });
}

fn body_to_html(body: &str) -> String {
    let mut html = String::from("<div>");
    for paragraph in body.split("\n\n").filter(|value| !value.trim().is_empty()) {
        html.push_str("<p>");
        for (idx, line) in paragraph.lines().enumerate() {
            if idx > 0 {
                html.push_str("<br />");
            }
            html.push_str(&escape_html(line.trim()));
        }
        html.push_str("</p>");
    }
    html.push_str("</div>");
    html
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn parent_dashboard_url(frontend_url: &str) -> String {
    format!("{}/parent/dashboard", frontend_url.trim_end_matches('/'))
}

fn display_or_parent(value: &str) -> String {
    if value.trim().is_empty() {
        "Orang Tua".to_string()
    } else {
        value.trim().to_string()
    }
}

fn format_idr(value: i64) -> String {
    let mut digits = value.abs().to_string();
    let mut groups = Vec::new();
    while digits.len() > 3 {
        let tail = digits.split_off(digits.len() - 3);
        groups.push(tail);
    }
    groups.push(digits);
    let formatted = groups.into_iter().rev().collect::<Vec<_>>().join(".");
    if value < 0 {
        format!("-Rp {}", formatted)
    } else {
        format!("Rp {}", formatted)
    }
}

fn payment_type_label(payment_type: &str) -> &str {
    match payment_type {
        "application_fee" => "biaya pendaftaran",
        "enrolment_fee" => "biaya enrolment",
        "capital_levy" => "capital levy",
        "term_fee" => "term fee",
        _ => "pembayaran",
    }
}

fn school_label(school: &str) -> String {
    let label = match school {
        "SCH-IIHS" | "IIHS" => "IIHS",
        "SCH-IISS" | "IISS" => "IISS",
        "SCH-IIBS" | "IIBS" => "IIBS",
        value if !value.trim().is_empty() => value,
        _ => "",
    };
    if label.is_empty() {
        "".to_string()
    } else {
        format!("({})", label)
    }
}
