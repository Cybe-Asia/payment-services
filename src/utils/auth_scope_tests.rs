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
        capability: false,
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

/// ADR 0018 slice 6: the finance and assisted-submit gates open through a
/// tenant-wide SIS capability only after roles deny, only for Auth's typed staff
/// credential naming the User's own Staff Member, and SIS failures deny.
#[tokio::test]
#[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
async fn legacy_gates_open_by_tenant_capability_only_after_roles_deny() {
    use super::auth::require_finance;
    use crate::utils::capabilities::{stub, PAYMENT_MANAGE, PAYMENT_READ, UPLOAD_ON_BEHALF};
    const STAFF_KEY: &str = "synthetic-staff-downstream-key-over-32-bytes";
    const ISSUER: &str = "https://auth.example.test";
    const T: &str = "TENANT-001";
    std::env::remove_var("ADMIN_EMAILS");
    std::env::remove_var("TENANT_ID");
    std::env::set_var("STAFF_DOWNSTREAM_JWT_SECRET", STAFF_KEY);
    std::env::set_var("STAFF_DOWNSTREAM_JWT_ISSUER", ISSUER);
    let graph = Graph::new(
        std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
        "neo4j",
        "test",
    )
    .await
    .unwrap();
    let tag = format!("cap-{}", uuid::Uuid::new_v4().simple());
    let id = |name: &str| format!("{tag}-{name}");
    graph
        .run(
            query(
                "CREATE (:User {id:$tag+'-admin', email:$tag+'-admin@example.test'})-[:STAFF_MEMBER]->(:StaffMember {id:$tag+'-admin-staff', membershipStatus:'ACTIVE', roles:['school_admin'], tenantIds:['TENANT-001'], schoolIds:['SCH-X'], teamIds:[]}) \
                 CREATE (:User {id:$tag+'-finance', email:$tag+'-finance@example.test'})-[:STAFF_MEMBER]->(:StaffMember {id:$tag+'-finance-staff', membershipStatus:'ACTIVE', roles:['finance_admin'], tenantIds:[], schoolIds:[], teamIds:[]}) \
                 CREATE (:User {id:$tag+'-mkt', email:$tag+'-mkt@example.test'})-[:STAFF_MEMBER]->(:StaffMember {id:$tag+'-mkt-staff', membershipStatus:'ACTIVE', roles:['marketing_staff'], tenantIds:[], schoolIds:[], teamIds:[]})",
            )
            .param("tag", tag.clone()),
        )
        .await
        .unwrap();
    let typed = |user: &str, staff: &str| {
        let now = chrono::Utc::now().timestamp();
        let header = Header {
            typ: Some("staff-api+jwt".into()),
            ..Default::default()
        };
        let token = encode(
            &header,
            &serde_json::json!({"iss":ISSUER,"aud":crate::utils::jwt::STAFF_API_AUDIENCE,"scope":crate::utils::jwt::STAFF_API_PURPOSE,"sub":user,"staff_member_id":staff,"iat":now,"exp":now+300}),
            &EncodingKey::from_secret(STAFF_KEY.as_bytes()),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", format!("Bearer {token}").parse().unwrap());
        headers
    };
    let admin = typed(&id("admin"), &id("admin-staff"));
    let status = |r: Result<super::auth::AdminAuth, (StatusCode, String)>| match r {
        Ok(_) => StatusCode::OK,
        Err((s, _)) => s,
    };
    let graph = &graph;
    let gates = |h: HeaderMap| async move {
        [
            status(require_finance(graph, &h, KEY, false).await),
            status(require_finance(graph, &h, KEY, true).await),
            status(require_staff(graph, &h, KEY).await),
        ]
    };
    const DENIED: [StatusCode; 3] = [StatusCode::FORBIDDEN; 3];

    // SIS off: today's answers, nobody asked.
    std::env::remove_var("SIS_ACCESS_ORIGIN");
    let sis = stub::Sis::start().await;
    sis.holds(&id("admin-staff"), &[(PAYMENT_MANAGE, &[T], &[])]);
    assert_eq!(gates(admin.clone()).await, DENIED);
    assert!(sis.calls().is_empty());
    std::env::set_var("SIS_ACCESS_ORIGIN", &sis.origin);

    // Each capability opens only its own gates, tenant-wide only.
    let (ok, no) = (StatusCode::OK, StatusCode::FORBIDDEN);
    for (entries, want) in [
        (vec![], DENIED),
        (vec![(PAYMENT_READ, vec![T], vec![])], [ok, no, no]),
        (vec![(PAYMENT_MANAGE, vec![T], vec![])], [ok, ok, no]),
        (vec![(UPLOAD_ON_BEHALF, vec![T], vec![])], [no, no, ok]),
        (vec![(PAYMENT_MANAGE, vec!["TENANT-OTHER"], vec![])], DENIED),
        (
            vec![
                (PAYMENT_READ, vec![], vec![(T, "SCH-X")]),
                (PAYMENT_MANAGE, vec![], vec![(T, "SCH-X")]),
                (UPLOAD_ON_BEHALF, vec![], vec![(T, "SCH-X")]),
            ],
            DENIED,
        ),
    ] {
        let entries: Vec<stub::Entry> = entries
            .iter()
            .map(|(c, t, s)| (*c, t.as_slice(), s.as_slice()))
            .collect();
        sis.holds(&id("admin-staff"), &entries);
        assert_eq!(gates(admin.clone()).await, want, "{entries:?}");
    }
    sis.holds(&id("admin-staff"), &[(UPLOAD_ON_BEHALF, &[T], &[])]);
    let assisted = require_staff(graph, &admin, KEY).await.unwrap();
    assert!(assisted.capability && !assisted.lead_scoped());

    // SIS failure, or an answer about another Staff Member: denied.
    sis.reply(503, serde_json::json!({}));
    assert_eq!(gates(admin.clone()).await, DENIED);
    sis.holds("someone-else", &[(PAYMENT_MANAGE, &[T], &[])]);
    assert_eq!(gates(admin.clone()).await, DENIED);

    // A credential naming a Staff Member the User does not link to never asks.
    sis.holds(
        &id("other-staff"),
        &[(PAYMENT_MANAGE, &[T], &[]), (UPLOAD_ON_BEHALF, &[T], &[])],
    );
    sis.forget();
    assert_eq!(gates(typed(&id("admin"), &id("other-staff"))).await, DENIED);
    assert!(sis.calls().is_empty());

    // Role holders keep their answer and never ask SIS.
    sis.holds(&id("finance-staff"), &[(PAYMENT_MANAGE, &[T], &[])]);
    assert_eq!(
        gates(typed(&id("finance"), &id("finance-staff"))).await,
        [ok, ok, ok]
    );
    let mkt = typed(&id("mkt"), &id("mkt-staff"));
    let marketing = require_staff(graph, &mkt, KEY).await.unwrap();
    assert!(!marketing.capability && marketing.lead_scoped());
    assert!(sis.calls().is_empty());
    // A role-denied active member may pass the finance gate by capability.
    sis.holds(&id("mkt-staff"), &[(PAYMENT_READ, &[T], &[])]);
    assert_eq!(gates(mkt).await, [ok, no, ok]);
    assert_eq!(sis.calls().len(), 2);

    // A legacy session token never asks SIS.
    sis.forget();
    sis.holds(&id("admin-staff"), &[(PAYMENT_MANAGE, &[T], &[])]);
    assert_eq!(
        gates(bearer(&id("admin"), &format!("{tag}-admin@example.test"))).await,
        DENIED
    );
    assert!(sis.calls().is_empty());

    std::env::remove_var("SIS_ACCESS_ORIGIN");
    graph
        .run(
            query("MATCH (n) WHERE (n:User OR n:StaffMember) AND n.id STARTS WITH $tag DETACH DELETE n")
                .param("tag", tag),
        )
        .await
        .unwrap();
}
