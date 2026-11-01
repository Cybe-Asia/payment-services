use axum::{http::StatusCode, Json};
use serde::Serialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct ApiResponse<T: Serialize> {
    #[serde(rename = "responseCode")]
    pub response_code: u16,
    #[serde(rename = "responseMessage")]
    pub response_message: String,
    #[serde(rename = "responseError", skip_serializing_if = "Option::is_none")]
    pub response_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            response_code: 200,
            response_message: "success".to_string(),
            response_error: None,
            data: Some(data),
        }
    }
}

/// 200 with `data` in the standard envelope; a value that cannot be encoded is a 500.
pub fn ok<T: Serialize>(data: T) -> (StatusCode, Json<Value>) {
    match serde_json::to_value(ApiResponse::success(data)) {
        Ok(value) => (StatusCode::OK, Json(value)),
        Err(error) => {
            tracing::error!("response encoding failed: {error}");
            fail(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
        }
    }
}

/// The standard failure envelope with `responseCode` set to the HTTP status.
pub fn fail(status: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    let body = json!({
        "responseCode": status.as_u16(),
        "responseMessage": "failed",
        "responseError": msg,
    });
    (status, Json(body))
}
