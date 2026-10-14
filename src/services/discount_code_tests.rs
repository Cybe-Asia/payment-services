//! The Lead's discount code as a promotion candidate, against a disposable
//! graph: eligibility is evaluated when the invoice is computed.
use crate::repositories::promotion_repository::find_candidates_for_lead;
use neo4rs::{query, Graph};

async fn graph() -> Graph {
    Graph::new(
        std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
        "neo4j",
        std::env::var("ADMISSIONS_TEST_NEO4J_PASSWORD")
            .unwrap_or_else(|_| "test".into())
            .as_str(),
    )
    .await
    .unwrap()
}

/// `(source, code, rule id, discount type, value)` of each candidate.
async fn candidates(
    graph: &Graph,
    lead: &str,
    payment_type: &str,
) -> Vec<(String, String, String, String, i64)> {
    let mut found: Vec<_> = find_candidates_for_lead(graph, lead, payment_type)
        .await
        .unwrap()
        .into_iter()
        .map(|rule| {
            (
                rule.source,
                rule.promotion_code,
                rule.promotion_rule_id,
                rule.discount_type,
                rule.discount_value,
            )
        })
        .collect();
    found.sort();
    found
}

/// Run `cypher` with the Lead `l`, the code `r`, its rule `p` and the
/// Lead's redemption `d` bound.
async fn change(graph: &Graph, lead: &str, code: &str, cypher: &str) {
    graph
        .run(
            query(&format!(
                "MATCH (l:Lead {{lead_id:$lead}}), \
                       (r:ReferenceCode {{normalized_code:$code}})-[:HAS_DISCOUNT_RULE]->(p) \
                 OPTIONAL MATCH (r)-[:HAS_REDEMPTION]->(d) {cypher}"
            ))
            .param("code", code)
            .param("lead", lead),
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
async fn a_verified_discount_code_is_a_candidate_only_while_it_is_in_force() {
    let graph = graph().await;
    let tag = format!("DC{}", &uuid::Uuid::new_v4().simple().to_string()[..10]).to_uppercase();
    let lead = format!("LEAD-{tag}");
    let code = format!("{tag}A");
    let marketing = format!("{tag}M");
    // A Lead with a verified marketing reference code (fixed 100.000 on the
    // application fee) and a verified discount code (10 %), holding its
    // redemption, as admission-service stores them.
    graph
        .run(
            query(
                "CREATE (l:Lead {lead_id:$lead, tenant_id:'T1', dc_test:$tag, \
                    reference_code:$marketing, reference_code_status:'verified', \
                    discount_code:$code, discount_code_status:'verified'}) \
                 CREATE (m:ReferenceCode {normalized_code:$marketing, code:$marketing, status:'active', dc_test:$tag}) \
                    -[:HAS_PROMOTION_RULE]->(:PromotionRule {promotion_rule_id:'PROMO-' + $marketing, status:'active', \
                        payment_type_scope:'application_fee', discount_type:'fixed_amount', discount_value:100000, \
                        approved_by:'approver', approved_at:datetime(), dc_test:$tag}) \
                 CREATE (r:ReferenceCode {normalized_code:$code, code:$code, status:'active', kind:'discount', dc_test:$tag}) \
                    -[:HAS_DISCOUNT_RULE]->(:DiscountRule {promotion_rule_id:'DISC-' + $code, status:'active', \
                        payment_type_scope:'application_fee', discount_type:'percent', discount_value:10, \
                        valid_from:datetime() - duration('P1D'), valid_until:datetime() + duration('P1D'), \
                        max_redemptions:1, approved_by:'approver', approved_at:datetime(), dc_test:$tag}) \
                 CREATE (r)-[:HAS_REDEMPTION]->(:DiscountRedemption {redemption_key:$code + '|' + $lead, \
                        lead_id:$lead, code:$code, redeemed_at:datetime(), dc_test:$tag})",
            )
            .param("lead", lead.as_str())
            .param("code", code.as_str())
            .param("marketing", marketing.as_str())
            .param("tag", tag.as_str()),
        )
        .await
        .unwrap();
    let reference = (
        "reference_code".to_string(),
        marketing.clone(),
        format!("PROMO-{marketing}"),
        "fixed_amount".to_string(),
        100_000,
    );
    let discount = (
        "discount_code".to_string(),
        code.clone(),
        format!("DISC-{code}"),
        "percent".to_string(),
        10,
    );
    // Both are candidates for the application fee; the service keeps the
    // larger reduction (unit-tested in `payment_service`).
    assert_eq!(
        candidates(&graph, &lead, "application_fee").await,
        vec![discount.clone(), reference.clone()]
    );
    // The rule is for one charge kind.
    assert!(candidates(&graph, &lead, "enrolment_fee").await.is_empty());

    // Each condition is evaluated when the invoice is computed. After every
    // change the reference-code promotion is still there, alone.
    for (broken, restored) in [
        // Expired.
        (
            "SET p.valid_until = datetime() - duration('PT1M')",
            "SET p.valid_until = datetime() + duration('P1D')",
        ),
        // Not yet valid.
        (
            "SET p.valid_from = datetime() + duration('PT1H')",
            "SET p.valid_from = datetime() - duration('P1D')",
        ),
        // Deactivated by staff (code and rule).
        (
            "SET r.status = 'inactive', p.status = 'inactive'",
            "SET r.status = 'active', p.status = 'active'",
        ),
        ("SET p.status = 'inactive'", "SET p.status = 'active'"),
        ("SET r.status = 'inactive'", "SET r.status = 'active'"),
        // Not approved.
        ("SET p.approved_by = ''", "SET p.approved_by = 'approver'"),
        ("REMOVE p.approved_by", "SET p.approved_by = 'approver'"),
        // The Lead's code is not verified: unverified, or still pending
        // because the family has not verified a contact yet.
        (
            "SET l.discount_code_status = 'unverified'",
            "SET l.discount_code_status = 'verified'",
        ),
        (
            "SET l.discount_code_status = 'pending'",
            "SET l.discount_code_status = 'verified'",
        ),
        // The Lead holds no redemption (the limit was reached before it).
        (
            "SET d.lead_id = 'LEAD-someone-else'",
            "SET d.lead_id = l.lead_id",
        ),
    ] {
        change(&graph, &lead, &code, broken).await;
        assert_eq!(
            candidates(&graph, &lead, "application_fee").await,
            vec![reference.clone()],
            "{broken}"
        );
        change(&graph, &lead, &code, restored).await;
        assert_eq!(
            candidates(&graph, &lead, "application_fee").await,
            vec![discount.clone(), reference.clone()],
            "{restored}"
        );
    }

    // A fixed amount is for the application fee only: on a per-student
    // charge a discount rule with a fixed amount is not a candidate.
    change(
        &graph,
        &lead,
        &code,
        "SET p.discount_type = 'fixed_amount', p.discount_value = 250000, p.payment_type_scope = 'enrolment_fee'",
    )
    .await;
    assert!(candidates(&graph, &lead, "enrolment_fee").await.is_empty());
    change(
        &graph,
        &lead,
        &code,
        "SET p.payment_type_scope = 'application_fee'",
    )
    .await;
    assert_eq!(
        candidates(&graph, &lead, "application_fee").await,
        vec![
            (
                "discount_code".to_string(),
                code.clone(),
                format!("DISC-{code}"),
                "fixed_amount".to_string(),
                250_000
            ),
            reference.clone()
        ]
    );
    change(
        &graph,
        &lead,
        &code,
        "SET p.discount_type = 'percent', p.discount_value = 10",
    )
    .await;

    // A marketing code that carries a promotion, typed into the discount
    // field and redeemed there: a candidate through the discount path, with
    // its own rule.
    graph
        .run(
            query(
                "MATCH (l:Lead {lead_id:$lead}), (m:ReferenceCode {normalized_code:$marketing}) \
                 SET l.reference_code = null, l.reference_code_status = null, l.discount_code = $marketing \
                 CREATE (m)-[:HAS_REDEMPTION]->(:DiscountRedemption {redemption_key:$marketing + '|' + $lead, \
                        lead_id:$lead, code:$marketing, redeemed_at:datetime(), dc_test:$tag})",
            )
            .param("lead", lead.as_str())
            .param("marketing", marketing.as_str())
            .param("tag", tag.as_str()),
        )
        .await
        .unwrap();
    assert_eq!(
        candidates(&graph, &lead, "application_fee").await,
        vec![(
            "discount_code".to_string(),
            marketing.clone(),
            format!("PROMO-{marketing}"),
            "fixed_amount".to_string(),
            100_000
        )]
    );

    // A discount code typed into the referral field is never applied as a
    // reference-code promotion: that would escape its window and limit. Its
    // rule is not even reachable from that lookup (`HAS_PROMOTION_RULE`),
    // which is what keeps an image from before discount codes from
    // applying it too.
    graph
        .run(
            query(
                "MATCH (l:Lead {lead_id:$lead}) \
                 SET l.reference_code = $code, l.discount_code = null, l.discount_code_status = null",
            )
            .param("lead", lead.as_str())
            .param("code", code.as_str()),
        )
        .await
        .unwrap();
    assert!(candidates(&graph, &lead, "application_fee")
        .await
        .is_empty());
    let mut reachable = graph
        .execute(
            query(
                "MATCH (r:ReferenceCode {normalized_code:$code}) \
                 RETURN size([(r)-[:HAS_PROMOTION_RULE]->(p:PromotionRule) | p]) AS n",
            )
            .param("code", code.as_str()),
        )
        .await
        .unwrap();
    assert_eq!(
        reachable.next().await.unwrap().unwrap().get::<i64>("n"),
        Some(0)
    );

    // A Parent's referral code never discounts, even with a rule on it.
    graph
        .run(
            query(
                "MATCH (l:Lead {lead_id:$lead}), (r:ReferenceCode {normalized_code:$code}) \
                 SET r.kind = 'parent_referral', l.reference_code = null, \
                     l.discount_code = $code, l.discount_code_status = 'verified'",
            )
            .param("lead", lead.as_str())
            .param("code", code.as_str()),
        )
        .await
        .unwrap();
    assert!(candidates(&graph, &lead, "application_fee")
        .await
        .is_empty());

    graph
        .run(query("MATCH (n {dc_test:$tag}) DETACH DELETE n").param("tag", tag))
        .await
        .unwrap();
}

/// The invoice of a family with a discount code: computed with the larger
/// of its promotions, recorded on the Payment and the FeeObligation like
/// every other promotion, and left alone once it exists.
#[tokio::test]
#[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI; no providers or notifications"]
async fn the_invoice_records_the_discount_code_and_is_not_recalculated_afterwards() {
    use super::payment_service::{create_manual_payment, preview_invoice, PaymentContext};
    let graph = std::sync::Arc::new(graph().await);
    let key = uuid::Uuid::new_v4().simple().to_string();
    let tag = format!("DI{}", &key[..10]).to_uppercase();
    let tenant = format!("discount-invoice-test-{key}");
    let lead = format!("LEAD-{tag}");
    let school = format!("SCH-{tag}");
    let code = format!("{tag}A");
    let marketing = format!("{tag}M");
    // Two children, application fee Rp 1.333.333 each; a reference code
    // worth Rp 100.000 and a 15 % discount code.
    graph
        .run(
            query(
                "CREATE (l:Lead {lead_id:$lead, tenant_id:$tenant, parent_name:'Synthetic parent', \
                    email:'parent@example.invalid', target_school_preference:$school, dc_test:$tag, \
                    reference_code:$marketing, reference_code_status:'verified', \
                    discount_code:$code, discount_code_status:'verified'}) \
                 CREATE (l)-[:HAS_STUDENT]->(:Student {studentId:$tag + '-1', dc_test:$tag}) \
                 CREATE (l)-[:HAS_STUDENT]->(:Student {studentId:$tag + '-2', dc_test:$tag}) \
                 CREATE (:School {school_id:$school, school_code:$school, tenant_id:$tenant, dc_test:$tag}) \
                    -[:HAS_FEE_STRUCTURE]->(:FeeStructure {fee_structure_id:$tag, tenant_id:$tenant, \
                        school_id:$school, payment_type:'application_fee', amount:1333333, currency:'IDR', \
                        status:'active', effective_from:datetime(), dc_test:$tag}) \
                 CREATE (:ReferenceCode {normalized_code:$marketing, code:$marketing, status:'active', dc_test:$tag}) \
                    -[:HAS_PROMOTION_RULE]->(:PromotionRule {promotion_rule_id:'PROMO-' + $marketing, status:'active', \
                        payment_type_scope:'application_fee', discount_type:'fixed_amount', discount_value:100000, \
                        approved_by:'approver', approved_at:datetime(), dc_test:$tag}) \
                 CREATE (r:ReferenceCode {normalized_code:$code, code:$code, status:'active', kind:'discount', dc_test:$tag}) \
                    -[:HAS_DISCOUNT_RULE]->(:DiscountRule {promotion_rule_id:'DISC-' + $code, status:'active', \
                        payment_type_scope:'application_fee', discount_type:'percent', discount_value:15, \
                        valid_until:datetime() + duration('P1D'), \
                        approved_by:'approver', approved_at:datetime(), dc_test:$tag}) \
                 CREATE (r)-[:HAS_REDEMPTION]->(:DiscountRedemption {redemption_key:$code + '|' + $lead, \
                        lead_id:$lead, code:$code, redeemed_at:datetime(), dc_test:$tag})",
            )
            .param("lead", lead.as_str())
            .param("tenant", tenant.as_str())
            .param("school", school.as_str())
            .param("code", code.as_str())
            .param("marketing", marketing.as_str())
            .param("tag", tag.as_str()),
        )
        .await
        .unwrap();

    // Gross 2 × 1.333.333 = 2.666.666. 15 % = 399.999,9 → Rp 399.999
    // (rounded down), more than the reference code's Rp 100.000: the
    // discount code is applied, alone.
    let preview = preview_invoice(&graph, &tenant, &lead, "application_fee")
        .await
        .unwrap();
    assert_eq!(preview.student_count, 2);
    assert_eq!(preview.gross_amount, 2_666_666);
    assert_eq!(preview.discount_amount, 399_999);
    assert_eq!(preview.net_amount, 2_266_667);
    assert_eq!(preview.total, 2_266_667);
    assert_eq!(preview.promotion_code.as_deref(), Some(code.as_str()));
    assert_eq!(preview.promotion_rule_id, Some(format!("DISC-{code}")));
    let lines: Vec<(String, i64)> = preview
        .line_items
        .iter()
        .map(|item| (item.label.clone(), item.amount))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].1, 2_666_666);
    assert_eq!(lines[1], (format!("Discount code ({code})"), -399_999));

    let xendit = crate::clients::xendit::XenditClient::new("http://127.0.0.1:1", "", "", "");
    let context = || PaymentContext {
        graph: graph.clone(),
        xendit: &xendit,
        tenant_id: &tenant,
        default_due_hours: 24,
        settings_seed: crate::repositories::payment_settings_repository::PaymentSettingsSeed {
            tenant_id: tenant.clone(),
            bank_name: "SYNTHETIC BANK".into(),
            bank_account_name: "SYNTHETIC SCHOOL".into(),
            bank_account_number: "0000000000".into(),
            instructions: "Synthetic fixture, do not transfer".into(),
        },
    };
    let issued = create_manual_payment(context(), &lead, "application_fee", None)
        .await
        .unwrap()
        .payment;
    assert_eq!(issued.amount, 2_266_667);
    assert_eq!(issued.gross_amount, Some(2_666_666));
    assert_eq!(issued.discount_amount, Some(399_999));
    assert_eq!(issued.promotion_code.as_deref(), Some(code.as_str()));
    assert_eq!(issued.promotion_rule_id, Some(format!("DISC-{code}")));
    let snapshot: serde_json::Value =
        serde_json::from_str(issued.promotion_snapshot_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        snapshot,
        serde_json::json!({
            "promotionCode": code, "promotionRuleId": format!("DISC-{code}"),
            "promotionSource": "discount_code", "discountType": "percent", "discountValue": 15,
            "maxDiscountAmount": null, "minNetAmount": null
        })
    );
    // The obligation carries the same figures.
    let mut rows = graph
        .execute(
            query(
                "MATCH (f:FeeObligation)-[:SETTLED_BY]->(:Payment {payment_id:$id}) \
                 RETURN f.amount_due AS due, f.gross_amount AS gross, f.discount_amount AS discount, \
                        f.promotion_code AS code, f.promotion_rule_id AS rule",
            )
            .param("id", issued.payment_id.as_str()),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        (
            row.get::<i64>("due"),
            row.get::<i64>("gross"),
            row.get::<i64>("discount"),
            row.get::<String>("code"),
            row.get::<String>("rule")
        ),
        (
            Some(2_266_667),
            Some(2_666_666),
            Some(399_999),
            Some(code.clone()),
            Some(format!("DISC-{code}"))
        )
    );

    // The code ends. A new computation gives the reference-code promotion
    // instead; the invoice that was already issued keeps its amounts.
    change(
        &graph,
        &lead,
        &code,
        "SET p.valid_until = datetime() - duration('PT1M')",
    )
    .await;
    let later = preview_invoice(&graph, &tenant, &lead, "application_fee")
        .await
        .unwrap();
    assert_eq!(later.discount_amount, 100_000);
    assert_eq!(later.net_amount, 2_566_666);
    assert_eq!(later.promotion_code.as_deref(), Some(marketing.as_str()));
    let again = create_manual_payment(context(), &lead, "application_fee", None)
        .await
        .unwrap()
        .payment;
    assert_eq!(again.payment_id, issued.payment_id);
    assert_eq!(again.amount, 2_266_667);
    assert_eq!(again.discount_amount, Some(399_999));
    assert_eq!(again.promotion_code.as_deref(), Some(code.as_str()));

    graph
        .run(
            query(
                "MATCH (n) WHERE n.dc_test = $tag OR n.tenant_id = $tenant \
                 OPTIONAL MATCH (n)-[:MADE_PAYMENT]->(p:Payment) \
                 OPTIONAL MATCH (f:FeeObligation)-[:SETTLED_BY]->(p) DETACH DELETE f, p, n",
            )
            .param("tag", tag)
            .param("tenant", tenant),
        )
        .await
        .unwrap();
}
