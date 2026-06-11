use neo4rs::{Graph, Query};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManualBankAccount {
    pub id: String,
    pub bank_name: String,
    pub account_name: String,
    pub account_number: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct PaymentSettings {
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "xenditEnabled")]
    pub xendit_enabled: bool,
    #[serde(rename = "manualTransferEnabled")]
    pub manual_transfer_enabled: bool,
    #[serde(rename = "qrisEnabled")]
    pub qris_enabled: bool,
    #[serde(rename = "qrisImageUrl")]
    pub qris_image_url: String,
    #[serde(rename = "qrisLabel")]
    pub qris_label: String,
    #[serde(rename = "qrisInstructions")]
    pub qris_instructions: String,
    #[serde(rename = "bankName")]
    pub bank_name: String,
    #[serde(rename = "bankAccountName")]
    pub bank_account_name: String,
    #[serde(rename = "bankAccountNumber")]
    pub bank_account_number: String,
    pub instructions: String,
    #[serde(rename = "manualBankAccounts")]
    pub manual_bank_accounts: Vec<ManualBankAccount>,
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
    pub qris_enabled: Option<bool>,
    pub qris_image_url: Option<String>,
    pub qris_label: Option<String>,
    pub qris_instructions: Option<String>,
    pub bank_name: Option<String>,
    pub bank_account_name: Option<String>,
    pub bank_account_number: Option<String>,
    pub instructions: Option<String>,
    pub manual_bank_accounts: Option<Vec<ManualBankAccount>>,
}

pub async fn get_or_seed(
    graph: &Graph,
    seed: &PaymentSettingsSeed,
) -> Result<PaymentSettings, neo4rs::Error> {
    let seeded_accounts = legacy_manual_bank_accounts(
        &seed.bank_name,
        &seed.bank_account_name,
        &seed.bank_account_number,
        &seed.instructions,
    );
    let seeded_accounts_json =
        serde_json::to_string(&seeded_accounts).unwrap_or_else(|_| "[]".to_string());
    let q = Query::new(
        "MERGE (s:PaymentSettings {tenant_id:$tenant_id}) \
         ON CREATE SET s.xendit_enabled = true, \
                       s.manual_transfer_enabled = true, \
                       s.qris_enabled = false, \
                       s.qris_image_url = '', \
                       s.qris_label = 'QRIS', \
                       s.qris_instructions = '', \
                       s.bank_name = $bank_name, \
                       s.bank_account_name = $bank_account_name, \
                       s.bank_account_number = $bank_account_number, \
                       s.instructions = $instructions, \
                       s.manual_bank_accounts_json = $manual_bank_accounts_json, \
                       s.created_at = datetime(), s.updated_at = datetime() \
         RETURN s.tenant_id AS tenant_id, \
                coalesce(s.xendit_enabled, true) AS xendit_enabled, \
                coalesce(s.manual_transfer_enabled, true) AS manual_transfer_enabled, \
                coalesce(s.qris_enabled, false) AS qris_enabled, \
                coalesce(s.qris_image_url, '') AS qris_image_url, \
                coalesce(s.qris_label, 'QRIS') AS qris_label, \
                coalesce(s.qris_instructions, '') AS qris_instructions, \
                coalesce(s.bank_name, '') AS bank_name, \
                coalesce(s.bank_account_name, '') AS bank_account_name, \
                coalesce(s.bank_account_number, '') AS bank_account_number, \
                coalesce(s.instructions, '') AS instructions, \
                coalesce(s.manual_bank_accounts_json, '') AS manual_bank_accounts_json, \
                s.updated_by AS updated_by, toString(s.updated_at) AS updated_at \
         LIMIT 1"
            .to_string(),
    )
    .param("tenant_id", seed.tenant_id.clone())
    .param("bank_name", seed.bank_name.clone())
    .param("bank_account_name", seed.bank_account_name.clone())
    .param("bank_account_number", seed.bank_account_number.clone())
    .param("instructions", seed.instructions.clone())
    .param("manual_bank_accounts_json", seeded_accounts_json);
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(PaymentSettings::from_parts(
            row.get("tenant_id")
                .unwrap_or_else(|| seed.tenant_id.clone()),
            row.get("xendit_enabled").unwrap_or(true),
            row.get("manual_transfer_enabled").unwrap_or(true),
            row.get("qris_enabled").unwrap_or(false),
            row.get("qris_image_url").unwrap_or_default(),
            row.get("qris_label").unwrap_or_else(|| "QRIS".to_string()),
            row.get("qris_instructions").unwrap_or_default(),
            row.get("bank_name").unwrap_or_default(),
            row.get("bank_account_name").unwrap_or_default(),
            row.get("bank_account_number").unwrap_or_default(),
            row.get("instructions").unwrap_or_default(),
            row.get("manual_bank_accounts_json").unwrap_or_default(),
            row.get("updated_by"),
            row.get("updated_at"),
        ))
    } else {
        Ok(PaymentSettings::from_parts(
            seed.tenant_id.clone(),
            true,
            true,
            false,
            String::new(),
            "QRIS".to_string(),
            String::new(),
            seed.bank_name.clone(),
            seed.bank_account_name.clone(),
            seed.bank_account_number.clone(),
            seed.instructions.clone(),
            serde_json::to_string(&seeded_accounts).unwrap_or_else(|_| "[]".to_string()),
            None,
            None,
        ))
    }
}

pub async fn update(
    graph: &Graph,
    tenant_id: &str,
    payload: UpdatePaymentSettings,
    actor: &str,
) -> Result<PaymentSettings, neo4rs::Error> {
    let manual_bank_accounts =
        sanitize_manual_bank_accounts(payload.manual_bank_accounts.unwrap_or_default());
    let manual_bank_accounts_json =
        serde_json::to_string(&manual_bank_accounts).unwrap_or_else(|_| "[]".to_string());
    let q = Query::new(
        "MERGE (s:PaymentSettings {tenant_id:$tenant_id}) \
         SET s.xendit_enabled = $xendit_enabled, \
             s.manual_transfer_enabled = $manual_transfer_enabled, \
             s.qris_enabled = $qris_enabled, \
             s.qris_image_url = coalesce($qris_image_url, s.qris_image_url, ''), \
             s.qris_label = coalesce($qris_label, s.qris_label, 'QRIS'), \
             s.qris_instructions = coalesce($qris_instructions, s.qris_instructions, ''), \
             s.bank_name = coalesce($bank_name, s.bank_name, ''), \
             s.bank_account_name = coalesce($bank_account_name, s.bank_account_name, ''), \
             s.bank_account_number = coalesce($bank_account_number, s.bank_account_number, ''), \
             s.instructions = coalesce($instructions, s.instructions, ''), \
             s.manual_bank_accounts_json = $manual_bank_accounts_json, \
             s.updated_by = $actor, s.updated_at = datetime() \
         RETURN s.tenant_id AS tenant_id, \
                coalesce(s.xendit_enabled, true) AS xendit_enabled, \
                coalesce(s.manual_transfer_enabled, true) AS manual_transfer_enabled, \
                coalesce(s.qris_enabled, false) AS qris_enabled, \
                coalesce(s.qris_image_url, '') AS qris_image_url, \
                coalesce(s.qris_label, 'QRIS') AS qris_label, \
                coalesce(s.qris_instructions, '') AS qris_instructions, \
                coalesce(s.bank_name, '') AS bank_name, \
                coalesce(s.bank_account_name, '') AS bank_account_name, \
                coalesce(s.bank_account_number, '') AS bank_account_number, \
                coalesce(s.instructions, '') AS instructions, \
                coalesce(s.manual_bank_accounts_json, '') AS manual_bank_accounts_json, \
                s.updated_by AS updated_by, toString(s.updated_at) AS updated_at \
         LIMIT 1"
            .to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("xendit_enabled", payload.xendit_enabled)
    .param("manual_transfer_enabled", payload.manual_transfer_enabled)
    .param("qris_enabled", payload.qris_enabled.unwrap_or(false))
    .param("qris_image_url", payload.qris_image_url.unwrap_or_default())
    .param(
        "qris_label",
        payload.qris_label.unwrap_or_else(|| "QRIS".to_string()),
    )
    .param(
        "qris_instructions",
        payload.qris_instructions.unwrap_or_default(),
    )
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
    .param("manual_bank_accounts_json", manual_bank_accounts_json)
    .param("actor", actor.to_string());
    let mut result = graph.execute(q).await?;
    let row = result
        .next()
        .await?
        .expect("PaymentSettings MERGE returned no row");
    Ok(PaymentSettings::from_parts(
        row.get("tenant_id")
            .unwrap_or_else(|| tenant_id.to_string()),
        row.get("xendit_enabled").unwrap_or(true),
        row.get("manual_transfer_enabled").unwrap_or(true),
        row.get("qris_enabled").unwrap_or(false),
        row.get("qris_image_url").unwrap_or_default(),
        row.get("qris_label").unwrap_or_else(|| "QRIS".to_string()),
        row.get("qris_instructions").unwrap_or_default(),
        row.get("bank_name").unwrap_or_default(),
        row.get("bank_account_name").unwrap_or_default(),
        row.get("bank_account_number").unwrap_or_default(),
        row.get("instructions").unwrap_or_default(),
        row.get("manual_bank_accounts_json").unwrap_or_default(),
        row.get("updated_by"),
        row.get("updated_at"),
    ))
}

impl PaymentSettings {
    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        tenant_id: String,
        xendit_enabled: bool,
        manual_transfer_enabled: bool,
        qris_enabled: bool,
        qris_image_url: String,
        qris_label: String,
        qris_instructions: String,
        bank_name: String,
        bank_account_name: String,
        bank_account_number: String,
        instructions: String,
        manual_bank_accounts_json: String,
        updated_by: Option<String>,
        updated_at: Option<String>,
    ) -> Self {
        let parsed_accounts = parse_manual_bank_accounts(&manual_bank_accounts_json);
        let manual_bank_accounts = if parsed_accounts.is_empty() {
            legacy_manual_bank_accounts(
                &bank_name,
                &bank_account_name,
                &bank_account_number,
                &instructions,
            )
        } else {
            parsed_accounts
        };

        Self {
            tenant_id,
            xendit_enabled,
            manual_transfer_enabled,
            qris_enabled,
            qris_image_url,
            qris_label,
            qris_instructions,
            bank_name,
            bank_account_name,
            bank_account_number,
            instructions,
            manual_bank_accounts,
            updated_by,
            updated_at,
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn parse_manual_bank_accounts(raw: &str) -> Vec<ManualBankAccount> {
    if raw.trim().is_empty() {
        return Vec::new();
    }

    serde_json::from_str::<Vec<ManualBankAccount>>(raw)
        .map(sanitize_manual_bank_accounts)
        .unwrap_or_default()
}

fn legacy_manual_bank_accounts(
    bank_name: &str,
    account_name: &str,
    account_number: &str,
    instructions: &str,
) -> Vec<ManualBankAccount> {
    if bank_name.trim().is_empty()
        && account_name.trim().is_empty()
        && account_number.trim().is_empty()
        && instructions.trim().is_empty()
    {
        return Vec::new();
    }

    vec![ManualBankAccount {
        id: "primary".to_string(),
        bank_name: bank_name.trim().to_string(),
        account_name: account_name.trim().to_string(),
        account_number: account_number.trim().to_string(),
        instructions: instructions.trim().to_string(),
        enabled: true,
    }]
}

fn sanitize_manual_bank_accounts(accounts: Vec<ManualBankAccount>) -> Vec<ManualBankAccount> {
    accounts
        .into_iter()
        .enumerate()
        .filter_map(|(index, account)| {
            let bank_name = account.bank_name.trim().to_string();
            let account_name = account.account_name.trim().to_string();
            let account_number = account.account_number.trim().to_string();
            let instructions = account.instructions.trim().to_string();
            if bank_name.is_empty()
                && account_name.is_empty()
                && account_number.is_empty()
                && instructions.is_empty()
            {
                return None;
            }
            let id = account.id.trim();
            Some(ManualBankAccount {
                id: if id.is_empty() {
                    format!("bank-{}", index + 1)
                } else {
                    id.to_string()
                },
                bank_name,
                account_name,
                account_number,
                instructions,
                enabled: account.enabled,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manual_bank_accounts_from_json() {
        let json = r#"[{"id":"bca","bankName":" BCA ","accountName":" TWSI ","accountNumber":" 123 ","instructions":" Main ","enabled":true}]"#;

        let accounts = parse_manual_bank_accounts(json);

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "bca");
        assert_eq!(accounts[0].bank_name, "BCA");
        assert_eq!(accounts[0].account_name, "TWSI");
        assert_eq!(accounts[0].account_number, "123");
        assert!(accounts[0].enabled);
    }

    #[test]
    fn falls_back_to_legacy_single_bank_fields() {
        let settings = PaymentSettings::from_parts(
            "TENANT-001".to_string(),
            false,
            true,
            false,
            String::new(),
            "QRIS".to_string(),
            String::new(),
            "BCA".to_string(),
            "PT TWSI Indonesia Jaya".to_string(),
            "1234567890".to_string(),
            "Use the payment reference.".to_string(),
            String::new(),
            None,
            None,
        );

        assert_eq!(settings.manual_bank_accounts.len(), 1);
        assert_eq!(settings.manual_bank_accounts[0].id, "primary");
        assert_eq!(settings.manual_bank_accounts[0].bank_name, "BCA");
    }
}
