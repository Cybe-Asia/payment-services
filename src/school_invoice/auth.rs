use super::{failure, Failure};
use crate::utils::capabilities::{self, SchoolReach};
use axum::http::{HeaderMap, StatusCode};
use neo4rs::{query, Graph, Query};

#[derive(Clone, Debug)]
pub struct Staff {
    pub subject: String,
    pub id: String,
    pub expires: i64,
    /// Where SIS says this Staff Member holds `billing.school_invoice.manage`
    /// (ADR 0018 slice 6). Filled only by `with_capability`, only after the
    /// role check denied; empty keeps every statement's answer as today.
    pub capability: SchoolReach,
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
        capability: SchoolReach::default(),
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
pub const STAFF: &str = "MATCH (staff_user:User {id:$subject}) OPTIONAL MATCH (staff_user)-[staff_link:STAFF_MEMBER]->(member:StaffMember) WITH collect(DISTINCT staff_user) AS users,collect(DISTINCT member) AS members,collect(DISTINCT staff_link) AS links WHERE size(users)=1 AND size(members)=1 AND size(links)=1 WITH head(members) AS actor WHERE actor.id=$staff AND NOT EXISTS { MATCH(other:StaffMember {id:actor.id}) WHERE other<>actor } AND NOT EXISTS { MATCH(other_user:User)-[:STAFF_MEMBER]->(actor) WHERE coalesce(other_user.id,'')<>$subject } AND actor.membershipStatus='ACTIVE' AND (any(role IN coalesce(actor.roles,[]) WHERE role IN $roles) OR $capable) AND size(coalesce(actor.teamIds,[]))=0 AND datetime.realtime().epochSeconds < $expires";
/// Carry only Payment-owned locks into a fresh read of Auth authority.
pub fn fresh(kept: &str) -> String {
    STAFF
        .replace("WITH collect", &format!("WITH {kept},collect"))
        .replace("WITH head(members)", &format!("WITH {kept},head(members)"))
}
/// Today's rule needs a matching role (re-checked here, since `STAFF` also lets
/// a capability holder through) plus the membership scope. ADR 0018 slice 6 adds
/// one branch: `billing.school_invoice.manage` held at this school or for its
/// whole tenant, and the actor's own membership naming both. `$capable` is false
/// unless `with_capability` filled the reach, so the rule is then unchanged.
pub fn scope(school: &str, tenant: &str) -> String {
    format!("((any(role IN coalesce(actor.roles,[]) WHERE role IN $roles) AND ((('owner' IN coalesce(actor.roles,[])) AND size(coalesce(actor.schoolIds,[]))=0 AND size(coalesce(actor.tenantIds,[]))=0) OR ({school} IN coalesce(actor.schoolIds,[]) AND {tenant} IN coalesce(actor.tenantIds,[])))) OR ($capable AND ({tenant} IN $capable_tenants OR ({tenant}+'|'+{school}) IN $capable_schools) AND {school} IN coalesce(actor.schoolIds,[]) AND {tenant} IN coalesce(actor.tenantIds,[])))")
}

impl Staff {
    /// Every staff guard parameter, with this request's capability reach.
    pub fn bind(&self, q: Query, roles: Vec<String>) -> Query {
        self.bind_reach(q, roles, &self.capability)
    }
    /// Owner-only statements never take a capability.
    pub fn bind_owner_only(&self, q: Query) -> Query {
        self.bind_reach(q, vec!["owner".to_string()], &SchoolReach::default())
    }
    fn bind_reach(&self, q: Query, roles: Vec<String>, reach: &SchoolReach) -> Query {
        q.param("subject", self.subject.clone())
            .param("staff", self.id.clone())
            .param("roles", roles)
            .param("expires", self.expires)
            .param("capable", reach.any())
            .param("capable_tenants", reach.tenants.clone())
            .param("capable_schools", reach.schools.clone())
    }
}

/// ADR 0018 slice 6: the role check runs first, exactly as the statements do
/// (`STAFF` with `roles(review)` and no capability). Only when it finds no
/// actor does the request ask SIS where the caller holds
/// `billing.school_invoice.manage`. Off, failing or foreign answers leave the
/// reach empty, so the statements deny as today.
pub async fn with_capability(
    graph: Option<&Graph>,
    h: &HeaderMap,
    mut a: Staff,
    review: bool,
) -> Staff {
    a.capability = SchoolReach::default();
    let Some(graph) = graph else {
        return a;
    };
    if !capabilities::enabled() {
        return a;
    }
    let check = a.bind(
        query(&format!("{STAFF} RETURN actor.id AS id LIMIT 1")),
        roles(review),
    );
    let by_role = match graph.execute(check).await {
        Ok(mut rows) => rows.next().await.map(|row| row.is_some()),
        Err(e) => Err(e),
    };
    // A graph failure is left to the statement itself, which reports it as today.
    if !matches!(by_role, Ok(false)) {
        return a;
    }
    let Ok(token) = bearer(h) else {
        return a;
    };
    let answer = capabilities::held(token, &[capabilities::SCHOOL_INVOICE_MANAGE]).await;
    a.capability = capabilities::school_reach(&answer, &a.id, capabilities::SCHOOL_INVOICE_MANAGE);
    a
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
