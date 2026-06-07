use axum::{
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{error, info};
use utoipa::ToSchema;
use uuid::Uuid;

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
    pub currency: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct ManualPaymentResponseData {
    pub payment: crate::models::payment::Payment,
    pub settings: crate::repositories::payment_settings_repository::PaymentSettings,
}

#[derive(Serialize, ToSchema)]
pub struct DownloadUrlResponse {
    #[serde(rename = "presignedUrl")]
    pub presigned_url: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "expiresInSeconds")]
    pub expires_in_seconds: u64,
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
        default_due_hours: state.default_due_hours,
        settings_seed: state.payment_settings_seed.clone(),
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
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };

    let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
        Ok(auth) => auth,
        Err((status, msg)) => return fail(status, &msg),
    };
    if payload.admission_id.starts_with("LEAD-")
        && !parent.lead_ids.iter().any(|lead_id| lead_id == &payload.admission_id)
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

    match payment_service::create_manual_payment(ctx, &payload.admission_id, &payload.payment_type)
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
            } else if e.contains("disabled") || e.contains("no students registered") {
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
    mut multipart: Multipart,
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
    if payment.payment_method.as_deref() != Some("manual_transfer") {
        return fail(StatusCode::BAD_REQUEST, "Payment is not a manual transfer");
    }
    if payment.status == "paid" {
        return fail(StatusCode::CONFLICT, "Payment is already paid");
    }
    if payment.status == "pending_verification" {
        return fail(
            StatusCode::CONFLICT,
            "A proof is already waiting for finance review",
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
        &graph,
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
    if let Err(e) = minio.put_object(&object_key, &mime, bytes).await {
        return fail(StatusCode::BAD_GATEWAY, &format!("upload failed: {e}"));
    }

    let uploaded_by = parent.email.as_deref().unwrap_or(parent.subject.as_str());
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
        uploaded_by,
    };

    match payment_service::record_manual_proof(&graph, input).await {
        Ok(proof) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(proof)).unwrap()),
        ),
        Err(e) => {
            error!("record_manual_proof failed: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, &e)
        }
    }
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
        Ok(Some(p)) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(p)).unwrap()),
        ),
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
        Ok(settings) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(settings)).unwrap()),
        ),
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
pub struct ReviewQueueQuery {
    #[serde(default = "default_review_status")]
    pub status: String,
    #[serde(default)]
    pub school: String,
    #[serde(default)]
    pub search: String,
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
    if let Err((status, msg)) = auth::require_admin(&graph, &headers, &state.jwt_secret).await {
        return fail(status, &msg);
    }

    let limit = q.limit.clamp(1, 200);
    let offset = q.offset.max(0);
    match payment_service::list_manual_review_rows(
        &graph, &q.status, &q.school, &q.search, limit, offset,
    )
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

pub async fn admin_payment_review_detail_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    if let Err((status, msg)) = auth::require_admin(&graph, &headers, &state.jwt_secret).await {
        return fail(status, &msg);
    }
    match payment_service::get_manual_review_detail(&graph, &payment_id).await {
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
    let admin = match auth::require_admin(&graph, &headers, &state.jwt_secret).await {
        Ok(admin) => admin,
        Err((status, msg)) => return fail(status, &msg),
    };
    match payment_service::review_manual_payment(&graph, &payment_id, payload, &admin.email).await {
        Ok(payment) => (
            StatusCode::OK,
            Json(serde_json::to_value(ApiResponse::success(payment)).unwrap()),
        ),
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

    let proof =
        match crate::repositories::payment_repository::find_payment_proof_object(&graph, &proof_id)
            .await
        {
            Ok(Some(proof)) => proof,
            Ok(None) => return fail(StatusCode::NOT_FOUND, "Payment proof not found"),
            Err(e) => {
                return fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("proof lookup failed: {e}"),
                )
            }
        };

    let is_admin = auth::require_admin(&graph, &headers, &state.jwt_secret)
        .await
        .is_ok();
    if !is_admin {
        let parent = match auth::require_parent_auth(&graph, &headers, &state.jwt_secret).await {
            Ok(parent) => parent,
            Err((status, msg)) => return fail(status, &msg),
        };
        if !auth::owns_lead(&parent, proof.2.as_deref()) {
            return fail(
                StatusCode::FORBIDDEN,
                "Payment proof does not belong to the current session",
            );
        }
    }

    let ttl = 600;
    match minio.presigned_get(&proof.0, ttl).await {
        Ok(url) => {
            let data = DownloadUrlResponse {
                presigned_url: url,
                file_name: proof.1,
                expires_in_seconds: ttl,
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(ApiResponse::success(data)).unwrap()),
            )
        }
        Err(e) => fail(
            StatusCode::BAD_GATEWAY,
            &format!("download url failed: {e}"),
        ),
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
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "responseCode": 200, "responseMessage": "ok" })),
        ),
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
