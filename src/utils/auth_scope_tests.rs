//! Graph-backed checks for the staff payment guards: legacy parent roles never
//! open staff endpoints, marketing staff stay inside their lead scope, and a
//! reviewer cannot approve evidence they uploaded.
use super::auth::{require_staff, staff_can_reach_lead, AdminAuth};
use crate::repositories::payment_repository::actor_uploaded_live_proof;
use axum::http::{HeaderMap, StatusCode};
use jsonwebtoken::{encode, EncodingKey, Header};
use neo4rs::{query, Graph};

const KEY: &str = "synthetic-guard-key";

fn bearer(sub: &str, email: &str) -> HeaderMap {
    let token = encode(
        &Header::default(),
        &serde_json::json!({"sub": sub, "email": email, "exp": chrono::Utc::now().timestamp() + 300}),
        &EncodingKey::from_secret(KEY.as_bytes()),
    )
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("Authorization", format!("Bearer {token}").parse().unwrap());
    headers
}

fn staff(email: &str, role: &str) -> AdminAuth {
    AdminAuth {
        email: email.to_string(),
        roles: vec![role.to_string()],
    }
}

#[tokio::test]
#[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
async fn staff_guards_scope_and_segregation_of_duties() {
    std::env::remove_var("ADMIN_EMAILS");
    let graph = Graph::new(
        std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
        "neo4j",
        "test",
    )
    .await
    .unwrap();
    let run = uuid::Uuid::new_v4().simple().to_string();
    let tag = format!("scope-{run}");
    let parent = format!("{tag}-parent@example.test");
    let mine = format!("{tag}-mine@example.test");
    let other = format!("{tag}-other@example.test");
    graph
        .run(
            query(
                "CREATE (:User {id:$tag+'-parent', email:$parent, roles:['parent']}) \
                 CREATE (:User {id:$tag+'-legacy', email:$tag+'-legacy@example.test', marketingRoles:['marketing_staff']}) \
                 CREATE (:Lead {lead_id:$tag+'-assigned', assigned_admin_email:$mine, reference_code:'REF-X'}) \
                 CREATE (:Lead {lead_id:$tag+'-foreign', assigned_admin_email:$other, reference_code:'REF-Y'})-[:HAS_STUDENT]->(:Student {studentId:$tag+'-stu'}) \
                 CREATE (:Lead {lead_id:$tag+'-pool'}) \
                 CREATE (:Lead {lead_id:$tag+'-claimed', assigned_admin_email:$other}) \
                 CREATE (p:Payment {payment_id:$tag+'-pay'})-[:HAS_PROOF]->(:PaymentProof {payment_proof_id:$tag+'-p1', uploaded_by:$mine, status:'uploaded'}) \
                 CREATE (p)-[:HAS_PROOF]->(:PaymentProof {payment_proof_id:$tag+'-p0', uploaded_by:$other, status:'rejected'})",
            )
            .param("tag", tag.clone())
            .param("parent", parent.clone())
            .param("mine", mine.clone())
            .param("other", other.clone()),
        )
        .await
        .unwrap();

    // A parent's legacy `User.roles` never passes the staff gate.
    let denied = require_staff(&graph, &bearer(&format!("{tag}-parent"), &parent), KEY)
        .await
        .unwrap_err();
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    let legacy = require_staff(
        &graph,
        &bearer(
            &format!("{tag}-legacy"),
            &format!("{tag}-legacy@example.test"),
        ),
        KEY,
    )
    .await
    .unwrap();
    assert_eq!(legacy.roles, vec!["marketing_staff".to_string()]);
    assert!(legacy.lead_scoped());

    // Marketing staff: own + unassigned pool only; managers reach everything.
    let marketing = staff(&mine, "marketing_staff");
    for (lead, want) in [
        (format!("{tag}-assigned"), true),
        (format!("{tag}-pool"), true),
        (format!("{tag}-claimed"), false),
        (format!("{tag}-foreign"), false),
        (format!("{tag}-stu"), false),
        (format!("{tag}-missing"), false),
    ] {
        assert_eq!(
            staff_can_reach_lead(&graph, &marketing, &lead)
                .await
                .unwrap(),
            want,
            "{lead}"
        );
    }
    let manager = staff(&mine, "marketing_manager");
    assert!(
        staff_can_reach_lead(&graph, &manager, &format!("{tag}-foreign"))
            .await
            .unwrap()
    );

    // Uploader != approver; rejected evidence does not block a reviewer.
    let pay = format!("{tag}-pay");
    assert!(
        actor_uploaded_live_proof(&graph, &pay, &mine.to_uppercase())
            .await
            .unwrap()
    );
    assert!(!actor_uploaded_live_proof(&graph, &pay, &other)
        .await
        .unwrap());

    graph
        .run(
            query(
                "MATCH (n) WHERE any(k IN ['id','lead_id','payment_id','payment_proof_id','studentId'] \
                 WHERE toString(n[k]) STARTS WITH $tag) DETACH DELETE n",
            )
            .param("tag", tag),
        )
        .await
        .unwrap();
}
