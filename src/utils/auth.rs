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

/// Staff gate for the marketing-assisted payment endpoints.
///
/// Accepts the ADMIN_EMAILS allowlist OR any account with an active staff
/// role on the shared graph's User node (same lookup admission-services
/// uses — suspended StaffProfiles resolve to no roles, so deactivation
/// bites here too). Marketing only *submits* evidence through these
/// endpoints; approving money stays behind `require_admin` on the
/// finance review routes.
pub async fn require_staff(
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

    if is_admin_email(&email) {
        return Ok(AdminAuth { email });
    }

    let roles = staff_roles_for_email(graph, &email).await?;
    if roles.iter().any(|r| !r.trim().is_empty()) {
        return Ok(AdminAuth { email });
    }
    Err((StatusCode::FORBIDDEN, "Staff access required".to_string()))
}

/// Roles allowed to APPROVE money: finance plus the full-admin-like roles.
/// Deliberately excludes marketing (any level) and admissions staff — they
/// can submit evidence via `require_staff`, never confirm it.
const FINANCE_APPROVE_ROLES: &[&str] = &["finance_admin", "finance_approver", "owner"];

/// Roles allowed to VIEW the payment review queue/detail/proofs. Superset of
/// the approve roles: admissions managers can look (they track applications
/// blocked on payment) but the approve endpoint stays finance-only.
const FINANCE_VIEW_ROLES: &[&str] = &[
    "finance_admin",
    "finance_approver",
    "owner",
    "admissions_admin",
    "admissions_manager",
];

/// Role-aware finance gate. `approve = true` for the money-mutating review
/// endpoint; `false` for read surfaces (queue, detail, proof download).
/// Accepts the ADMIN_EMAILS allowlist OR a matching active role from the
/// shared graph — so finance staff work from their role alone, without
/// needing an env-var entry per person.
pub async fn require_finance(
    graph: &Graph,
    headers: &HeaderMap,
    jwt_secret: &str,
    approve: bool,
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

    if !approve && is_admin_email(&email) {
        return Ok(AdminAuth { email });
    }

    let roles = staff_roles_for_email(graph, &email).await?;
    if finance_roles_allowed(&roles, approve) {
        return Ok(AdminAuth { email });
    }
    Err((
        StatusCode::FORBIDDEN,
        if approve {
            "Finance approval access required".to_string()
        } else {
            "Finance access required".to_string()
        },
    ))
}

fn finance_roles_allowed(roles: &[String], approve: bool) -> bool {
    let allowed: &[&str] = if approve {
        FINANCE_APPROVE_ROLES
    } else {
        FINANCE_VIEW_ROLES
    };
    roles.iter().any(|role| allowed.contains(&role.as_str()))
}

/// Active staff roles for an email from the shared graph (empty when the
/// StaffProfile is suspended — deactivation bites here too).
async fn staff_roles_for_email(
    graph: &Graph,
    email: &str,
) -> Result<Vec<String>, (StatusCode, String)> {
    let q = Query::new(
        "MATCH (u:User) WHERE toLower(u.email) = toLower($email) \
         OPTIONAL MATCH (u)-[:STAFF_PROFILE]->(s:StaffProfile) \
         RETURN CASE WHEN s IS NOT NULL AND s.status = 'suspended' THEN [] \
                     ELSE coalesce(u.marketingRoles, u.roles, []) END AS roles LIMIT 1"
            .to_string(),
    )
    .param("email", email.to_string());
    let mut res = graph.execute(q).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("staff role lookup: {e}"),
        )
    })?;
    Ok(res
        .next()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("staff role row: {e}"),
            )
        })?
        .and_then(|row| row.get::<Vec<String>>("roles"))
        .unwrap_or_default())
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

#[cfg(test)]
mod tests {
    use super::finance_roles_allowed;

    fn roles(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn admissions_can_view_but_cannot_confirm_money() {
        let admissions = roles(&["admissions_admin"]);
        assert!(finance_roles_allowed(&admissions, false));
        assert!(!finance_roles_allowed(&admissions, true));
        assert!(finance_roles_allowed(&roles(&["finance_approver"]), true));
        assert!(finance_roles_allowed(&roles(&["owner"]), true));
    }
}
