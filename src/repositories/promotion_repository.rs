use chrono::{DateTime, Duration, NaiveDate, Utc};

/// Promo windows are business dates in school time. "Until 31 July" must
/// mean 31 July in Jakarta, not UTC — at 05:00 WIB on 1 August, UTC is
/// still 22:00 on 31 July and would honour an expired promo (and the
/// mirror error at month start). WIB has no DST, so a fixed +7 is exact.
pub fn jakarta_today() -> NaiveDate {
    (Utc::now() + Duration::hours(7)).date_naive()
}
use neo4rs::{Graph, Query};

const ELIGIBILITY_SIBLING: &str = "sibling";

/// `PromotionRuleSnapshot.source` values, recorded on the invoice as
/// `promotionSource`.
pub const SOURCE_LEAD_PROMOTION_CODE: &str = "lead_promotion_code";
pub const SOURCE_REFERENCE_CODE: &str = "reference_code";
pub const SOURCE_GLOBAL_LADDER: &str = "global_ladder";
/// The family's own discount code (`Lead.discount_code`).
pub const SOURCE_DISCOUNT_CODE: &str = "discount_code";

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

/// Every promotion that could apply to this payment right now. The caller
/// computes the actual rupiah discount per candidate (percent vs fixed needs
/// the gross amount) and keeps the single best one — promos never stack.
pub async fn find_candidates_for_lead(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Vec<PromotionRuleSnapshot>, neo4rs::Error> {
    let mut out = Vec::new();
    if let Some(explicit) = find_explicit_lead_promotion(graph, lead_id, payment_type).await? {
        out.push(explicit);
    }
    if let Some(reference) = find_reference_code_promotion(graph, lead_id, payment_type).await? {
        out.push(reference);
    }
    if let Some(discount) =
        find_discount_code_promotion(graph, lead_id, payment_type, Utc::now()).await?
    {
        out.push(discount);
    }
    out.extend(find_global_ladder_promotions(graph, lead_id, payment_type).await?);
    Ok(out)
}

/// Auto-apply ladder rows (e.g. the early-bird monthly discount): global
/// PromotionCodes flagged `auto_apply` whose window covers today. No
/// per-lead attachment — every payment in the window is a candidate.
async fn find_global_ladder_promotions(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Vec<PromotionRuleSnapshot>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}) \
         OPTIONAL MATCH (l)-[:HAS_STUDENT]->(s:Student) \
         WITH l, count(s) AS applicantCount \
         MATCH (p:PromotionCode) \
         WHERE coalesce(p.auto_apply, false) = true \
           AND p.status IN ['active', 'approved'] \
           AND p.payment_type_scope IN [$payment_type, 'both'] \
           AND coalesce(p.approved_by, '') <> '' \
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
                applicantCount"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    let mut out = Vec::new();
    while let Some(row) = result.next().await? {
        let intake_scope: Option<String> = row.get("intakeScope");
        let valid_from: Option<String> = row.get("validFrom");
        let valid_until: Option<String> = row.get("validUntil");
        let eligibility: Option<String> = row.get("eligibility");
        let intake: String = row.get("intake").unwrap_or_default();
        let applicant_count: i64 = row.get("applicantCount").unwrap_or_default();
        if !promotion_window_ok(
            intake_scope.as_deref(),
            valid_from.as_deref(),
            valid_until.as_deref(),
            eligibility.as_deref(),
            &intake,
            applicant_count,
            jakarta_today(),
        ) {
            continue;
        }
        out.push(PromotionRuleSnapshot {
            promotion_code: row.get("promotionCode").unwrap_or_default(),
            promotion_rule_id: row.get("promotionRuleId").unwrap_or_default(),
            source: SOURCE_GLOBAL_LADDER.to_string(),
            discount_type: row.get("discountType").unwrap_or_default(),
            discount_value: row.get::<i64>("discountValue").unwrap_or_default(),
            max_discount_amount: row.get("maxDiscountAmount"),
            min_net_amount: row.get("minNetAmount"),
        });
    }
    Ok(out)
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
         WITH p, l, count(s) AS applicantCount \
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
                applicantCount \
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
            jakarta_today(),
        ) {
            return Ok(None);
        }

        return Ok(Some(PromotionRuleSnapshot {
            promotion_code: row.get("promotionCode").unwrap_or_default(),
            promotion_rule_id: row.get("promotionRuleId").unwrap_or_default(),
            source: SOURCE_LEAD_PROMOTION_CODE.to_string(),
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
           AND coalesce(r.kind, 'marketing') = 'marketing' \
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
            source: SOURCE_REFERENCE_CODE.to_string(),
            discount_type: row.get("discountType").unwrap_or_default(),
            discount_value: row.get::<i64>("discountValue").unwrap_or_default(),
            max_discount_amount: row.get("maxDiscountAmount"),
            min_net_amount: row.get("minNetAmount"),
        }));
    }

    Ok(None)
}

/// The Lead's verified discount code (`Lead.discount_code`, stored by
/// admission-service at EOI), when it gives a discount on this charge now.
///
/// A discount code's rule is a `DiscountRule` behind `HAS_DISCOUNT_RULE`; a
/// marketing code typed into the discount field brings its `PromotionRule`.
/// The reference-code lookup above reads `HAS_PROMOTION_RULE` only, so a
/// discount rule is out of its reach by construction (and out of reach of
/// every image from before discount codes).
///
/// Eligibility is evaluated again here, each time an invoice is computed:
///   - `Lead.discount_code_status` is `verified` (not `pending`: a code is
///     pending until the family has verified a contact);
///   - the code is active and is not a Parent's referral code;
///   - its rule is active, approved, and for this payment type; a discount
///     rule with a fixed amount applies to the application fee only (one
///     invoice per family), never to a per-student charge;
///   - `now` is inside the rule's window `[valid_from, valid_until)`;
///   - the Lead holds a redemption of the code. admission-service creates
///     that record atomically against the rule's redemption limit, so a
///     limited code never discounts more families than it allows.
///
/// A discount code's rule applies only through here, inside its window and
/// limit.
async fn find_discount_code_promotion(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
    now: DateTime<Utc>,
) -> Result<Option<PromotionRuleSnapshot>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}) \
         WHERE coalesce(l.discount_code_status, '') = 'verified' \
           AND coalesce(l.discount_code, '') <> '' \
         MATCH (r:ReferenceCode {normalized_code: l.discount_code})-[:HAS_DISCOUNT_RULE|HAS_PROMOTION_RULE]->(p) \
         WHERE (p:DiscountRule OR p:PromotionRule) \
           AND r.status = 'active' \
           AND coalesce(r.kind, 'marketing') <> 'parent_referral' \
           AND p.status = 'active' \
           AND p.payment_type_scope = $payment_type \
           AND coalesce(p.approved_by, '') <> '' \
           AND NOT (p:DiscountRule AND p.discount_type = 'fixed_amount' \
                    AND $payment_type <> 'application_fee') \
           AND EXISTS { MATCH (r)-[:HAS_REDEMPTION]->(:DiscountRedemption {lead_id: l.lead_id}) } \
         RETURN r.normalized_code AS promotionCode, \
                p.promotion_rule_id AS promotionRuleId, \
                p.discount_type AS discountType, \
                p.discount_value AS discountValue, \
                p.max_discount_amount AS maxDiscountAmount, \
                p.min_net_amount AS minNetAmount, \
                toString(p.valid_from) AS validFrom, \
                toString(p.valid_until) AS validUntil \
         ORDER BY p.approved_at DESC \
         LIMIT 1"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        let valid_from: Option<String> = row.get("validFrom");
        let valid_until: Option<String> = row.get("validUntil");
        if !discount_window_ok(valid_from.as_deref(), valid_until.as_deref(), now) {
            return Ok(None);
        }
        return Ok(Some(PromotionRuleSnapshot {
            promotion_code: row.get("promotionCode").unwrap_or_default(),
            promotion_rule_id: row.get("promotionRuleId").unwrap_or_default(),
            source: SOURCE_DISCOUNT_CODE.to_string(),
            discount_type: row.get("discountType").unwrap_or_default(),
            discount_value: row.get::<i64>("discountValue").unwrap_or_default(),
            max_discount_amount: row.get("maxDiscountAmount"),
            min_net_amount: row.get("minNetAmount"),
        }));
    }

    Ok(None)
}

/// Whether `now` is inside a discount code's window: from `valid_from`
/// (inclusive) until `valid_until` (exclusive), both RFC 3339 instants and
/// both optional. Pure so it can be unit tested. A stored value that cannot
/// be read fails closed (no discount).
pub fn discount_window_ok(
    valid_from: Option<&str>,
    valid_until: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    let instant = |value: &str| {
        DateTime::parse_from_rfc3339(value.trim())
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc))
    };
    if let Some(from) = nonblank(valid_from) {
        match instant(from) {
            Some(from) if now >= from => {}
            _ => return false,
        }
    }
    if let Some(until) = nonblank(valid_until) {
        match instant(until) {
            Some(until) if now < until => {}
            _ => return false,
        }
    }
    true
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

    fn at(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    #[test]
    fn discount_code_without_a_window_always_applies() {
        assert!(discount_window_ok(None, None, at("2026-10-13T00:00:00Z")));
        assert!(discount_window_ok(
            Some(""),
            Some("  "),
            at("2026-10-13T00:00:00Z")
        ));
    }

    #[test]
    fn discount_code_applies_from_valid_from_inclusive_until_valid_until_exclusive() {
        let (from, until) = (Some("2026-11-01T00:00:00Z"), Some("2026-12-01T00:00:00Z"));
        // Not yet valid.
        assert!(!discount_window_ok(from, until, at("2026-10-31T23:59:59Z")));
        assert!(discount_window_ok(from, until, at("2026-11-01T00:00:00Z")));
        assert!(discount_window_ok(from, until, at("2026-11-30T23:59:59Z")));
        // Expired: the end instant itself is already outside.
        assert!(!discount_window_ok(from, until, at("2026-12-01T00:00:00Z")));
        assert!(!discount_window_ok(None, until, at("2027-01-01T00:00:00Z")));
        assert!(!discount_window_ok(from, None, at("2026-01-01T00:00:00Z")));
        // Instants, not calendar days: an offset is honoured, and so are
        // the fractional seconds Neo4j prints.
        assert!(discount_window_ok(
            Some("2026-11-01T00:00:00+07:00"),
            None,
            at("2026-10-31T17:00:00Z")
        ));
        assert!(!discount_window_ok(
            None,
            Some("2026-12-01T00:00:00.000000000Z"),
            at("2026-12-01T00:00:00Z")
        ));
    }

    #[test]
    fn discount_code_with_an_unreadable_window_fails_closed() {
        let now = at("2026-11-15T00:00:00Z");
        assert!(!discount_window_ok(Some("2026-11-01"), None, now));
        assert!(!discount_window_ok(None, Some("31-12-2026"), now));
        assert!(!discount_window_ok(
            Some("soon"),
            Some("2026-12-01T00:00:00Z"),
            now
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
