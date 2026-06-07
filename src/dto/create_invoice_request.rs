use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
pub struct CreateInvoiceRequest {
    /// The Lead (admission) this payment belongs to. Comes from the
    /// setup-account URL `admissionId` query param.
    #[serde(rename = "admissionId")]
    pub admission_id: String,
    /// One of: "application_fee" | "enrolment_fee" | "capital_levy" | "term_fee".
    #[serde(rename = "paymentType", default = "default_app_fee")]
    pub payment_type: String,
    /// Optional destination bank chosen by the parent for manual transfer payments.
    #[serde(rename = "manualBankAccountId")]
    pub manual_bank_account_id: Option<String>,
}

fn default_app_fee() -> String {
    "application_fee".to_string()
}
