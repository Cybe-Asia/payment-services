use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// FeeStructure — configurable fee catalog per school × payment_type.
/// Follows the temporal pattern from the graph-model spec §2.1.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct FeeStructure {
    #[serde(rename = "feeStructureId")]
    pub fee_structure_id: String,
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "schoolId")]
    pub school_id: String,
    #[serde(rename = "schoolCode")]
    pub school_code: String,
    #[serde(rename = "paymentType")]
    pub payment_type: String,
    pub amount: i64,
    pub currency: String,
    pub status: String,
}

/// FeeObligation — what this applicant *owes*. Per graph-model §4.8.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct FeeObligation {
    #[serde(rename = "feeObligationId")]
    pub fee_obligation_id: String,
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "obligationType")]
    pub obligation_type: String,
    #[serde(rename = "amountDue")]
    pub amount_due: i64,
    pub currency: String,
    pub status: String,
    #[serde(rename = "dueAt")]
    pub due_at: Option<String>,
}
