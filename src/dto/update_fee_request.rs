use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
pub struct UpdateFeeRequest {
    #[serde(rename = "schoolCode")]
    pub school_code: String,
    #[serde(rename = "paymentType", default = "default_app_fee")]
    pub payment_type: String,
    pub amount: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_app_fee() -> String {
    "application_fee".to_string()
}

fn default_currency() -> String {
    "IDR".to_string()
}
