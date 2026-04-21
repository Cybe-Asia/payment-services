use serde::Serialize;
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
    pub fn error<E: AsRef<str>>(err: E) -> ApiResponse<serde_json::Value> {
        ApiResponse {
            response_code: 0,
            response_message: "failed".to_string(),
            response_error: Some(err.as_ref().to_string()),
            data: None,
        }
    }
}
