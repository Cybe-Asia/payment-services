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

#[derive(Debug, Clone, Default)]
struct StaffAuthorization {
    roles: Vec<String>,
    tenant_ids: Vec<String>,
    school_ids: Vec<String>,
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
    tenant_id: &str,
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

    let authorization = staff_authorization_for_subject(graph, &claims.sub).await?;
    let fallback = nonprod_admin_email_fallback_enabled() && is_admin_email(&email);
    if !(payment_admin_roles_allowed(&authorization.roles)
        && tenant_scope_allowed(&authorization, tenant_id))
        && !fallback
    {
        return Err((StatusCode::FORBIDDEN, "Admin access required".to_string()));
    }

    Ok(AdminAuth { email })
}

/// Staff gate for the marketing-assisted payment endpoints.
///
/// Accepts an account with at least one active canonical staff role. Marketing
/// only *submits* evidence through these endpoints; approving money stays
/// behind the finance review routes.
pub async fn require_staff(
    graph: &Graph,
    headers: &HeaderMap,
    jwt_secret: &str,
    tenant_id: &str,
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

    let authorization = staff_authorization_for_subject(graph, &claims.sub).await?;
    if payment_evidence_roles_allowed(&authorization.roles)
        && tenant_scope_allowed(&authorization, tenant_id)
        || nonprod_admin_email_fallback_enabled() && is_admin_email(&email)
    {
        return Ok(AdminAuth { email });
    }
    Err((StatusCode::FORBIDDEN, "Staff access required".to_string()))
}

/// Roles allowed to APPROVE money: finance plus the full-admin-like roles.
/// Deliberately excludes marketing (any level) and admissions staff — they
/// can submit evidence via `require_staff`, never confirm it.
const FINANCE_APPROVE_ROLES: &[&str] = &["finance_admin", "finance_approver", "owner"];
const PAYMENT_ADMIN_ROLES: &[&str] = &["finance_admin", "owner", "school_admin"];
const PAYMENT_EVIDENCE_ROLES: &[&str] = &[
    "admissions_staff",
    "admissions_manager",
    "admissions_admin",
    "marketing_staff",
    "marketing_manager",
    "finance_approver",
    "finance_admin",
    "owner",
    "school_admin",
];

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
/// Accepts a matching active role from the shared graph.
pub async fn require_finance(
    graph: &Graph,
    headers: &HeaderMap,
    jwt_secret: &str,
    approve: bool,
    tenant_id: &str,
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

    let authorization = staff_authorization_for_subject(graph, &claims.sub).await?;
    if finance_roles_allowed(&authorization.roles, approve)
        && tenant_scope_allowed(&authorization, tenant_id)
        || nonprod_admin_email_fallback_enabled() && is_admin_email(&email)
    {
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

fn payment_admin_roles_allowed(roles: &[String]) -> bool {
    roles
        .iter()
        .any(|role| PAYMENT_ADMIN_ROLES.contains(&role.as_str()))
}

fn payment_evidence_roles_allowed(roles: &[String]) -> bool {
    roles
        .iter()
        .any(|role| PAYMENT_EVIDENCE_ROLES.contains(&role.as_str()))
}

/// Active staff roles for an email from the shared graph. Auth's canonical
/// StaffMember projection wins when present; invited, suspended, and
/// deactivated users all resolve to no roles.
async fn staff_authorization_for_subject(
    graph: &Graph,
    subject: &str,
) -> Result<StaffAuthorization, (StatusCode, String)> {
    let q = Query::new(
        "MATCH (u:User {id:$subject}) \
         OPTIONAL MATCH (u)-[:STAFF_MEMBER]->(member:StaffMember) \
         OPTIONAL MATCH (u)-[:STAFF_PROFILE]->(profile:StaffProfile) \
         RETURN CASE \
           WHEN member IS NOT NULL THEN CASE WHEN member.membershipStatus = 'ACTIVE' THEN coalesce(member.roles,[]) ELSE [] END \
           WHEN profile IS NOT NULL THEN CASE WHEN toLower(profile.status) = 'active' THEN coalesce(profile.roles,u.roles,[]) ELSE [] END \
           WHEN toLower(coalesce(u.staffStatus,'')) = 'active' THEN coalesce(u.roles,[]) \
           ELSE [] END AS roles, \
           CASE WHEN member IS NOT NULL THEN coalesce(member.tenantIds,[]) ELSE coalesce(profile.tenant_ids,u.staffTenantIds,[]) END AS tenantIds, \
           CASE WHEN member IS NOT NULL THEN coalesce(member.schoolIds,[]) ELSE coalesce(profile.school_ids,u.staffSchoolIds,[]) END AS schoolIds LIMIT 1"
            .to_string(),
    )
    .param("subject", subject.to_string());
    let mut res = graph.execute(q).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("staff role lookup: {e}"),
        )
    })?;
    let row = res
        .next()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("staff role row: {e}"),
            )
        })?;
    Ok(row
        .map(|row| StaffAuthorization {
            roles: row.get::<Vec<String>>("roles").unwrap_or_default(),
            tenant_ids: row.get::<Vec<String>>("tenantIds").unwrap_or_default(),
            school_ids: row.get::<Vec<String>>("schoolIds").unwrap_or_default(),
        })
        .unwrap_or_default())
}

fn tenant_scope_allowed(authorization: &StaffAuthorization, required_tenant: &str) -> bool {
    authorization.roles.iter().any(|role| role == "owner")
        || (!authorization.school_ids.is_empty()
            && authorization
                .tenant_ids
                .iter()
                .any(|tenant| tenant == required_tenant))
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

fn nonprod_admin_email_fallback_enabled() -> bool {
    let enabled = std::env::var("PAYMENT_NONPROD_ADMIN_EMAIL_FALLBACK_ENABLED")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(false);
    enabled
        && matches!(
            std::env::var("APP_ENV")
                .unwrap_or_else(|_| "local".to_string())
                .as_str(),
            "local" | "dev" | "test"
        )
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
    use super::{
        finance_roles_allowed, payment_admin_roles_allowed, payment_evidence_roles_allowed,
        tenant_scope_allowed, StaffAuthorization,
    };

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

    #[test]
    fn payment_administration_requires_an_explicit_canonical_role() {
        assert!(payment_admin_roles_allowed(&roles(&["owner"])));
        assert!(payment_admin_roles_allowed(&roles(&["finance_admin"])));
        assert!(payment_admin_roles_allowed(&roles(&["school_admin"])));
        assert!(!payment_admin_roles_allowed(&roles(&["finance_approver"])));
        assert!(!payment_admin_roles_allowed(&roles(&[])));
    }

    #[test]
    fn payment_evidence_submission_excludes_unrelated_staff_roles() {
        assert!(payment_evidence_roles_allowed(&roles(&["marketing_staff"])));
        assert!(payment_evidence_roles_allowed(&roles(&["admissions_staff"])));
        assert!(payment_evidence_roles_allowed(&roles(&["finance_approver"])));
        assert!(!payment_evidence_roles_allowed(&roles(&["teacher"])));
        assert!(!payment_evidence_roles_allowed(&roles(&["attendance_employee"])));
    }

    #[test]
    fn tenant_scope_is_explicit_except_for_owner() {
        let scoped = StaffAuthorization {
            roles: roles(&["finance_admin"]),
            tenant_ids: roles(&["tenant-a"]),
            school_ids: roles(&["school-a"]),
        };
        assert!(tenant_scope_allowed(&scoped, "tenant-a"));
        assert!(!tenant_scope_allowed(&scoped, "tenant-b"));
        let owner = StaffAuthorization {
            roles: roles(&["owner"]),
            ..Default::default()
        };
        assert!(tenant_scope_allowed(&owner, "tenant-b"));
    }
}
