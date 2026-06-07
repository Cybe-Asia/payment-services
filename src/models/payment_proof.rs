use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct PaymentProof {
    #[serde(rename = "paymentProofId")]
    pub payment_proof_id: String,
    #[serde(rename = "paymentId")]
    pub payment_id: String,
    pub status: String,
    #[serde(rename = "amountSubmitted")]
    pub amount_submitted: i64,
    #[serde(rename = "amountVerified")]
    pub amount_verified: Option<i64>,
    #[serde(rename = "paidAt")]
    pub paid_at: Option<String>,
    #[serde(rename = "payerName")]
    pub payer_name: Option<String>,
    #[serde(rename = "payerBank")]
    pub payer_bank: Option<String>,
    #[serde(rename = "referenceNumber")]
    pub reference_number: Option<String>,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: i64,
    #[serde(rename = "documentHash")]
    pub document_hash: String,
    #[serde(rename = "uploadedBy")]
    pub uploaded_by: Option<String>,
    #[serde(rename = "uploadedAt")]
    pub uploaded_at: String,
    #[serde(rename = "reviewedBy")]
    pub reviewed_by: Option<String>,
    #[serde(rename = "reviewedAt")]
    pub reviewed_at: Option<String>,
    #[serde(rename = "reviewNote")]
    pub review_note: Option<String>,
}
