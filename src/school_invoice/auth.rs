use super::{failure, Failure};
use axum::http::{HeaderMap, StatusCode};

#[derive(Clone, Debug)]
pub struct Staff {
    pub subject: String,
    pub id: String,
    pub expires: i64,
}
#[derive(Clone, Debug)]
pub struct Parent {
    pub subject: String,
    pub expires: i64,
}
pub fn live(expires: i64) -> bool {
    chrono::Utc::now().timestamp() < expires
}
fn bearer(h: &HeaderMap) -> Result<&str, Failure> {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty() && v.len() <= 8192)
        .ok_or_else(|| failure(StatusCode::UNAUTHORIZED))
}
pub fn staff(h: &HeaderMap, parent_key: &str) -> Result<Staff, Failure> {
    let (key, issuer) = crate::config::config::staff_downstream_settings(parent_key)
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE))?;
    let claims = crate::utils::jwt::decode_staff_api_claims(bearer(h)?, &key, &issuer)
        .map_err(|_| failure(StatusCode::UNAUTHORIZED))?;
    if !super::model::identifier(&claims.sub) || !super::model::identifier(&claims.staff_member_id)
    {
        return Err(failure(StatusCode::UNAUTHORIZED));
    }
    let expires = i64::try_from(claims.exp).map_err(|_| failure(StatusCode::UNAUTHORIZED))?;
    if !live(expires) {
        return Err(failure(StatusCode::UNAUTHORIZED));
    }
    Ok(Staff {
        subject: claims.sub,
        id: claims.staff_member_id,
        expires,
    })
}
pub fn parent(h: &HeaderMap, key: &str) -> Result<Parent, Failure> {
    let c = crate::utils::jwt::decode_session_claims(bearer(h)?, key)
        .map_err(|_| failure(StatusCode::UNAUTHORIZED))?;
    // This contract binds a canonical User, never an email-resolved legacy Lead.
    if !super::model::identifier(&c.sub) || c.sub.starts_with("LEAD-") {
        return Err(failure(StatusCode::UNAUTHORIZED));
    }
    let expires = i64::try_from(c._exp).map_err(|_| failure(StatusCode::UNAUTHORIZED))?;
    if !live(expires) {
        return Err(failure(StatusCode::UNAUTHORIZED));
    }
    Ok(Parent {
        subject: c.sub,
        expires,
    })
}
/// Fresh Auth-owned directory guard, evaluated inside every graph transaction.
/// Unscoped Owner follows Auth's global-owner semantics. Scoped memberships must
/// explicitly contain BOTH the invoice's school and tenant; team-only scope is
/// unsupported and cannot become global authority.
pub const STAFF: &str = "MATCH (staff_user:User {id:$subject}) OPTIONAL MATCH (staff_user)-[staff_link:STAFF_MEMBER]->(member:StaffMember) WITH collect(DISTINCT staff_user) AS users,collect(DISTINCT member) AS members,collect(DISTINCT staff_link) AS links WHERE size(users)=1 AND size(members)=1 AND size(links)=1 WITH head(members) AS actor WHERE actor.id=$staff AND NOT EXISTS { MATCH(other:StaffMember {id:actor.id}) WHERE other<>actor } AND NOT EXISTS { MATCH(other_user:User)-[:STAFF_MEMBER]->(actor) WHERE coalesce(other_user.id,'')<>$subject } AND actor.membershipStatus='ACTIVE' AND any(role IN coalesce(actor.roles,[]) WHERE role IN $roles) AND size(coalesce(actor.teamIds,[]))=0 AND datetime.realtime().epochSeconds < $expires";
/// Carry only Payment-owned locks into a fresh read of Auth authority.
pub fn fresh(kept: &str) -> String {
    STAFF
        .replace("WITH collect", &format!("WITH {kept},collect"))
        .replace("WITH head(members)", &format!("WITH {kept},head(members)"))
}
pub fn scope(school: &str, tenant: &str) -> String {
    format!("((('owner' IN coalesce(actor.roles,[])) AND size(coalesce(actor.schoolIds,[]))=0 AND size(coalesce(actor.tenantIds,[]))=0) OR ({school} IN coalesce(actor.schoolIds,[]) AND {tenant} IN coalesce(actor.tenantIds,[])))")
}
pub fn roles(review: bool) -> Vec<String> {
    if review {
        vec!["finance_admin", "finance_approver", "owner"]
    } else {
        vec!["finance_admin", "owner"]
    }
    .into_iter()
    .map(str::to_string)
    .collect()
}
