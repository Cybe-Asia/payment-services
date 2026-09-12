//! Read-only bridge to the auth-owned Staff Directory. Never resolve permissions by email.
use neo4rs::{query, Graph};

#[derive(Debug, PartialEq)]
pub enum CanonicalAdmin {
    NotLinked,
    Denied,
    Owner(String),
}

// Preserve all links, including inactive memberships, so suspension and ambiguous
// identity links cannot fall through to the legacy email allowlist.
const LOOKUP: &str = "MATCH (u:User {id:$userId})-[:STAFF_MEMBER]->(s:StaffMember) \
 RETURN s.id AS id, coalesce(s.membershipStatus,'') AS status, \
 coalesce(s.roles,[]) AS roles, coalesce(s.tenantIds,[]) AS tenantIds, \
 coalesce(s.schoolIds,[]) AS schoolIds LIMIT 2";

pub async fn resolve_admin(graph: &Graph, user_id: &str) -> Result<CanonicalAdmin, String> {
    let lookup = if user_id.starts_with("LEAD-") {
        LOOKUP.replace(
            "MATCH (u:User {id:$userId})",
            "MATCH (u:User)-[:HAS_APPLICATION]->(:Lead {lead_id:$userId}) MATCH (u)",
        )
    } else {
        LOOKUP.to_string()
    };
    let mut rows = graph
        .execute(query(&lookup).param("userId", user_id.to_string()))
        .await
        .map_err(|_| "Staff authorization unavailable".to_string())?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| "Staff authorization unavailable".to_string())?
    else {
        return Ok(CanonicalAdmin::NotLinked);
    };
    if rows
        .next()
        .await
        .map_err(|_| "Staff authorization unavailable".to_string())?
        .is_some()
    {
        return Ok(CanonicalAdmin::Denied);
    }
    let id = row.get::<String>("id").unwrap_or_default();
    let status = row.get::<String>("status").unwrap_or_default();
    let roles = row.get::<Vec<String>>("roles").unwrap_or_default();
    let tenants = row
        .get::<Vec<String>>("tenantIds")
        .ok_or_else(|| "Invalid staff scope".to_string())?;
    let schools = row
        .get::<Vec<String>>("schoolIds")
        .ok_or_else(|| "Invalid staff scope".to_string())?;
    // These legacy endpoints expose global queues and mutations. Only an active,
    // unscoped owner can use this bridge; scoped staff need resource-level guards.
    if !id.is_empty()
        && status == "ACTIVE"
        && roles.iter().any(|role| role == "owner")
        && tenants.is_empty()
        && schools.is_empty()
    {
        Ok(CanonicalAdmin::Owner(id))
    } else {
        Ok(CanonicalAdmin::Denied)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
    async fn canonical_admin_requires_unique_active_unscoped_owner() {
        let graph = Graph::new(
            std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
            "neo4j",
            "test",
        )
        .await
        .unwrap();
        let user = format!("staff-guard-{}", uuid::Uuid::new_v4());
        assert_eq!(
            resolve_admin(&graph, &user).await.unwrap(),
            CanonicalAdmin::NotLinked
        );
        graph.run(query("CREATE (u:User {id:$id})-[:STAFF_MEMBER]->(:StaffMember {id:$id,membershipStatus:'ACTIVE',roles:['owner'],tenantIds:[],schoolIds:[]})").param("id",user.clone())).await.unwrap();
        assert_eq!(
            resolve_admin(&graph, &user).await.unwrap(),
            CanonicalAdmin::Owner(user.clone())
        );
        let lead = format!("LEAD-{}", uuid::Uuid::new_v4());
        graph.run(query("MATCH (u:User {id:$id}) CREATE (u)-[:HAS_APPLICATION]->(:Lead {lead_id:$lead})").param("id",user.clone()).param("lead",lead.clone())).await.unwrap();
        assert_eq!(
            resolve_admin(&graph, &lead).await.unwrap(),
            CanonicalAdmin::Owner(user.clone())
        );
        for (status, roles, tenants, schools) in [
            ("SUSPENDED", vec!["owner"], vec![], vec![]),
            ("INVITED", vec!["owner"], vec![], vec![]),
            ("ACTIVE", vec!["school_admin"], vec![], vec![]),
            ("ACTIVE", vec!["admissions_staff"], vec![], vec![]),
            ("ACTIVE", vec!["finance_admin"], vec![], vec![]),
            ("ACTIVE", vec![], vec![], vec![]),
            ("ACTIVE", vec!["owner"], vec!["other-tenant"], vec![]),
            ("ACTIVE", vec!["owner"], vec![], vec!["school-1"]),
        ] {
            graph.run(query("MATCH (s:StaffMember {id:$id}) SET s.membershipStatus=$status,s.roles=$roles,s.tenantIds=$tenants,s.schoolIds=$schools")
                .param("id",user.clone()).param("status",status).param("roles",roles).param("tenants",tenants).param("schools",schools)).await.unwrap();
            assert_eq!(
                resolve_admin(&graph, &user).await.unwrap(),
                CanonicalAdmin::Denied
            );
            assert_eq!(
                resolve_admin(&graph, &lead).await.unwrap(),
                CanonicalAdmin::Denied
            );
        }
        graph.run(query("MATCH (u:User {id:$id}) CREATE (u)-[:STAFF_MEMBER]->(:StaffMember {id:$id,membershipStatus:'ACTIVE',roles:['owner']})").param("id",user.clone())).await.unwrap();
        assert_eq!(
            resolve_admin(&graph, &user).await.unwrap(),
            CanonicalAdmin::Denied
        );
        graph
            .run(query("MATCH (n:Lead {lead_id:$id}) DETACH DELETE n").param("id", lead))
            .await
            .unwrap();
        graph
            .run(
                query("MATCH (n) WHERE (n:User OR n:StaffMember) AND n.id=$id DETACH DELETE n")
                    .param("id", user),
            )
            .await
            .unwrap();
    }
}
