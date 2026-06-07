use axum::http::{HeaderMap, StatusCode};
use neo4rs::{Graph, Query};

use crate::utils::jwt::{decode_session_claims, Claims};

#[derive(Debug, Clone)]
pub struct ParentAuth {
    pub subject: String,
    pub email: Option<String>,
    pub lead_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AdminAuth {
    pub email: String,
}

pub async fn require_parent_auth(
    graph: &Graph,
    headers: &HeaderMap,
    jwt_secret: &str,
) -> Result<ParentAuth, (StatusCode, String)> {
    let claims = claims_from_bearer(headers, jwt_secret)?;
    let (email, lead_ids) = resolve_owned_leads(graph, &claims)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if lead_ids.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            "No lead found for this user".to_string(),
        ));
    }

    Ok(ParentAuth {
        subject: claims.sub,
        email,
        lead_ids,
    })
}

pub async fn require_admin(
    graph: &Graph,
    headers: &HeaderMap,
    jwt_secret: &str,
) -> Result<AdminAuth, (StatusCode, String)> {
    let claims = claims_from_bearer(headers, jwt_secret)?;
    let email = match claims.email.clone() {
        Some(email) if !email.trim().is_empty() => email,
        _ => resolve_email_for_subject(graph, &claims.sub)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
            .ok_or_else(|| {
                (
                    StatusCode::UNAUTHORIZED,
                    "Token missing email claim".to_string(),
                )
            })?,
    };

    if !is_admin_email(&email) {
        return Err((StatusCode::FORBIDDEN, "Admin access required".to_string()));
    }

    Ok(AdminAuth { email })
}

pub fn owns_lead(auth: &ParentAuth, lead_id: Option<&str>) -> bool {
    let Some(lead_id) = lead_id else {
        return false;
    };
    auth.lead_ids.iter().any(|id| id == lead_id)
}

fn claims_from_bearer(
    headers: &HeaderMap,
    jwt_secret: &str,
) -> Result<Claims, (StatusCode, String)> {
    let Some(token) = bearer_from(headers) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Missing or invalid Authorization header".to_string(),
        ));
    };
    decode_session_claims(&token, jwt_secret).map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            "Invalid or expired token".to_string(),
        )
    })
}

fn bearer_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn is_admin_email(email: &str) -> bool {
    let allowed = std::env::var("ADMIN_EMAILS").unwrap_or_default();
    let requester = email.to_lowercase();
    allowed
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .any(|e| !e.is_empty() && e == requester)
}

async fn resolve_owned_leads(
    graph: &Graph,
    claims: &Claims,
) -> Result<(Option<String>, Vec<String>), String> {
    if claims.sub.starts_with("LEAD-") {
        let q = Query::new(
            "MATCH (seed:Lead {lead_id: $seed}) \
             OPTIONAL MATCH (u:User) WHERE toLower(u.email) = toLower(seed.email) \
             OPTIONAL MATCH (u)-[:HAS_APPLICATION]->(l:Lead) \
             WITH seed, collect(DISTINCT l.lead_id) AS linked \
             RETURN seed.email AS email, \
                    CASE WHEN size(linked) = 0 THEN [seed.lead_id] ELSE linked END AS ids"
                .to_string(),
        )
        .param("seed", claims.sub.clone());

        let mut res = graph
            .execute(q)
            .await
            .map_err(|e| format!("lead auth lookup: {e}"))?;
        if let Some(row) = res
            .next()
            .await
            .map_err(|e| format!("lead auth row: {e}"))?
        {
            let email = row.get::<String>("email");
            let ids = row.get::<Vec<String>>("ids").unwrap_or_default();
            return Ok((email, ids));
        }
        return Ok((claims.email.clone(), Vec::new()));
    }

    let q = Query::new(
        "MATCH (u:User {id: $id}) \
         OPTIONAL MATCH (u)-[:HAS_APPLICATION]->(l:Lead) \
         RETURN u.email AS email, collect(DISTINCT l.lead_id) AS ids"
            .to_string(),
    )
    .param("id", claims.sub.clone());

    let mut res = graph
        .execute(q)
        .await
        .map_err(|e| format!("user auth lookup: {e}"))?;
    if let Some(row) = res
        .next()
        .await
        .map_err(|e| format!("user auth row: {e}"))?
    {
        let email = row.get::<String>("email").or_else(|| claims.email.clone());
        let ids = row.get::<Vec<String>>("ids").unwrap_or_default();
        Ok((email, ids))
    } else {
        Ok((claims.email.clone(), Vec::new()))
    }
}

async fn resolve_email_for_subject(graph: &Graph, subject: &str) -> Result<Option<String>, String> {
    if subject.starts_with("LEAD-") {
        let q =
            Query::new("MATCH (l:Lead {lead_id: $id}) RETURN l.email AS email LIMIT 1".to_string())
                .param("id", subject.to_string());
        let mut res = graph
            .execute(q)
            .await
            .map_err(|e| format!("lead email lookup: {e}"))?;
        return Ok(res
            .next()
            .await
            .map_err(|e| format!("lead email row: {e}"))?
            .and_then(|row| row.get::<String>("email")));
    }

    let q = Query::new("MATCH (u:User {id: $id}) RETURN u.email AS email LIMIT 1".to_string())
        .param("id", subject.to_string());
    let mut res = graph
        .execute(q)
        .await
        .map_err(|e| format!("user email lookup: {e}"))?;
    Ok(res
        .next()
        .await
        .map_err(|e| format!("user email row: {e}"))?
        .and_then(|row| row.get::<String>("email")))
}
