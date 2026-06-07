use neo4rs::{Graph, Query};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct PaymentSettings {
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "xenditEnabled")]
    pub xendit_enabled: bool,
    #[serde(rename = "manualTransferEnabled")]
    pub manual_transfer_enabled: bool,
    #[serde(rename = "bankName")]
    pub bank_name: String,
    #[serde(rename = "bankAccountName")]
    pub bank_account_name: String,
    #[serde(rename = "bankAccountNumber")]
    pub bank_account_number: String,
    pub instructions: String,
    #[serde(rename = "updatedBy")]
    pub updated_by: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PaymentSettingsSeed {
    pub tenant_id: String,
    pub bank_name: String,
    pub bank_account_name: String,
    pub bank_account_number: String,
    pub instructions: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePaymentSettings {
    pub xendit_enabled: bool,
    pub manual_transfer_enabled: bool,
    pub bank_name: Option<String>,
    pub bank_account_name: Option<String>,
    pub bank_account_number: Option<String>,
    pub instructions: Option<String>,
}

pub async fn get_or_seed(
    graph: &Graph,
    seed: &PaymentSettingsSeed,
) -> Result<PaymentSettings, neo4rs::Error> {
    let q = Query::new(
        "MERGE (s:PaymentSettings {tenant_id:$tenant_id}) \
         ON CREATE SET s.xendit_enabled = true, \
                       s.manual_transfer_enabled = true, \
                       s.bank_name = $bank_name, \
                       s.bank_account_name = $bank_account_name, \
                       s.bank_account_number = $bank_account_number, \
                       s.instructions = $instructions, \
                       s.created_at = datetime(), s.updated_at = datetime() \
         RETURN s.tenant_id AS tenant_id, \
                coalesce(s.xendit_enabled, true) AS xendit_enabled, \
                coalesce(s.manual_transfer_enabled, true) AS manual_transfer_enabled, \
                coalesce(s.bank_name, '') AS bank_name, \
                coalesce(s.bank_account_name, '') AS bank_account_name, \
                coalesce(s.bank_account_number, '') AS bank_account_number, \
                coalesce(s.instructions, '') AS instructions, \
                s.updated_by AS updated_by, toString(s.updated_at) AS updated_at \
         LIMIT 1"
            .to_string(),
    )
    .param("tenant_id", seed.tenant_id.clone())
    .param("bank_name", seed.bank_name.clone())
    .param("bank_account_name", seed.bank_account_name.clone())
    .param("bank_account_number", seed.bank_account_number.clone())
    .param("instructions", seed.instructions.clone());
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(PaymentSettings {
            tenant_id: row
                .get("tenant_id")
                .unwrap_or_else(|| seed.tenant_id.clone()),
            xendit_enabled: row.get("xendit_enabled").unwrap_or(true),
            manual_transfer_enabled: row.get("manual_transfer_enabled").unwrap_or(true),
            bank_name: row.get("bank_name").unwrap_or_default(),
            bank_account_name: row.get("bank_account_name").unwrap_or_default(),
            bank_account_number: row.get("bank_account_number").unwrap_or_default(),
            instructions: row.get("instructions").unwrap_or_default(),
            updated_by: row.get("updated_by"),
            updated_at: row.get("updated_at"),
        })
    } else {
        Ok(PaymentSettings {
            tenant_id: seed.tenant_id.clone(),
            xendit_enabled: true,
            manual_transfer_enabled: true,
            bank_name: seed.bank_name.clone(),
            bank_account_name: seed.bank_account_name.clone(),
            bank_account_number: seed.bank_account_number.clone(),
            instructions: seed.instructions.clone(),
            updated_by: None,
            updated_at: None,
        })
    }
}

pub async fn update(
    graph: &Graph,
    tenant_id: &str,
    payload: UpdatePaymentSettings,
    actor: &str,
) -> Result<PaymentSettings, neo4rs::Error> {
    let q = Query::new(
        "MERGE (s:PaymentSettings {tenant_id:$tenant_id}) \
         SET s.xendit_enabled = $xendit_enabled, \
             s.manual_transfer_enabled = $manual_transfer_enabled, \
             s.bank_name = coalesce($bank_name, s.bank_name, ''), \
             s.bank_account_name = coalesce($bank_account_name, s.bank_account_name, ''), \
             s.bank_account_number = coalesce($bank_account_number, s.bank_account_number, ''), \
             s.instructions = coalesce($instructions, s.instructions, ''), \
             s.updated_by = $actor, s.updated_at = datetime() \
         RETURN s.tenant_id AS tenant_id, \
                coalesce(s.xendit_enabled, true) AS xendit_enabled, \
                coalesce(s.manual_transfer_enabled, true) AS manual_transfer_enabled, \
                coalesce(s.bank_name, '') AS bank_name, \
                coalesce(s.bank_account_name, '') AS bank_account_name, \
                coalesce(s.bank_account_number, '') AS bank_account_number, \
                coalesce(s.instructions, '') AS instructions, \
                s.updated_by AS updated_by, toString(s.updated_at) AS updated_at \
         LIMIT 1"
            .to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("xendit_enabled", payload.xendit_enabled)
    .param("manual_transfer_enabled", payload.manual_transfer_enabled)
    .param("bank_name", payload.bank_name.unwrap_or_default())
    .param(
        "bank_account_name",
        payload.bank_account_name.unwrap_or_default(),
    )
    .param(
        "bank_account_number",
        payload.bank_account_number.unwrap_or_default(),
    )
    .param("instructions", payload.instructions.unwrap_or_default())
    .param("actor", actor.to_string());
    let mut result = graph.execute(q).await?;
    let row = result
        .next()
        .await?
        .expect("PaymentSettings MERGE returned no row");
    Ok(PaymentSettings {
        tenant_id: row
            .get("tenant_id")
            .unwrap_or_else(|| tenant_id.to_string()),
        xendit_enabled: row.get("xendit_enabled").unwrap_or(true),
        manual_transfer_enabled: row.get("manual_transfer_enabled").unwrap_or(true),
        bank_name: row.get("bank_name").unwrap_or_default(),
        bank_account_name: row.get("bank_account_name").unwrap_or_default(),
        bank_account_number: row.get("bank_account_number").unwrap_or_default(),
        instructions: row.get("instructions").unwrap_or_default(),
        updated_by: row.get("updated_by"),
        updated_at: row.get("updated_at"),
    })
}
