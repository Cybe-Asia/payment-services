use chrono::{NaiveDate, Utc};
use neo4rs::{Graph, Query};

const ELIGIBILITY_SIBLING: &str = "sibling";

#[derive(Clone, Debug)]
pub struct PromotionRuleSnapshot {
    pub promotion_code: String,
    pub promotion_rule_id: String,
    pub source: String,
    pub discount_type: String,
    pub discount_value: i64,
    pub max_discount_amount: Option<i64>,
    pub min_net_amount: Option<i64>,
}

pub async fn find_active_for_lead(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Option<PromotionRuleSnapshot>, neo4rs::Error> {
    if let Some(explicit) = find_explicit_lead_promotion(graph, lead_id, payment_type).await? {
        return Ok(Some(explicit));
    }
    find_reference_code_promotion(graph, lead_id, payment_type).await
}

async fn find_explicit_lead_promotion(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Option<PromotionRuleSnapshot>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}) \
         WHERE coalesce(l.promotion_code, '') <> '' \
         MATCH (p:PromotionCode {normalized_code:l.promotion_code}) \
         WHERE p.status IN ['active', 'approved'] \
           AND p.payment_type_scope = $payment_type \
           AND coalesce(p.approved_by, '') <> '' \
         OPTIONAL MATCH (l)-[:HAS_STUDENT]->(s:Student) \
         RETURN p.normalized_code AS promotionCode, \
                p.promotion_code_id AS promotionRuleId, \
                p.discount_type AS discountType, \
                p.discount_value AS discountValue, \
                p.max_discount_amount AS maxDiscountAmount, \
                p.min_net_amount AS minNetAmount, \
                p.intake_scope AS intakeScope, \
                toString(p.valid_from) AS validFrom, \
                toString(p.valid_until) AS validUntil, \
                p.eligibility AS eligibility, \
                coalesce(l.n_label, '') AS intake, \
                count(s) AS applicantCount \
         ORDER BY p.approved_at DESC \
         LIMIT 1"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        let intake_scope: Option<String> = row.get("intakeScope");
        let valid_from: Option<String> = row.get("validFrom");
        let valid_until: Option<String> = row.get("validUntil");
        let eligibility: Option<String> = row.get("eligibility");
        let intake: String = row.get("intake").unwrap_or_default();
        let applicant_count: i64 = row.get("applicantCount").unwrap_or_default();

        // A promo attached earlier may since have expired, be out of its
        // intake, or no longer meet the sibling rule — never discount unless
        // it still applies at payment time.
        if !promotion_window_ok(
            intake_scope.as_deref(),
            valid_from.as_deref(),
            valid_until.as_deref(),
            eligibility.as_deref(),
            &intake,
            applicant_count,
            Utc::now().date_naive(),
        ) {
            return Ok(None);
        }

        return Ok(Some(PromotionRuleSnapshot {
            promotion_code: row.get("promotionCode").unwrap_or_default(),
            promotion_rule_id: row.get("promotionRuleId").unwrap_or_default(),
            source: "lead_promotion_code".to_string(),
            discount_type: row.get("discountType").unwrap_or_default(),
            discount_value: row.get::<i64>("discountValue").unwrap_or_default(),
            max_discount_amount: row.get("maxDiscountAmount"),
            min_net_amount: row.get("minNetAmount"),
        }));
    }

    Ok(None)
}

async fn find_reference_code_promotion(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Option<PromotionRuleSnapshot>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}) \
         WHERE coalesce(l.reference_code_status, '') = 'verified' \
         WITH coalesce(l.reference_code, l.referral_code, '') AS code \
         MATCH (r:ReferenceCode {normalized_code: code})-[:HAS_PROMOTION_RULE]->(p:PromotionRule) \
         WHERE r.status = 'active' \
           AND p.status = 'active' \
           AND p.payment_type_scope = $payment_type \
           AND coalesce(p.approved_by, '') <> '' \
         RETURN r.normalized_code AS promotionCode, \
                p.promotion_rule_id AS promotionRuleId, \
                p.discount_type AS discountType, \
                p.discount_value AS discountValue, \
                p.max_discount_amount AS maxDiscountAmount, \
                p.min_net_amount AS minNetAmount \
         ORDER BY p.approved_at DESC \
         LIMIT 1"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        return Ok(Some(PromotionRuleSnapshot {
            promotion_code: row.get("promotionCode").unwrap_or_default(),
            promotion_rule_id: row.get("promotionRuleId").unwrap_or_default(),
            source: "reference_code".to_string(),
            discount_type: row.get("discountType").unwrap_or_default(),
            discount_value: row.get::<i64>("discountValue").unwrap_or_default(),
            max_discount_amount: row.get("maxDiscountAmount"),
            min_net_amount: row.get("minNetAmount"),
        }));
    }

    Ok(None)
}

/// Whether a promotion still applies to a lead at payment time, honoring
/// the validity window, intake scope and sibling eligibility. Pure so it
/// can be unit tested. A malformed stored date fails closed (no discount).
pub fn promotion_window_ok(
    intake_scope: Option<&str>,
    valid_from: Option<&str>,
    valid_until: Option<&str>,
    eligibility: Option<&str>,
    lead_intake: &str,
    applicant_count: i64,
    today: NaiveDate,
) -> bool {
    if let Some(from) = nonblank(valid_from) {
        match parse_date(from) {
            Some(d) if today < d => return false,
            None => return false,
            _ => {}
        }
    }
    if let Some(until) = nonblank(valid_until) {
        match parse_date(until) {
            Some(d) if today > d => return false,
            None => return false,
            _ => {}
        }
    }
    if let Some(scope) = nonblank(intake_scope) {
        if !lead_intake.trim().eq_ignore_ascii_case(scope) {
            return false;
        }
    }
    if let Some(elig) = nonblank(eligibility) {
        if elig.eq_ignore_ascii_case(ELIGIBILITY_SIBLING) && applicant_count < 2 {
            return false;
        }
    }
    true
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    let part = value.split('T').next().unwrap_or(value);
    NaiveDate::parse_from_str(part.trim(), "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn open_ended_promotion_applies() {
        assert!(promotion_window_ok(
            None,
            None,
            None,
            None,
            "N1-2026",
            1,
            day("2026-06-29")
        ));
    }

    #[test]
    fn expired_does_not_apply_but_last_day_does() {
        assert!(!promotion_window_ok(
            None,
            None,
            Some("2026-06-01"),
            None,
            "",
            1,
            day("2026-06-29")
        ));
        assert!(promotion_window_ok(
            None,
            None,
            Some("2026-06-01"),
            None,
            "",
            1,
            day("2026-06-01")
        ));
    }

    #[test]
    fn not_yet_active_does_not_apply() {
        assert!(!promotion_window_ok(
            None,
            Some("2026-07-01"),
            None,
            None,
            "",
            1,
            day("2026-06-29")
        ));
    }

    #[test]
    fn intake_scope_must_match() {
        assert!(promotion_window_ok(
            Some("N1-2026"),
            None,
            None,
            None,
            "n1-2026",
            1,
            day("2026-06-29")
        ));
        assert!(!promotion_window_ok(
            Some("N1-2026"),
            None,
            None,
            None,
            "N2-2026",
            1,
            day("2026-06-29")
        ));
    }

    #[test]
    fn sibling_requires_two_children() {
        assert!(promotion_window_ok(
            None,
            None,
            None,
            Some("sibling"),
            "",
            2,
            day("2026-06-29")
        ));
        assert!(!promotion_window_ok(
            None,
            None,
            None,
            Some("sibling"),
            "",
            1,
            day("2026-06-29")
        ));
    }

    #[test]
    fn malformed_date_fails_closed() {
        assert!(!promotion_window_ok(
            None,
            None,
            Some("31-12-2026"),
            None,
            "",
            1,
            day("2026-06-29")
        ));
    }
}
