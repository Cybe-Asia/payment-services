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
    pub default_fee_currency: String,
    pub default_fee_due_hours: i64,
}

pub fn load() -> Config {
    let server_port = env::var("SERVER_PORT").unwrap_or_else(|_| "8085".to_string()).parse().unwrap();
    let neo4j_uri = env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://neo4j:7687".to_string());
    let neo4j_user = env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
    let neo4j_password = env::var("NEO4J_PASSWORD").unwrap_or_else(|_| "password".to_string());
    let jwt_secret = env::var("JWT_SECRET").unwrap_or_else(|_| "mysecretkey".to_string());
    let tenant_id = env::var("TENANT_ID").unwrap_or_else(|_| "TENANT-001".to_string());
    let xendit_api_url = env::var("XENDIT_API_URL").unwrap_or_else(|_| "https://api.xendit.co".to_string());
    let xendit_api_key = env::var("XENDIT_API_KEY").unwrap_or_default();
    let xendit_webhook_token = env::var("XENDIT_WEBHOOK_TOKEN").unwrap_or_default();
    let xendit_success_redirect_url = env::var("XENDIT_SUCCESS_REDIRECT_URL")
        .unwrap_or_else(|_| "http://localhost:3000/auth/setup-account/payment/return?status=paid".to_string());
    let xendit_failure_redirect_url = env::var("XENDIT_FAILURE_REDIRECT_URL")
        .unwrap_or_else(|_| "http://localhost:3000/auth/setup-account/payment/return?status=failed".to_string());
    let default_fee_currency = env::var("DEFAULT_FEE_CURRENCY").unwrap_or_else(|_| "IDR".to_string());
    let default_fee_due_hours = env::var("DEFAULT_FEE_DUE_HOURS")
        .unwrap_or_else(|_| "168".to_string()) // 7 days
        .parse()
        .unwrap_or(168);

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
        default_fee_currency,
        default_fee_due_hours,
    }
}
