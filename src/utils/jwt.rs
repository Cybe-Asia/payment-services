use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    pub sub: String,
    #[serde(rename = "exp")]
    pub _exp: usize,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

pub fn decode_session_claims(token: &str, secret: &str) -> Result<Claims, String> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_ref()),
        &Validation::default(),
    )
    .map_err(|e| e.to_string())?;

    if data.claims.scope.is_some() {
        return Err("token scope mismatch".to_string());
    }

    Ok(data.claims)
}
