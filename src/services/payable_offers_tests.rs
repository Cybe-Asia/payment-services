//! Staff desk payments for enrolment: only parent-accepted, unpaid offers
//! are payable, priced from the accepted snapshot.
use neo4rs::{query, Graph};

#[tokio::test]
#[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
async fn only_accepted_unpaid_offers_are_payable_by_staff() {
    let graph = Graph::new(
        std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
        "neo4j",
        "test",
    )
    .await
    .unwrap();
    let tag = format!("payable-{}", uuid::Uuid::new_v4().simple());
    let pricing =
        r#"{"snapshotVersion":"offer-pricing-v1","currency":"IDR","amountDueNow":15000000}"#;
    graph
        .run(
            query(
                "CREATE (l:Lead {lead_id:$tag+'-lead', tenant_id:'T1'}) \
                 CREATE (l)-[:HAS_STUDENT]->(a:Student {studentId:$tag+'-a', fullName:'Ali'})-[:HAS_OFFER]->(oa:Offer {offer_id:$tag+'-oa', tenant_id:'T1', pricing_snapshot_json:$pricing})-[:ACCEPTED_VIA]->(:OfferAcceptance) \
                 CREATE (oa)-[:PAID_VIA]->(:Payment {payment_id:$tag+'-pay', status:'awaiting_proof', created_at:datetime()}) \
                 CREATE (l)-[:HAS_STUDENT]->(:Student {studentId:$tag+'-b', fullName:'Bima'})-[:HAS_OFFER]->(:Offer {offer_id:$tag+'-ob', tenant_id:'T1', pricing_snapshot_json:$pricing}) \
                 CREATE (l)-[:HAS_STUDENT]->(:Student {studentId:$tag+'-c', fullName:'Citra'})-[:HAS_OFFER]->(oc:Offer {offer_id:$tag+'-oc', tenant_id:'T1', payment_status:'paid', pricing_snapshot_json:$pricing})-[:ACCEPTED_VIA]->(:OfferAcceptance)",
            )
            .param("tag", tag.clone())
            .param("pricing", pricing),
        )
        .await
        .unwrap();

    let offers = super::payment_service::list_payable_offers(&graph, "T1", &format!("{tag}-lead"))
        .await
        .unwrap();
    assert_eq!(offers.len(), 1, "only the accepted, unpaid offer");
    assert_eq!(offers[0].offer_id, format!("{tag}-oa"));
    assert_eq!(offers[0].student_name, "Ali");
    assert_eq!(offers[0].amount_due_now, 15_000_000);
    assert_eq!(offers[0].payment_status.as_deref(), Some("awaiting_proof"));
    assert!(
        super::payment_service::list_payable_offers(&graph, "OTHER", &format!("{tag}-lead"))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        crate::repositories::payment_repository::find_offer_lead_id(
            &graph,
            &format!("{tag}-ob"),
            "T1"
        )
        .await
        .unwrap(),
        Some(format!("{tag}-lead"))
    );
    assert_eq!(
        crate::repositories::payment_repository::find_offer_lead_id(
            &graph,
            &format!("{tag}-ob"),
            "OTHER"
        )
        .await
        .unwrap(),
        None
    );

    graph
        .run(
            query(
                "MATCH (n) WHERE any(k IN ['lead_id','studentId','offer_id','payment_id'] WHERE toString(n[k]) STARTS WITH $tag) \
                 OPTIONAL MATCH (n)-[:ACCEPTED_VIA]->(acc) DETACH DELETE n, acc",
            )
            .param("tag", tag),
        )
        .await
        .unwrap();
}
