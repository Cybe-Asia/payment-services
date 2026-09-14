//! Read-only bridge to the auth-owned Staff Directory. Never resolve permissions by email.
use neo4rs::{query, Graph};

#[derive(Debug, PartialEq)]
pub enum CanonicalAdmin {
    NotLinked,
    Denied,
    Owner(String),
}

#[derive(Debug, PartialEq)]
pub enum CanonicalStaff {
    NotLinked,
    Denied,
    Active { id: String, roles: Vec<String> },
}

pub async fn resolve_admin(graph: &Graph, user_id: &str) -> Result<CanonicalAdmin, String> {
    Ok(match resolve_staff(graph, user_id).await? {
        CanonicalStaff::NotLinked => CanonicalAdmin::NotLinked,
        CanonicalStaff::Active { id, roles } if roles.iter().any(|r| r == "owner") => {
            CanonicalAdmin::Owner(id)
        }
        _ => CanonicalAdmin::Denied,
    })
}

// Preserve all links, including inactive memberships, so suspension and ambiguous
// identity links cannot fall through to the legacy email allowlist.
const LOOKUP: &str = "MATCH (u:User {id:$userId})-[:STAFF_MEMBER]->(s:StaffMember) \
 RETURN s.id AS id, coalesce(s.membershipStatus,'') AS status, \
 coalesce(s.roles,[]) AS roles, coalesce(s.tenantIds,[]) AS tenantIds, \
 coalesce(s.schoolIds,[]) AS schoolIds, coalesce(s.teamIds,[]) AS teamIds LIMIT 2";

pub async fn resolve_staff(graph: &Graph, user_id: &str) -> Result<CanonicalStaff, String> {
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
        return Ok(CanonicalStaff::NotLinked);
    };
    if rows
        .next()
        .await
        .map_err(|_| "Staff authorization unavailable".to_string())?
        .is_some()
    {
        return Ok(CanonicalStaff::Denied);
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
    let teams = row
        .get::<Vec<String>>("teamIds")
        .ok_or_else(|| "Invalid staff scope".to_string())?;
    // These legacy endpoints expose global queues and mutations. Only active,
    // unscoped memberships can use this bridge; callers enforce role capabilities.
    // Scoped staff remain denied until a resource-level guard is present.
    if !id.is_empty()
        && status == "ACTIVE"
        && !roles.is_empty()
        && tenants.is_empty()
        && schools.is_empty()
        && teams.is_empty()
    {
        Ok(CanonicalStaff::Active { id, roles })
    } else {
        Ok(CanonicalStaff::Denied)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires Auth-issued STAFF_TEST_FIXTURE and disposable Neo4j"]
    async fn auth_issued_token_obeys_finance_gate_and_rejects_parent_replay() {
        use axum::http::{HeaderMap, StatusCode};
        let fixture: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::env::var("STAFF_TEST_FIXTURE").unwrap()).unwrap(),
        )
        .unwrap();
        let user = fixture["user_id"].as_str().unwrap();
        let staff = fixture["staff_id"].as_str().unwrap();
        let token = fixture["token"].as_str().unwrap();
        let parent_key = fixture["parent_key"].as_str().unwrap();
        let graph = Graph::new(
            std::env::var("NEO4J_URI").unwrap().as_str(),
            "neo4j",
            "test",
        )
        .await
        .unwrap();
        graph.run(query("MATCH (n) WHERE (n:User AND n.id=$user) OR (n:StaffMember AND n.id=$staff) DETACH DELETE n").param("user",user).param("staff",staff)).await.unwrap();
        graph.run(query("CREATE (u:User {id:$user,email:'contract@example.test',marketingRoles:['owner']})-[:STAFF_MEMBER]->(:StaffMember {id:$staff,membershipStatus:'ACTIVE',roles:['finance_approver'],tenantIds:[],schoolIds:[],teamIds:[]})").param("user",user).param("staff",staff)).await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", format!("Bearer {token}").parse().unwrap());
        for approve in [false, true] {
            assert!(
                crate::utils::auth::require_finance(&graph, &headers, parent_key, approve)
                    .await
                    .is_ok()
            );
        }
        assert_eq!(
            crate::utils::auth::require_parent_auth(&graph, &headers, parent_key)
                .await
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        for (status, teams) in [("SUSPENDED", vec![]), ("ACTIVE", vec!["team-other"])] {
            graph.run(query("MATCH (s:StaffMember {id:$staff}) SET s.membershipStatus=$status,s.teamIds=$teams").param("staff",staff).param("status",status).param("teams",teams)).await.unwrap();
            for approve in [false, true] {
                assert_eq!(
                    crate::utils::auth::require_finance(&graph, &headers, parent_key, approve)
                        .await
                        .unwrap_err()
                        .0,
                    StatusCode::FORBIDDEN
                );
            }
        }
        graph.run(query("MATCH (n) WHERE (n:User AND n.id=$user) OR (n:StaffMember AND n.id=$staff) DETACH DELETE n").param("user",user).param("staff",staff)).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
    async fn finance_uses_canonical_roles_and_fails_closed_on_suspension_or_scope() {
        use axum::http::{HeaderMap, StatusCode};
        use jsonwebtoken::{encode, EncodingKey, Header};
        let graph = Graph::new(
            std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
            "neo4j",
            "test",
        )
        .await
        .unwrap();
        let id = format!("finance-guard-{}", uuid::Uuid::new_v4());
        let email = format!("{id}@example.test");
        graph.run(query("CREATE (u:User {id:$id,email:$email,marketingRoles:['finance_admin']})-[:STAFF_MEMBER]->(:StaffMember {id:$id,membershipStatus:'ACTIVE',roles:['finance_approver'],tenantIds:[],schoolIds:[]})")
            .param("id", id.clone()).param("email", email.clone())).await.unwrap();
        let token = encode(
            &Header::default(),
            &serde_json::json!({"sub":id,"email":email,"exp":chrono::Utc::now().timestamp()+300}),
            &EncodingKey::from_secret(b"synthetic-guard-key"),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", format!("Bearer {token}").parse().unwrap());
        for approve in [false, true] {
            assert!(crate::utils::auth::require_finance(
                &graph,
                &headers,
                "synthetic-guard-key",
                approve
            )
            .await
            .is_ok());
        }
        graph
            .run(
                query("MATCH (s:StaffMember {id:$id}) SET s.roles=['admissions_manager']")
                    .param("id", id.clone()),
            )
            .await
            .unwrap();
        assert!(crate::utils::auth::require_finance(
            &graph,
            &headers,
            "synthetic-guard-key",
            false
        )
        .await
        .is_ok());
        assert_eq!(
            crate::utils::auth::require_finance(&graph, &headers, "synthetic-guard-key", true)
                .await
                .unwrap_err()
                .0,
            StatusCode::FORBIDDEN
        );
        for (status, schools) in [
            ("SUSPENDED", vec![]),
            ("INVITED", vec![]),
            ("ACTIVE", vec!["school-other"]),
        ] {
            graph.run(query("MATCH (s:StaffMember {id:$id}) SET s.membershipStatus=$status,s.roles=['finance_admin'],s.schoolIds=$schools")
                .param("id",id.clone()).param("status",status).param("schools",schools)).await.unwrap();
            for approve in [false, true] {
                assert_eq!(
                    crate::utils::auth::require_finance(
                        &graph,
                        &headers,
                        "synthetic-guard-key",
                        approve
                    )
                    .await
                    .unwrap_err()
                    .0,
                    StatusCode::FORBIDDEN
                );
            }
            assert_eq!(
                crate::utils::auth::require_staff(&graph, &headers, "synthetic-guard-key")
                    .await
                    .unwrap_err()
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        graph
            .run(
                query("MATCH (n) WHERE (n:User OR n:StaffMember) AND n.id=$id DETACH DELETE n")
                    .param("id", id),
            )
            .await
            .unwrap();
    }

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
        graph.run(query("MATCH (s:StaffMember {id:$id}) SET s.membershipStatus='ACTIVE',s.roles=['owner'],s.tenantIds=[],s.schoolIds=[],s.teamIds=['other-team']").param("id", user.clone())).await.unwrap();
        assert_eq!(
            resolve_admin(&graph, &user).await.unwrap(),
            CanonicalAdmin::Denied
        );
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
