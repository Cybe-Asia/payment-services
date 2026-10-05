//! Only canonical Owner may associate a configured tenant bank with a school.
//! Finance cannot provide raw beneficiary fields or create a fallback account.
use super::{
    auth,
    model::{identifier, text},
    repository::{Error, Result},
};
use crate::repositories::payment_settings_repository::ManualBankAccount;
use neo4rs::{query, Graph};
use serde::{Deserialize, Serialize};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Configure {
    pub tenant_id: String,
    pub school_id: String,
    pub bank_account_id: String,
    pub version: i64,
}
impl Configure {
    pub fn valid(&self) -> bool {
        self.version >= 0
            && [&self.tenant_id, &self.school_id, &self.bank_account_id]
                .iter()
                .all(|v| identifier(v))
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Configured {
    pub tenant_id: String,
    pub school_id: String,
    pub version: i64,
    pub bank: ManualBankAccount,
}
pub async fn configure(g: &Graph, a: &auth::Staff, input: &Configure) -> Result<Configured> {
    super::repository::schema_ready(g).await?;
    let prefix=format!("{} WITH actor MATCH(school:School {{school_id:$school,tenant_id:$tenant}}) WHERE {} AND NOT EXISTS {{ MATCH(other_school:School {{school_id:school.school_id,tenant_id:school.tenant_id}}) WHERE other_school<>school }} MATCH(ps:PaymentSettings {{tenant_id:school.tenant_id,manual_transfer_enabled:true}}) WHERE NOT EXISTS {{ MATCH(other_settings:PaymentSettings {{tenant_id:ps.tenant_id}}) WHERE other_settings<>ps }} RETURN ps.manual_bank_accounts_json AS banks LIMIT 2",auth::STAFF,auth::scope("school.school_id","school.tenant_id"));
    let mut rows = g
        .execute(
            query(&prefix)
                .param("subject", a.subject.clone())
                .param("staff", a.id.clone())
                .param("expires", a.expires)
                .param("roles", vec!["owner"])
                .param("school", input.school_id.clone())
                .param("tenant", input.tenant_id.clone()),
        )
        .await
        .map_err(|_| Error::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::Denied)?;
    if rows.next().await.map_err(|_| Error::Unavailable)?.is_some() {
        return Err(Error::Denied);
    }
    let raw = row.get::<String>("banks").ok_or(Error::Unavailable)?;
    let banks: Vec<ManualBankAccount> =
        serde_json::from_str(&raw).map_err(|_| Error::Unavailable)?;
    let mut matches = banks
        .into_iter()
        .filter(|b| b.id == input.bank_account_id && b.enabled);
    let bank = matches.next().ok_or(Error::Denied)?;
    if matches.next().is_some() {
        return Err(Error::Unavailable);
    }
    if !text(&bank.bank_name, 128)
        || !text(&bank.account_name, 128)
        || !text(&bank.account_number, 64)
        || bank.instructions.len() > 1024
    {
        return Err(Error::Unavailable);
    }
    let bank_json = serde_json::to_string(&bank).map_err(|_| Error::Unavailable)?;
    let body=format!("{} WITH actor MATCH(school:School {{school_id:$school,tenant_id:$tenant}}) WHERE {} AND NOT EXISTS {{ MATCH(other_school:School {{school_id:school.school_id,tenant_id:school.tenant_id}}) WHERE other_school<>school }} MATCH(ps:PaymentSettings {{tenant_id:school.tenant_id,manual_transfer_enabled:true,manual_bank_accounts_json:$raw}}) WHERE NOT EXISTS {{ MATCH(other_settings:PaymentSettings {{tenant_id:ps.tenant_id}}) WHERE other_settings<>ps }} SET ps.school_billing_lock=coalesce(ps.school_billing_lock,0)+1 WITH actor,school,ps WHERE ps.manual_transfer_enabled=true AND ps.manual_bank_accounts_json=$raw OPTIONAL MATCH(existing:SchoolBillingPayee {{key:$key}}) WITH actor,school,ps,existing WHERE (existing IS NULL AND $version=0) OR existing.version=$version MERGE(config:SchoolBillingPayee {{key:$key}}) ON CREATE SET config.version=0 SET config._lock=coalesce(config._lock,0)+1 WITH config,school,ps {} WITH actor,config,school,ps WHERE config.version=$version AND school.school_id=$school AND school.tenant_id=$tenant AND ps.manual_transfer_enabled=true AND ps.manual_bank_accounts_json=$raw AND {} SET config.school_id=school.school_id,config.tenant_id=school.tenant_id,config.bank_account_id=$bank,config.bank_json=$json,config.version=config.version+1,config.active=true,config.updated_at=datetime(),config.updated_by=actor.id CREATE(:SchoolInvoiceAudit {{id:$audit,school_id:school.school_id,tenant_id:school.tenant_id,actor_id:actor.id,action:'payee_configured',version:config.version,created_at:datetime()}}) RETURN config.version AS version",auth::STAFF,auth::scope("school.school_id","school.tenant_id"),auth::fresh("config,school,ps"),auth::scope("school.school_id","school.tenant_id"));
    let version = super::repository::write(
        g,
        query(&body)
            .param("subject", a.subject.clone())
            .param("staff", a.id.clone())
            .param("expires", a.expires)
            .param("roles", vec!["owner"])
            .param("school", input.school_id.clone())
            .param("tenant", input.tenant_id.clone())
            .param("raw", raw)
            .param("key", format!("{}|{}", input.tenant_id, input.school_id))
            .param("bank", input.bank_account_id.clone())
            .param("json", bank_json)
            .param("version", input.version)
            .param("audit", uuid::Uuid::new_v4().to_string()),
        a.expires,
        |row| row.get("version").ok_or(Error::Unavailable),
    )
    .await?;
    Ok(Configured {
        tenant_id: input.tenant_id.clone(),
        school_id: input.school_id.clone(),
        version,
        bank,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchoolConfiguration {
    pub school_id: String,
    pub tenant_id: String,
    pub school_name: String,
    pub version: i64,
    pub bank: Option<ManualBankAccount>,
    pub banks: Vec<ManualBankAccount>,
}
/// Current scoped configuration for operational selection; no inferred payee.
pub async fn list(g: &Graph, a: &auth::Staff) -> Result<Vec<SchoolConfiguration>> {
    let body=format!("{} WITH actor MATCH(school:School) WHERE {} AND NOT EXISTS {{MATCH(other:School {{school_id:school.school_id,tenant_id:school.tenant_id}}) WHERE other<>school}} MATCH(ps:PaymentSettings {{tenant_id:school.tenant_id}}) WHERE NOT EXISTS {{MATCH(other:PaymentSettings {{tenant_id:ps.tenant_id}}) WHERE other<>ps}} OPTIONAL MATCH(config:SchoolBillingPayee {{tenant_id:school.tenant_id,school_id:school.school_id}}) RETURN school.school_id AS school,school.tenant_id AS tenant,coalesce(school.name,school.school_name,school.school_code) AS name,ps.manual_transfer_enabled AS enabled,ps.manual_bank_accounts_json AS directory,config.version AS version,config.active AS active,config.bank_json AS bank,config.bank_account_id AS bank_id,('owner' IN actor.roles) AS owner LIMIT 201",auth::STAFF,auth::scope("school.school_id","school.tenant_id"));
    // Also validates authority when there are no configured school rows.
    super::repository::staff_authority(g, a).await?;
    let mut rows = g
        .execute(
            query(&body)
                .param("subject", a.subject.clone())
                .param("staff", a.id.clone())
                .param("roles", auth::roles(true))
                .param("expires", a.expires),
        )
        .await
        .map_err(|_| Error::Unavailable)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| Error::Unavailable)? {
        if result.len() == 200 {
            return Err(Error::Unavailable);
        }
        let directory: Vec<ManualBankAccount> =
            serde_json::from_str(&row.get::<String>("directory").ok_or(Error::Unavailable)?)
                .map_err(|_| Error::Unavailable)?;
        let version = row.get::<i64>("version").unwrap_or(0);
        let raw = row.get::<String>("bank");
        let selected: Option<ManualBankAccount> = raw
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(|_| Error::Unavailable)?;
        let enabled = row.get::<bool>("enabled") == Some(true);
        let bank = selected.filter(|bank| {
            enabled
                && row.get::<bool>("active") == Some(true)
                && row.get::<String>("bank_id").as_deref() == Some(bank.id.as_str())
                && bank.enabled
                && directory.iter().filter(|b| b.id == bank.id).count() == 1
                && directory.iter().any(|b| b == bank)
        });
        let banks = if row.get::<bool>("owner") == Some(true) && enabled {
            directory.into_iter().filter(|b| b.enabled).collect()
        } else {
            bank.iter().cloned().collect()
        };
        result.push(SchoolConfiguration {
            school_id: row.get("school").ok_or(Error::Unavailable)?,
            tenant_id: row.get("tenant").ok_or(Error::Unavailable)?,
            school_name: row.get("name").ok_or(Error::Unavailable)?,
            version,
            bank,
            banks,
        });
    }
    if !auth::live(a.expires) {
        return Err(Error::Expired);
    }
    let mut scopes = std::collections::HashSet::new();
    if result
        .iter()
        .any(|c| !scopes.insert((c.tenant_id.clone(), c.school_id.clone())))
    {
        return Err(Error::Unavailable);
    }
    Ok(result)
}
