use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use tracing::error;
use uuid::Uuid;

use crate::dto::update_fee_request::UpdateFeeRequest;
use crate::repositories::{fee_structure_repository, school_repository};
use crate::utils::auth;
use crate::utils::response::ApiResponse;
use crate::AppState;

#[derive(Deserialize)]
pub struct FeeQuery {
    #[serde(default = "default_app_fee")]
    pub payment_type: String,
}

fn default_app_fee() -> String {
    "application_fee".to_string()
}

/// GET /api/v1/payments/fees/:school_code — returns the *current* fee for
/// a given school + payment_type (default: application_fee).
pub async fn get_fee_handler(
    State(state): State<AppState>,
    Path(school_code): Path<String>,
    Query(q): Query<FeeQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    let school_id =
        match school_repository::find_school_id_by_code(&graph, &state.tenant_id, &school_code)
            .await
        {
            Ok(Some(id)) => id,
            Ok(None) => return fail(StatusCode::NOT_FOUND, "School not found"),
            Err(e) => {
                error!("school lookup: {e}");
                return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
            }
        };
    match fee_structure_repository::find_active(
        &graph,
        &state.tenant_id,
        &school_id,
        &q.payment_type,
    )
    .await
    {
        Ok(Some(fs)) => ok(fs),
        Ok(None) => fail(
            StatusCode::NOT_FOUND,
            "No active fee structure for this school and payment type",
        ),
        Err(e) => {
            error!("fee lookup: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
    }
}

/// PUT /api/v1/payments/fees — admin updates a fee amount. Creates a new
/// active FeeStructure version, supersedes the previous one (audit trail).
pub async fn update_fee_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdateFeeRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(graph) = state.graph.clone() else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    };
    if let Err((status, msg)) =
        auth::require_admin(&graph, &headers, &state.jwt_secret, &state.tenant_id).await
    {
        return fail(status, &msg);
    }
    let school_id = match school_repository::find_school_id_by_code(
        &graph,
        &state.tenant_id,
        &payload.school_code,
    )
    .await
    {
        Ok(Some(id)) => id,
        Ok(None) => return fail(StatusCode::NOT_FOUND, "School not found"),
        Err(e) => {
            error!("school lookup: {e}");
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
        }
    };
    let new_id = format!(
        "FEE-{}-{}-{}",
        payload.school_code,
        payload.payment_type.to_uppercase(),
        Uuid::new_v4()
    );
    match fee_structure_repository::supersede_amount(
        &graph,
        &state.tenant_id,
        &school_id,
        &payload.payment_type,
        payload.amount,
        &payload.currency,
        &new_id,
    )
    .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "responseCode": 200,
                "responseMessage": "success",
                "data": {
                    "feeStructureId": new_id,
                    "schoolCode": payload.school_code,
                    "paymentType": payload.payment_type,
                    "amount": payload.amount,
                    "currency": payload.currency,
                    "status": "active"
                }
            })),
        ),
        Err(e) => {
            error!("fee update: {e}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
    }
}

fn ok<T: serde::Serialize>(data: T) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(serde_json::to_value(ApiResponse::success(data)).unwrap()),
    )
}

fn fail(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    let body: ApiResponse<serde_json::Value> = ApiResponse::<serde_json::Value>::error(msg);
    let mut value = serde_json::to_value(body).unwrap();
    value["responseCode"] = serde_json::json!(status.as_u16());
    (status, Json(value))
}
