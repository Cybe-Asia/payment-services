use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::Deserialize;

const PARENT_PAYMENT_SETUP_SCOPES: &[&str] = &["eoi_setup", "setup_context"];

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

fn decode_claims(token: &str, secret: &str) -> Result<Claims, String> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_ref()),
        &Validation::default(),
    )
    .map_err(|e| e.to_string())?;

    Ok(data.claims)
}

pub fn decode_session_claims(token: &str, secret: &str) -> Result<Claims, String> {
    let claims = decode_claims(token, secret)?;

    if claims.scope.is_some() {
        return Err("token scope mismatch".to_string());
    }

    Ok(claims)
}

/// Decode a parent token for the setup payment flow.
///
/// Full parent sessions remain valid. The two onboarding scopes are accepted
/// only when their subject is the lead that owns the payment flow; all other
/// scoped tokens keep the default rejection behavior.
pub fn decode_parent_payment_claims(token: &str, secret: &str) -> Result<Claims, String> {
    let claims = decode_claims(token, secret)?;

    match claims.scope.as_deref() {
        None => Ok(claims),
        Some(scope)
            if claims.sub.starts_with("LEAD-") && PARENT_PAYMENT_SETUP_SCOPES.contains(&scope) =>
        {
            Ok(claims)
        }
        Some(_) => Err("token scope mismatch".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::decode_parent_payment_claims;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SECRET: &str = "payment-test-secret";

    #[derive(Serialize)]
    struct TestClaims<'a> {
        sub: &'a str,
        exp: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<&'a str>,
    }

    fn token(sub: &str, scope: Option<&str>) -> String {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs() as usize
            + 3600;
        encode(
            &Header::default(),
            &TestClaims { sub, exp, scope },
            &EncodingKey::from_secret(SECRET.as_bytes()),
        )
        .expect("test token")
    }

    #[test]
    fn parent_payment_accepts_onboarding_scopes_for_their_lead_subject() {
        for scope in ["eoi_setup", "setup_context"] {
            let claims = decode_parent_payment_claims(&token("LEAD-123", Some(scope)), SECRET)
                .expect("onboarding token accepted for parent payment");
            assert_eq!(claims.sub, "LEAD-123");
            assert_eq!(claims.scope.as_deref(), Some(scope));
        }
    }

    #[test]
    fn parent_payment_rejects_unrelated_scopes_and_non_lead_onboarding_subjects() {
        assert!(
            decode_parent_payment_claims(&token("LEAD-123", Some("staff_portal")), SECRET,)
                .is_err()
        );
        assert!(
            decode_parent_payment_claims(&token("USER-123", Some("setup_context")), SECRET,)
                .is_err()
        );
    }
}
