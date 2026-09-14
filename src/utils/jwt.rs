use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    pub sub: String,
    #[serde(default)]
    pub staff_member_id: Option<String>,
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

pub const STAFF_API_AUDIENCE: &str = "digital-school-admin-api";
pub const STAFF_API_PURPOSE: &str = "staff_downstream";

#[derive(Debug, Deserialize)]
pub struct StaffApiClaims {
    pub sub: String,
    pub staff_member_id: String,
    pub scope: String,
    pub iat: u64,
    pub exp: u64,
}

pub fn decode_staff_api_claims(
    token: &str,
    secret: &str,
    issuer: &str,
) -> Result<StaffApiClaims, String> {
    let header = jsonwebtoken::decode_header(token).map_err(|_| "Invalid staff token")?;
    if header.typ.as_deref() != Some("staff-api+jwt")
        || secret.len() < 32
        || !issuer.starts_with("https://")
    {
        return Err("Invalid staff credential configuration or type".into());
    }
    let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_audience(&[STAFF_API_AUDIENCE]);
    validation.set_issuer(&[issuer]);
    validation.leeway = 0;
    let claims = decode::<StaffApiClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|_| "Invalid staff token")?
    .claims;
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    if claims.scope != STAFF_API_PURPOSE
        || claims.sub.is_empty()
        || claims.staff_member_id.is_empty()
        || claims.iat > now
        || claims.exp <= claims.iat
        || claims.exp - claims.iat > 900
    {
        return Err("Invalid staff token claims".into());
    }
    Ok(claims)
}

#[cfg(test)]
mod staff_tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    const KEY: &str = "synthetic-staff-key-more-than-32-bytes";
    const ISSUER: &str = "https://auth.example.test/staff-api";
    fn token(change: Option<(&str, serde_json::Value)>) -> String {
        let now = chrono::Utc::now().timestamp();
        let mut claims = serde_json::json!({"iss":ISSUER,"aud":STAFF_API_AUDIENCE,"scope":STAFF_API_PURPOSE,"sub":"user-1","staff_member_id":"staff-1","iat":now,"exp":now+300});
        if let Some((key, value)) = change {
            claims[key] = value;
        }
        let mut header = Header::default();
        header.typ = Some("staff-api+jwt".into());
        encode(&header, &claims, &EncodingKey::from_secret(KEY.as_bytes())).unwrap()
    }
    #[test]
    fn validates_staff_family_and_rejects_parent_replay() {
        let valid = token(None);
        assert!(decode_staff_api_claims(&valid, KEY, ISSUER).is_ok());
        assert!(decode_session_claims(&valid, KEY).is_err());
        for (key, value) in [
            ("aud", serde_json::json!("parent-api")),
            ("iss", serde_json::json!("https://other.example")),
            ("scope", serde_json::json!("parent")),
            ("staff_member_id", serde_json::json!("")),
            ("exp", serde_json::json!(1)),
            (
                "iat",
                serde_json::json!(chrono::Utc::now().timestamp() + 30),
            ),
            (
                "exp",
                serde_json::json!(chrono::Utc::now().timestamp() + 1000),
            ),
        ] {
            assert!(decode_staff_api_claims(&token(Some((key, value))), KEY, ISSUER).is_err());
        }
    }
}
