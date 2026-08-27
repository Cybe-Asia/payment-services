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
    pub doku_secret_key: String,
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
    pub frontend_url: String,
}

pub fn load() -> Config {
    let server_port = env::var("SERVER_PORT")
        .unwrap_or_else(|_| "8085".to_string())
        .parse()
        .unwrap();
    let neo4j_uri = env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://neo4j:7687".to_string());
    let neo4j_user = env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
    let neo4j_password = env::var("NEO4J_PASSWORD").unwrap_or_else(|_| "password".to_string());
    let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| "mysecretkey".to_string());
    let tenant_id = env::var("TENANT_ID").unwrap_or_else(|_| "TENANT-001".to_string());
    let xendit_api_url =
        env::var("XENDIT_API_URL").unwrap_or_else(|_| "https://api.xendit.co".to_string());
    let xendit_api_key = env::var("XENDIT_API_KEY").unwrap_or_default();
    let xendit_webhook_token = env::var("XENDIT_WEBHOOK_TOKEN").unwrap_or_default();
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
    let doku_secret_key = env::var("DOKU_SECRET_KEY").unwrap_or_default();
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
    let minio_access_key = env::var("MINIO_ACCESS_KEY").unwrap_or_default();
    let minio_secret_key = env::var("MINIO_SECRET_KEY").unwrap_or_default();
    let document_encryption_primary_key_id =
        env::var("DOCUMENT_ENCRYPTION_PRIMARY_KEY_ID").unwrap_or_default();
    let document_encryption_keyring = env::var("DOCUMENT_ENCRYPTION_KEYRING").unwrap_or_default();
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
        frontend_url,
    }
}
