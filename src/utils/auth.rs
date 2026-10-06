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
    /// Staff roles behind the grant (`["owner"]` for the legacy allowlist).
    pub roles: Vec<String>,
}

impl AdminAuth {
    fn owner(email: String) -> Self {
        Self {
            email,
            roles: vec!["owner".to_string()],
        }
    }

    /// Marketing staff without a manager/admissions/finance/owner role: their
    /// lead reach is limited to own/claimed/team leads and the unassigned pool.
    pub fn lead_scoped(&self) -> bool {
        let wide = [
            "owner",
            "marketing_manager",
            "admissions_staff",
            "admissions_manager",
            "admissions_admin",
            "finance_admin",
            "finance_approver",
        ];
        !self.roles.iter().any(|role| wide.contains(&role.as_str()))
    }
}

/// Staff roles that may use the assisted payment endpoints.
const STAFF_ROLES: &[&str] = &[
    "owner",
    "marketing_staff",
    "marketing_manager",
    "admissions_staff",
    "admissions_manager",
    "admissions_admin",
    "finance_admin",
    "finance_approver",
];

fn staff_roles(roles: Vec<String>) -> Vec<String> {
    roles
        .into_iter()
        .filter(|role| STAFF_ROLES.contains(&role.as_str()))
        .collect()
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
    let claims = staff_claims_from_bearer(headers, jwt_secret)?;
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

    use crate::repositories::canonical_staff_repository::{resolve_admin, CanonicalAdmin};
    match resolve_admin(graph, &claims.sub)
        .await
        .map_err(|message| (StatusCode::SERVICE_UNAVAILABLE, message))?
    {
        CanonicalAdmin::Owner(id)
            if claims
                .staff_member_id
                .as_ref()
                .is_none_or(|expected| expected == &id) =>
        {
            return Ok(AdminAuth::owner(email))
        }
        CanonicalAdmin::Owner(_) => {
            return Err((StatusCode::FORBIDDEN, "Staff identity mismatch".into()))
        }
        CanonicalAdmin::Denied => {
            return Err((StatusCode::FORBIDDEN, "Admin access required".to_string()))
        }
        CanonicalAdmin::NotLinked if claims.staff_member_id.is_some() => {
            return Err((StatusCode::FORBIDDEN, "Staff identity unavailable".into()))
        }
        CanonicalAdmin::NotLinked => {}
    }

    if !is_admin_email(&email) {
        return Err((StatusCode::FORBIDDEN, "Admin access required".to_string()));
    }

    Ok(AdminAuth::owner(email))
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
    let claims = staff_claims_from_bearer(headers, jwt_secret)?;
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

    use crate::repositories::canonical_staff_repository::{resolve_staff, CanonicalStaff};
    match resolve_staff(graph, &claims.sub)
        .await
        .map_err(|message| (StatusCode::SERVICE_UNAVAILABLE, message))?
    {
        CanonicalStaff::Active { id, roles } => {
            if claims
                .staff_member_id
                .as_ref()
                .is_some_and(|expected| expected != &id)
            {
                return Err((StatusCode::FORBIDDEN, "Staff identity mismatch".into()));
            }
            let roles = staff_roles(roles);
            if !roles.is_empty() {
                return Ok(AdminAuth { email, roles });
            }
            return Err((StatusCode::FORBIDDEN, "Staff access required".into()));
        }
        CanonicalStaff::Denied => {
            return Err((StatusCode::FORBIDDEN, "Staff access required".into()))
        }
        CanonicalStaff::NotLinked if claims.staff_member_id.is_some() => {
            return Err((StatusCode::FORBIDDEN, "Staff identity unavailable".into()))
        }
        CanonicalStaff::NotLinked => {}
    }

    if is_admin_email(&email) {
        return Ok(AdminAuth::owner(email));
    }

    // Legacy accounts: only recognised staff roles count (a parent's
    // `User.roles = ['parent']` must never open staff endpoints).
    let roles = staff_roles(staff_roles_for_email(graph, &email).await?);
    if !roles.is_empty() {
        return Ok(AdminAuth { email, roles });
    }
    Err((StatusCode::FORBIDDEN, "Staff access required".to_string()))
}

/// Lead reach for assisted actions. Mirrors admission-services
/// `can_view_lead`: lead-scoped marketing staff reach leads they are assigned
/// to, leads attributed to their marketing owner or team, and the unassigned
/// pool. Accepts a Lead id or a Student id (walked back to its Lead).
pub async fn staff_can_reach_lead(
    graph: &Graph,
    staff: &AdminAuth,
    lead_or_student_id: &str,
) -> Result<bool, (StatusCode, String)> {
    if !staff.lead_scoped() {
        return Ok(true);
    }
    let q = Query::new(
        "MATCH (l:Lead) WHERE l.lead_id = $id \
            OR EXISTS { MATCH (l)-[:HAS_STUDENT]->(:Student {studentId:$id}) } \
         WITH l LIMIT 1 \
         OPTIONAL MATCH (me:MarketingOwner) WHERE toLower(me.email) = toLower($email) \
         OPTIONAL MATCH (mu:User) WHERE toLower(mu.email) = toLower($email) \
         WITH l, head(collect(me)) AS me, head(collect(mu)) AS mu \
         WITH l, coalesce(me.owner_id, '') AS ownerId, \
              coalesce(me.team_ids, []) + coalesce(mu.staffTeamIds, []) AS teams \
         OPTIONAL MATCH (lo:MarketingOwner {owner_id: l.reference_owner_id}) \
         OPTIONAL MATCH (au:User) \
           WHERE toLower(au.email) = toLower(coalesce(l.assigned_admin_email, '')) \
         RETURN max(CASE WHEN \
              toLower(coalesce(l.assigned_admin_email, '')) = toLower($email) \
              OR (ownerId <> '' AND coalesce(l.reference_owner_id, '') = ownerId) \
              OR (size(teams) > 0 AND any(t IN coalesce(lo.team_ids, []) WHERE t IN teams)) \
              OR (size(teams) > 0 AND any(t IN coalesce(au.staffTeamIds, []) WHERE t IN teams)) \
              OR (coalesce(l.reference_code, l.referral_code, '') = '' \
                  AND coalesce(l.reference_owner_id, '') = '') \
              THEN 1 ELSE 0 END) = 1 AS allowed"
            .to_string(),
    )
    .param("id", lead_or_student_id.to_string())
    .param("email", staff.email.clone());
    let unavailable = |e: neo4rs::Error| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("staff scope lookup: {e}"),
        )
    };
    let mut rows = graph.execute(q).await.map_err(unavailable)?;
    Ok(rows
        .next()
        .await
        .map_err(unavailable)?
        .and_then(|row| row.get::<bool>("allowed"))
        .unwrap_or(false))
}

/// Roles allowed to APPROVE money: finance, admissions managers and owner.
/// Marketing (any level) and admissions staff can submit evidence via
/// `require_staff`, never confirm it; the reviewer must also differ from
/// whoever uploaded the proof (enforced in the review service).
const FINANCE_APPROVE_ROLES: &[&str] = &[
    "finance_admin",
    "finance_approver",
    "admissions_admin",
    "admissions_manager",
    "owner",
];

/// Roles allowed to VIEW the payment review queue/detail/proofs. Superset of
/// the approve roles.
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
    let claims = staff_claims_from_bearer(headers, jwt_secret)?;
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

    use crate::repositories::canonical_staff_repository::{resolve_staff, CanonicalStaff};
    match resolve_staff(graph, &claims.sub)
        .await
        .map_err(|message| (StatusCode::SERVICE_UNAVAILABLE, message))?
    {
        CanonicalStaff::Active { id, roles } => {
            if claims
                .staff_member_id
                .as_ref()
                .is_some_and(|expected| expected != &id)
            {
                return Err((StatusCode::FORBIDDEN, "Staff identity mismatch".into()));
            }
            return if finance_roles_allowed(&roles, approve) {
                Ok(AdminAuth {
                    email,
                    roles: staff_roles(roles),
                })
            } else {
                Err((StatusCode::FORBIDDEN, "Finance access required".into()))
            };
        }
        CanonicalStaff::Denied => {
            return Err((StatusCode::FORBIDDEN, "Finance access required".into()))
        }
        CanonicalStaff::NotLinked if claims.staff_member_id.is_some() => {
            return Err((StatusCode::FORBIDDEN, "Staff identity unavailable".into()))
        }
        CanonicalStaff::NotLinked => {}
    }

    if !approve && is_admin_email(&email) {
        return Ok(AdminAuth::owner(email));
    }

    let roles = staff_roles_for_email(graph, &email).await?;
    if finance_roles_allowed(&roles, approve) {
        return Ok(AdminAuth {
            email,
            roles: staff_roles(roles),
        });
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

/// Resolve legacy Lead or Student request identifiers before any payment mutation.
pub async fn owns_admission(
    graph: &Graph,
    parent: &ParentAuth,
    id: &str,
    tenant_id: &str,
) -> Result<bool, String> {
    let mut rows = graph
        .execute(
            Query::new(
                "MATCH (l:Lead {tenant_id:$tenant_id}) WHERE l.lead_id IN $owned \
         OPTIONAL MATCH (l)-[:HAS_STUDENT]->(s:Student) \
         WITH l, collect(s.studentId) AS children \
         WHERE l.lead_id = $id OR $id IN children RETURN l.lead_id AS id LIMIT 2"
                    .into(),
            )
            .param("owned", parent.lead_ids.clone())
            .param("tenant_id", tenant_id.to_string())
            .param("id", id.to_string()),
        )
        .await
        .map_err(|_| "Payment authorization unavailable".to_string())?;
    let found = rows
        .next()
        .await
        .map_err(|_| "Payment authorization unavailable".to_string())?
        .is_some();
    let duplicate = rows
        .next()
        .await
        .map_err(|_| "Payment authorization unavailable".to_string())?
        .is_some();
    Ok(found && !duplicate)
}

pub fn owns_lead(auth: &ParentAuth, lead_id: Option<&str>) -> bool {
    let Some(lead_id) = lead_id else {
        return false;
    };
    auth.lead_ids.iter().any(|id| id == lead_id)
}

fn staff_claims_from_bearer(
    headers: &HeaderMap,
    parent_secret: &str,
) -> Result<Claims, (StatusCode, String)> {
    let token = bearer_from(headers).ok_or((StatusCode::UNAUTHORIZED, "Missing bearer".into()))?;
    let header = jsonwebtoken::decode_header(&token)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid bearer".into()))?;
    if header.typ.as_deref() != Some("staff-api+jwt") {
        return claims_from_bearer(headers, parent_secret);
    }
    let (key, issuer) = crate::config::config::staff_downstream_settings(parent_secret)
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;
    let typed = crate::utils::jwt::decode_staff_api_claims(&token, &key, &issuer)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid staff credential".into()))?;
    Ok(Claims {
        sub: typed.sub,
        staff_member_id: Some(typed.staff_member_id),
        _exp: typed.exp as usize,
        email: None,
        scope: Some(typed.scope),
    })
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
    use super::{finance_roles_allowed, staff_roles, AdminAuth};

    fn roles(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    /// Matrix 2026-10-06: finance, admissions managers and owner approve
    /// money; marketing and admissions staff never do.
    #[test]
    fn money_approval_follows_the_role_matrix() {
        for role in [
            "finance_approver",
            "finance_admin",
            "admissions_manager",
            "admissions_admin",
            "owner",
        ] {
            assert!(finance_roles_allowed(&roles(&[role]), true), "{role}");
            assert!(finance_roles_allowed(&roles(&[role]), false), "{role}");
        }
        for role in [
            "admissions_staff",
            "marketing_staff",
            "marketing_manager",
            "parent",
        ] {
            assert!(!finance_roles_allowed(&roles(&[role]), true), "{role}");
            assert!(!finance_roles_allowed(&roles(&[role]), false), "{role}");
        }
    }

    #[test]
    fn only_staff_roles_survive_and_marketing_staff_are_lead_scoped() {
        assert!(staff_roles(roles(&["parent", "student", " "])).is_empty());
        assert_eq!(
            staff_roles(roles(&["parent", "marketing_staff"])),
            roles(&["marketing_staff"])
        );
        let auth = |values: &[&str]| AdminAuth {
            email: "staff@example.test".into(),
            roles: roles(values),
        };
        assert!(auth(&["marketing_staff"]).lead_scoped());
        assert!(!auth(&["marketing_staff", "finance_approver"]).lead_scoped());
        assert!(!auth(&["marketing_manager"]).lead_scoped());
        assert!(!AdminAuth::owner("o@example.test".into()).lead_scoped());
    }
}
