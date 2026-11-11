use crate::utils::error::ServiceError;
use std::env;

pub struct Config {
    pub server_port: u16,
    pub neo4j_uri: String,
    pub neo4j_user: String,
    pub neo4j_password: String,
    pub jwt_secret: String,
    pub tenant_id: String,
    pub xendit_api_url: String,
    pub xendit_api_key: String,
    pub xendit_webhook_token: String,
    pub xendit_success_redirect_url: String,
    pub xendit_failure_redirect_url: String,
    pub legacy_parent_payments_enabled: bool,
    pub doku_api_url: String,
    pub doku_client_id: String,
    /// `None` keeps DOKU checkout and its webhook disabled.
    pub doku_secret_key: Option<String>,
    pub doku_return_url: String,
    pub doku_notification_url: String,
    pub doku_payment_method_types: Vec<String>,
    pub default_fee_currency: String,
    pub default_fee_due_hours: i64,
    pub minio_endpoint: String,
    pub minio_bucket: String,
    pub minio_region: String,
    pub minio_access_key: String,
    pub minio_secret_key: String,
    pub document_encryption_primary_key_id: String,
    pub document_encryption_keyring: String,
    pub document_legacy_plaintext_reads_allowed: bool,
    pub document_encryption_migrate_on_startup: bool,
    pub manual_transfer_bank_name: String,
    pub manual_transfer_account_name: String,
    pub manual_transfer_account_number: String,
    pub manual_transfer_instructions: String,
    pub notification_service_url: String,
    /// Sent as `X-Internal-Token` on calls to notification-service when set.
    pub notification_internal_token: Option<String>,
    /// Shared secret shop-services sends as `X-Internal-Token`; unset or under 32 characters
    /// keeps the Shop ledger routes closed.
    pub shop_internal_token: Option<String>,
    pub frontend_url: String,
    pub invoice_email_enabled: bool,
    pub admissions_automation_enabled: bool,
    pub school_billing_worker_enabled: bool,
    pub school_billing_return_url: Option<String>,
    pub notification_activated_at: Option<String>,
    pub notification_recipient_allowlist: Option<String>,
}

pub fn load() -> Config {
    let server_port = env::var("SERVER_PORT")
        .unwrap_or_else(|_| "8085".to_string())
        .parse()
        .expect("SERVER_PORT must be a port number");
    let neo4j_uri = env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://neo4j:7687".to_string());
    let neo4j_user = env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
    let neo4j_password = env::var("NEO4J_PASSWORD").expect("NEO4J_PASSWORD must be set");
    let jwt_secret = env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    if jwt_secret.len() < 32 {
        tracing::warn!("JWT_SECRET is shorter than 32 bytes; rotate it to a longer secret");
    }
    let tenant_id = tenant_id();
    let xendit_api_url =
        env::var("XENDIT_API_URL").unwrap_or_else(|_| "https://api.xendit.co".to_string());
    let xendit_api_key = env::var("XENDIT_API_KEY").expect("XENDIT_API_KEY must be set");
    let xendit_webhook_token =
        env::var("XENDIT_WEBHOOK_TOKEN").expect("XENDIT_WEBHOOK_TOKEN must be set");
    let xendit_success_redirect_url =
        env::var("XENDIT_SUCCESS_REDIRECT_URL").unwrap_or_else(|_| {
            "http://localhost:3000/auth/setup-account/payment/return?status=paid".to_string()
        });
    let xendit_failure_redirect_url =
        env::var("XENDIT_FAILURE_REDIRECT_URL").unwrap_or_else(|_| {
            "http://localhost:3000/auth/setup-account/payment/return?status=failed".to_string()
        });
    let legacy_parent_payments_enabled = env::var("LEGACY_PARENT_PAYMENTS_ENABLED")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let doku_api_url =
        env::var("DOKU_API_URL").unwrap_or_else(|_| "https://api-sandbox.doku.com".to_string());
    let doku_client_id = env::var("DOKU_CLIENT_ID").unwrap_or_default();
    let doku_secret_key = env::var("DOKU_SECRET_KEY")
        .ok()
        .filter(|value| !value.is_empty());
    if doku_secret_key.is_none() {
        tracing::warn!("DOKU_SECRET_KEY is not set; DOKU checkout and its webhook are disabled");
    }
    let doku_return_url = env::var("DOKU_RETURN_URL").unwrap_or_default();
    let doku_notification_url = env::var("DOKU_NOTIFICATION_URL").unwrap_or_default();
    let doku_payment_method_types = env::var("DOKU_PAYMENT_METHOD_TYPES")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    let default_fee_currency =
        env::var("DEFAULT_FEE_CURRENCY").unwrap_or_else(|_| "IDR".to_string());
    let default_fee_due_hours = env::var("DEFAULT_FEE_DUE_HOURS")
        .unwrap_or_else(|_| "168".to_string()) // 7 days
        .parse()
        .unwrap_or(168);
    let minio_endpoint = env::var("MINIO_ENDPOINT").unwrap_or_default();
    let minio_bucket = env::var("MINIO_BUCKET").unwrap_or_default();
    let minio_region = env::var("MINIO_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let minio_access_key = env::var("MINIO_ACCESS_KEY").expect("MINIO_ACCESS_KEY must be set");
    let minio_secret_key = env::var("MINIO_SECRET_KEY").expect("MINIO_SECRET_KEY must be set");
    let document_encryption_primary_key_id = env::var("DOCUMENT_ENCRYPTION_PRIMARY_KEY_ID")
        .expect("DOCUMENT_ENCRYPTION_PRIMARY_KEY_ID must be set");
    let document_encryption_keyring =
        env::var("DOCUMENT_ENCRYPTION_KEYRING").expect("DOCUMENT_ENCRYPTION_KEYRING must be set");
    let document_legacy_plaintext_reads_allowed =
        env::var("DOCUMENT_LEGACY_PLAINTEXT_READS_ALLOWED")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    let document_encryption_migrate_on_startup = env::var("DOCUMENT_ENCRYPTION_MIGRATE_ON_STARTUP")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let manual_transfer_bank_name = env::var("MANUAL_TRANSFER_BANK_NAME").unwrap_or_default();
    let manual_transfer_account_name = env::var("MANUAL_TRANSFER_ACCOUNT_NAME").unwrap_or_default();
    let manual_transfer_account_number =
        env::var("MANUAL_TRANSFER_ACCOUNT_NUMBER").unwrap_or_default();
    let manual_transfer_instructions = env::var("MANUAL_TRANSFER_INSTRUCTIONS").unwrap_or_default();
    let notification_service_url = env::var("NOTIFICATION_SERVICE_URL")
        .unwrap_or_else(|_| "http://notification-service".to_string());
    let notification_internal_token = env::var("NOTIFICATION_INTERNAL_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let shop_internal_token = env::var("SHOP_INTERNAL_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| value.len() >= 32);
    let frontend_url =
        env::var("FRONTEND_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    Config {
        server_port,
        neo4j_uri,
        neo4j_user,
        neo4j_password,
        jwt_secret,
        tenant_id,
        xendit_api_url,
        xendit_api_key,
        xendit_webhook_token,
        xendit_success_redirect_url,
        xendit_failure_redirect_url,
        legacy_parent_payments_enabled,
        doku_api_url,
        doku_client_id,
        doku_secret_key,
        doku_return_url,
        doku_notification_url,
        doku_payment_method_types,
        default_fee_currency,
        default_fee_due_hours,
        minio_endpoint,
        minio_bucket,
        minio_region,
        minio_access_key,
        minio_secret_key,
        document_encryption_primary_key_id,
        document_encryption_keyring,
        document_legacy_plaintext_reads_allowed,
        document_encryption_migrate_on_startup,
        manual_transfer_bank_name,
        manual_transfer_account_name,
        manual_transfer_account_number,
        manual_transfer_instructions,
        notification_service_url,
        notification_internal_token,
        shop_internal_token,
        frontend_url,
        invoice_email_enabled: env::var("INVOICE_EMAIL_ENABLED").as_deref() == Ok("true"),
        admissions_automation_enabled: env::var("ADMISSIONS_AUTOMATION_ENABLED").as_deref()
            == Ok("true"),
        school_billing_worker_enabled: env::var("SCHOOL_BILLING_WORKER_ENABLED").as_deref()
            == Ok("true"),
        school_billing_return_url: env::var("SCHOOL_BILLING_RETURN_URL").ok(),
        notification_activated_at: env::var("NOTIFICATION_ACTIVATED_AT").ok(),
        notification_recipient_allowlist: env::var("NOTIFICATION_RECIPIENT_ALLOWLIST").ok(),
    }
}

/// The tenant this Payment deployment serves.
pub fn tenant_id() -> String {
    env::var("TENANT_ID").unwrap_or_else(|_| "TENANT-001".to_string())
}

// shortcut: the auth helpers below read per call because they have no AppState and
// tests switch these values at runtime; pass them through state once auth takes it.

/// Comma-separated legacy admin email allowlist (`ADMIN_EMAILS`), empty when unset.
pub fn admin_emails() -> String {
    env::var("ADMIN_EMAILS").unwrap_or_default()
}

/// SIS capability origin (`SIS_ACCESS_ORIGIN`), unvalidated.
pub fn sis_access_origin() -> Option<String> {
    env::var("SIS_ACCESS_ORIGIN").ok()
}

/// A graph setting for the read-only connection check, which runs before `load()`.
pub fn graph_setting(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// Staff credentials use a key distinct from legacy parent sessions. Missing
/// configuration disables the new family; never fall back to the parent key.
pub fn staff_downstream_settings(parent_secret: &str) -> Result<(String, String), ServiceError> {
    let key = env::var("STAFF_DOWNSTREAM_JWT_SECRET")
        .map_err(|_| "Staff API authorization unavailable")?;
    let issuer = env::var("STAFF_DOWNSTREAM_JWT_ISSUER")
        .map_err(|_| "Staff API authorization unavailable")?;
    if key.len() < 32 || key == parent_secret || !issuer.starts_with("https://") {
        return Err("Staff API authorization unavailable".into());
    }
    Ok((key, issuer))
}
