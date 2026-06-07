use std::sync::Arc;

use chrono::{Duration, Utc};
use neo4rs::{Graph, Query};
use tracing::{info, warn};
use uuid::Uuid;

use crate::clients::xendit::{CreateInvoiceRequest as XenditInvoiceReq, XenditClient};
use crate::models::payment::Payment;
use crate::models::payment_proof::PaymentProof;
use crate::repositories::payment_repository::{
    CreateProofInput, ManualBankDetails, PaymentReviewDetail, PaymentReviewRow,
};
use crate::repositories::payment_settings_repository::{
    PaymentSettings, PaymentSettingsSeed, UpdatePaymentSettings,
};
use crate::repositories::{
    fee_obligation_repository, fee_structure_repository, payment_repository,
    payment_settings_repository,
};

#[derive(Debug)]
pub struct CreateInvoiceOutcome {
    pub payment_id: String,
    pub hosted_invoice_url: String,
    pub amount: i64,
    pub currency: String,
    pub expires_at: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ManualPaymentOutcome {
    pub payment: Payment,
    pub settings: PaymentSettings,
}

#[derive(Debug, serde::Serialize)]
pub struct PaymentReviewList {
    pub rows: Vec<PaymentReviewRow>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewManualPaymentRequest {
    pub decision: String,
    pub verified_amount: Option<i64>,
    pub note: Option<String>,
}

pub struct PaymentContext<'a> {
    pub graph: Arc<Graph>,
    pub xendit: &'a XenditClient,
    pub tenant_id: &'a str,
    pub default_due_hours: i64,
    pub settings_seed: PaymentSettingsSeed,
}

pub async fn create_invoice(
    ctx: PaymentContext<'_>,
    admission_id: &str,
    payment_type: &str,
) -> Result<CreateInvoiceOutcome, String> {
    let settings = get_payment_settings(&ctx.graph, &ctx.settings_seed).await?;
    if !settings.xendit_enabled {
        return Err("xendit payment method is disabled".to_string());
    }

    // 1) Look up the Lead (parent contact info + school selection).
    let lead = fetch_lead(&ctx.graph, admission_id)
        .await?
        .ok_or_else(|| "Lead not found".to_string())?;

    // 2) Resolve school_id from the lead's target_school_preference (code).
    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        &ctx.graph,
        ctx.tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| {
        format!(
            "school not found for code {}",
            lead.target_school_preference
        )
    })?;

    // 3) Look up active FeeStructure to get the *per-student* amount.
    let fs =
        fee_structure_repository::find_active(&ctx.graph, ctx.tenant_id, &school_id, payment_type)
            .await
            .map_err(|e| format!("fee structure lookup failed: {e}"))?
            .ok_or_else(|| {
                format!(
                    "no active FeeStructure for {} / {}",
                    school_id, payment_type
                )
            })?;

    // 3b) Count students.
    //     application_fee: charged per-child for the *whole* application
    //                      (2 kids in one Lead → 2 × fee).
    //     enrolment_fee:   per-child (one offer → one invoice → one kid).
    //                      admissionId here is the Student id itself.
    //     Detect by prefix — STU- means we treat it as a single-student
    //     invoice; anything else is the Lead-wide application fee.
    let is_student_scoped = admission_id.starts_with("STU-") || payment_type == "enrolment_fee";
    let student_count = if is_student_scoped {
        1_i64
    } else {
        count_students_for_lead(&ctx.graph, admission_id).await?
    };
    if student_count == 0 {
        return Err(
            "no students registered for this application — add at least one student before paying"
                .to_string(),
        );
    }
    let total_amount = fs.amount * student_count;

    // 4) Create (or reuse pending) FeeObligation with the *scaled* total.
    let due_at = Utc::now() + Duration::hours(ctx.default_due_hours);
    let obligation_id = format!("FEEOBL-{}", Uuid::new_v4());
    let obligation = fee_obligation_repository::upsert_for_lead(
        &ctx.graph,
        ctx.tenant_id,
        &lead.lead_id,
        payment_type,
        total_amount,
        &fs.currency,
        &due_at.to_rfc3339(),
        &obligation_id,
    )
    .await
    .map_err(|e| format!("fee obligation upsert failed: {e}"))?;

    let payment_id = format!("PAY-{}", Uuid::new_v4());

    // 5) Create Xendit invoice with the scaled total.
    let description = format!(
        "{} — {} × {} student{}",
        pretty_payment_type(payment_type),
        fs.school_code,
        student_count,
        if student_count == 1 { "" } else { "s" },
    );
    let xendit_req = XenditInvoiceReq {
        external_id: &payment_id,
        amount: total_amount,
        currency: &fs.currency,
        description: &description,
        payer_email: &lead.email,
        customer_name: &lead.parent_name,
        customer_phone: &lead.whatsapp,
        invoice_duration_seconds: ctx.default_due_hours * 3600,
    };
    let invoice = ctx.xendit.create_invoice(&xendit_req).await?;

    // 6) Persist Payment node linked to Lead and FeeObligation.
    payment_repository::create_pending(
        &ctx.graph,
        &payment_id,
        ctx.tenant_id,
        payment_type,
        total_amount,
        &fs.currency,
        &invoice.id,
        &invoice.id,
        &invoice.invoice_url,
        &invoice
            .expiry_date
            .clone()
            .unwrap_or_else(|| due_at.to_rfc3339()),
        &obligation.fee_obligation_id,
        &lead.lead_id,
    )
    .await
    .map_err(|e| format!("payment persist failed: {e}"))?;

    // 7) Advance the Application lifecycle `submitted → payment_pending`
    //    (best-effort — the Application node is owned by admission-service
    //    but we share the neo4j, so a direct cypher is simpler than an
    //    HTTP hop. If the Application doesn't exist yet, the UPDATE just
    //    matches zero rows.)
    set_application_status_for_lead(&ctx.graph, &lead.lead_id, "submitted", "payment_pending")
        .await;

    info!(
        payment_id=%payment_id,
        admission_id=%admission_id,
        unit_amount=fs.amount,
        student_count=student_count,
        total_amount=total_amount,
        currency=%fs.currency,
        "created pending payment + xendit invoice"
    );

    Ok(CreateInvoiceOutcome {
        payment_id,
        hosted_invoice_url: invoice.invoice_url,
        amount: total_amount,
        currency: fs.currency,
        expires_at: invoice.expiry_date.unwrap_or_else(|| due_at.to_rfc3339()),
    })
}

pub async fn create_manual_payment(
    ctx: PaymentContext<'_>,
    admission_id: &str,
    payment_type: &str,
) -> Result<ManualPaymentOutcome, String> {
    let settings = get_payment_settings(&ctx.graph, &ctx.settings_seed).await?;
    if !settings.manual_transfer_enabled {
        return Err("manual transfer payment method is disabled".to_string());
    }

    let lead = fetch_lead(&ctx.graph, admission_id)
        .await?
        .ok_or_else(|| "Lead not found".to_string())?;

    if let Some(existing) =
        payment_repository::find_active_manual_for_lead(&ctx.graph, &lead.lead_id, payment_type)
            .await
            .map_err(|e| format!("manual payment lookup failed: {e}"))?
    {
        return Ok(ManualPaymentOutcome {
            payment: existing,
            settings,
        });
    }

    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        &ctx.graph,
        ctx.tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| {
        format!(
            "school not found for code {}",
            lead.target_school_preference
        )
    })?;

    let fs =
        fee_structure_repository::find_active(&ctx.graph, ctx.tenant_id, &school_id, payment_type)
            .await
            .map_err(|e| format!("fee structure lookup failed: {e}"))?
            .ok_or_else(|| {
                format!(
                    "no active FeeStructure for {} / {}",
                    school_id, payment_type
                )
            })?;

    let is_student_scoped = admission_id.starts_with("STU-") || payment_type == "enrolment_fee";
    let student_count = if is_student_scoped {
        1_i64
    } else {
        count_students_for_lead(&ctx.graph, &lead.lead_id).await?
    };
    if student_count == 0 {
        return Err(
            "no students registered for this application — add at least one student before paying"
                .to_string(),
        );
    }
    let total_amount = fs.amount * student_count;

    let due_at = Utc::now() + Duration::hours(ctx.default_due_hours);
    let obligation_id = format!("FEEOBL-{}", Uuid::new_v4());
    let obligation = fee_obligation_repository::upsert_for_lead(
        &ctx.graph,
        ctx.tenant_id,
        &lead.lead_id,
        payment_type,
        total_amount,
        &fs.currency,
        &due_at.to_rfc3339(),
        &obligation_id,
    )
    .await
    .map_err(|e| format!("fee obligation upsert failed: {e}"))?;

    let payment_id = format!("PAY-{}", Uuid::new_v4());
    let manual_reference = format!("TWSI-{}", &payment_id.trim_start_matches("PAY-")[..8]);
    let bank = ManualBankDetails {
        bank_name: settings.bank_name.clone(),
        account_name: settings.bank_account_name.clone(),
        account_number: settings.bank_account_number.clone(),
        instructions: settings.instructions.clone(),
    };

    payment_repository::create_manual_pending(
        &ctx.graph,
        &payment_id,
        ctx.tenant_id,
        payment_type,
        total_amount,
        &fs.currency,
        &due_at.to_rfc3339(),
        &obligation.fee_obligation_id,
        &lead.lead_id,
        &manual_reference,
        &bank,
    )
    .await
    .map_err(|e| format!("manual payment persist failed: {e}"))?;

    set_application_status_for_lead(&ctx.graph, &lead.lead_id, "submitted", "payment_pending")
        .await;

    let payment = payment_repository::find_by_id(&ctx.graph, &payment_id)
        .await
        .map_err(|e| format!("manual payment fetch failed: {e}"))?
        .ok_or_else(|| "manual payment created but not found".to_string())?;

    Ok(ManualPaymentOutcome { payment, settings })
}

/// Preview what this Lead would be charged without actually creating an
/// invoice. Powers the frontend's payment page breakdown:
/// "Rp 1.000.000 × 2 students = Rp 2.000.000".
pub async fn preview_invoice(
    graph: &Graph,
    tenant_id: &str,
    admission_id: &str,
    payment_type: &str,
) -> Result<InvoicePreview, String> {
    let lead = fetch_lead(graph, admission_id)
        .await?
        .ok_or_else(|| "Lead not found".to_string())?;

    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        graph,
        tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| {
        format!(
            "school not found for code {}",
            lead.target_school_preference
        )
    })?;

    let fs = fee_structure_repository::find_active(graph, tenant_id, &school_id, payment_type)
        .await
        .map_err(|e| format!("fee structure lookup failed: {e}"))?
        .ok_or_else(|| {
            format!(
                "no active FeeStructure for {} / {}",
                school_id, payment_type
            )
        })?;

    // Same per-student-scope rule as create_invoice: Student id or
    // enrolment_fee → always 1; Lead id + application_fee → count kids.
    let is_student_scoped = admission_id.starts_with("STU-") || payment_type == "enrolment_fee";
    let student_count = if is_student_scoped {
        1_i64
    } else {
        count_students_for_lead(graph, admission_id).await?
    };
    let total = fs.amount * student_count;

    Ok(InvoicePreview {
        school_code: fs.school_code,
        payment_type: payment_type.to_string(),
        unit_amount: fs.amount,
        currency: fs.currency,
        student_count,
        total,
    })
}

pub async fn get_payment_settings(
    graph: &Graph,
    seed: &PaymentSettingsSeed,
) -> Result<PaymentSettings, String> {
    payment_settings_repository::get_or_seed(graph, seed)
        .await
        .map_err(|e| format!("payment settings fetch failed: {e}"))
}

pub async fn update_payment_settings(
    graph: &Graph,
    seed: &PaymentSettingsSeed,
    payload: UpdatePaymentSettings,
    actor: &str,
) -> Result<PaymentSettings, String> {
    let current = get_payment_settings(graph, seed).await?;
    let merged = UpdatePaymentSettings {
        xendit_enabled: payload.xendit_enabled,
        manual_transfer_enabled: payload.manual_transfer_enabled,
        bank_name: Some(payload.bank_name.unwrap_or(current.bank_name)),
        bank_account_name: Some(
            payload
                .bank_account_name
                .unwrap_or(current.bank_account_name),
        ),
        bank_account_number: Some(
            payload
                .bank_account_number
                .unwrap_or(current.bank_account_number),
        ),
        instructions: Some(payload.instructions.unwrap_or(current.instructions)),
    };

    payment_settings_repository::update(graph, &seed.tenant_id, merged, actor)
        .await
        .map_err(|e| format!("payment settings update failed: {e}"))
}

pub async fn record_manual_proof(
    graph: &Graph,
    input: CreateProofInput<'_>,
) -> Result<PaymentProof, String> {
    payment_repository::create_payment_proof(graph, input)
        .await
        .map_err(|e| format!("payment proof persist failed: {e}"))
}

pub async fn list_manual_review_rows(
    graph: &Graph,
    status: &str,
    school: &str,
    search: &str,
    limit: i64,
    offset: i64,
) -> Result<PaymentReviewList, String> {
    let rows = payment_repository::list_review_rows(graph, status, school, search, limit, offset)
        .await
        .map_err(|e| format!("payment review queue failed: {e}"))?;
    let total = payment_repository::count_review_rows(graph, status, school, search)
        .await
        .map_err(|e| format!("payment review count failed: {e}"))?;
    Ok(PaymentReviewList {
        rows,
        total,
        limit,
        offset,
    })
}

pub async fn get_manual_review_detail(
    graph: &Graph,
    payment_id: &str,
) -> Result<Option<PaymentReviewDetail>, String> {
    payment_repository::find_review_detail(graph, payment_id)
        .await
        .map_err(|e| format!("payment review detail failed: {e}"))
}

pub async fn review_manual_payment(
    graph: &Graph,
    payment_id: &str,
    payload: ReviewManualPaymentRequest,
    actor: &str,
) -> Result<Payment, String> {
    let payment = payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("payment fetch failed: {e}"))?
        .ok_or_else(|| "Payment not found".to_string())?;

    if payment.payment_method.as_deref() != Some("manual_transfer") {
        return Err("payment is not a manual transfer".to_string());
    }
    if payment.status == "paid" {
        return Err("payment is already paid".to_string());
    }

    let due = payment.amount.max(0);
    let current_verified = payment.amount_verified.unwrap_or(0).max(0);
    let submitted = payment.amount_submitted.unwrap_or(0).max(current_verified);
    let note = payload.note.clone().unwrap_or_default();
    let decision = payload.decision.to_lowercase();

    let (payment_status, proof_status, verified, short, overpaid, rejection_reason, receipt_ref) =
        match decision.as_str() {
            "approve" => {
                let verified = payload
                    .verified_amount
                    .unwrap_or(submitted)
                    .max(current_verified);
                if verified < due {
                    return Err(
                        "verified amount is lower than amount due; use underpaid".to_string()
                    );
                }
                (
                    "paid",
                    "approved",
                    verified,
                    0,
                    (verified - due).max(0),
                    "",
                    Some(payment_id),
                )
            }
            "underpaid" => {
                let verified = payload
                    .verified_amount
                    .ok_or_else(|| "verifiedAmount is required for underpaid review".to_string())?;
                if verified <= 0 {
                    return Err("verifiedAmount must be greater than zero".to_string());
                }
                if verified >= due {
                    return Err("verifiedAmount covers the full amount; use approve".to_string());
                }
                (
                    "underpaid",
                    "approved",
                    verified,
                    due - verified,
                    0,
                    "",
                    None,
                )
            }
            "reject" => {
                if note.trim().is_empty() {
                    return Err("review note is required when rejecting proof".to_string());
                }
                let short = (due - current_verified).max(0);
                let status = if current_verified > 0 {
                    "underpaid"
                } else {
                    "proof_rejected"
                };
                (
                    status,
                    "rejected",
                    current_verified,
                    short,
                    0,
                    note.as_str(),
                    None,
                )
            }
            _ => return Err("decision must be approve, underpaid, or reject".to_string()),
        };

    payment_repository::review_manual_payment(
        graph,
        payment_id,
        payment_status,
        proof_status,
        verified,
        short,
        overpaid,
        if note.trim().is_empty() {
            None
        } else {
            Some(note.as_str())
        },
        if rejection_reason.is_empty() {
            None
        } else {
            Some(rejection_reason)
        },
        actor,
        receipt_ref,
    )
    .await
    .map_err(|e| format!("payment review persist failed: {e}"))?;

    let reviewed = payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("payment fetch after review failed: {e}"))?
        .ok_or_else(|| "Payment not found after review".to_string())?;

    if reviewed.status == "paid" {
        apply_paid_side_effects(graph, &reviewed).await?;
    }

    Ok(reviewed)
}

#[derive(Debug, serde::Serialize)]
pub struct InvoicePreview {
    #[serde(rename = "schoolCode")]
    pub school_code: String,
    #[serde(rename = "paymentType")]
    pub payment_type: String,
    /// Fee per student, as published by the school's FeeStructure.
    #[serde(rename = "unitAmount")]
    pub unit_amount: i64,
    pub currency: String,
    #[serde(rename = "studentCount")]
    pub student_count: i64,
    /// unit_amount × student_count. What the parent will actually be charged.
    pub total: i64,
}

/// Conditionally advance `Application.status` for the Application bound
/// to this Lead. Matches only if the current status is `from_status`
/// (idempotent — a double-fire from Xendit webhook + poll doesn't
/// re-transition). Best-effort: if there's no Application yet (first
/// invoice created before students were submitted, or non-standard
/// flow) the query just matches zero rows and we move on.
///
/// Owned by admission-service, but we do this via direct cypher since
/// the graph is shared and it avoids a synchronous HTTP dependency.
async fn set_application_status_for_lead(
    graph: &Graph,
    lead_id: &str,
    from_status: &str,
    to_status: &str,
) {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id})-[:CONVERTED_TO]->(a:Application) \
         WHERE a.status = $from \
         SET a.status = $to, a.updated_at = datetime() \
         RETURN a.application_id AS id"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("from", from_status.to_string())
    .param("to", to_status.to_string());

    match graph.execute(q).await {
        Ok(mut res) => {
            // Drain so the transaction commits. Don't care about the row.
            let _ = res.next().await;
            info!(lead_id=%lead_id, from=%from_status, to=%to_status, "application status advanced (best-effort)");
        }
        Err(e) => {
            warn!(lead_id=%lead_id, error=%e, "failed to advance application status; payment flow continues");
        }
    }
}

/// Cascade every ApplicantStudent under a Lead from `submitted` to
/// `test_pending` once the application fee is paid. Per spec §4.4,
/// passing the fee is what gates access to the entrance test.
///
/// The cypher is idempotent and no-op for students that are already
/// past `submitted` (e.g. an admin-advanced student, or a re-payment).
/// When an enrolment_fee Payment is confirmed paid, advance every
/// ApplicantStudent under the Lead that's in `offer_accepted` all the
/// way to `handed_to_sis`, creating the EnrolledStudent node and the
/// (ApplicantStudent)-[:ENROLLED_AS]->(EnrolledStudent) edge per
/// spec §2.2 (admissions + SIS linked, not overwritten).
///
/// student_id is `STU-<uuid>` and student_number is `{SCHOOL}{YEAR}-<4hex>`.
/// For MVP we compute both inline in cypher using randomUUID() +
/// current-year toString — good enough until SIS gets its own service
/// with a real sequencing store.
///
/// Idempotent via MERGE; if the kid is already enrolled the query
/// no-ops.
async fn cascade_students_on_enrolment_paid(graph: &Graph, lead_id: &str) {
    let q = Query::new(
        "MATCH (:Lead {lead_id: $lead_id})-[:HAS_STUDENT]->(s:Student) \
         WHERE coalesce(s.applicantStatus, '') = 'offer_accepted' \
         OPTIONAL MATCH (s)-[:HAS_OFFER]->(o:Offer) \
         WITH s, o, randomUUID() AS uid, toString(date().year) AS yyyy \
         MERGE (e:EnrolledStudent {applicant_student_id: s.studentId}) \
         ON CREATE SET e.student_id = 'STU-' + uid, \
                       e.student_number = coalesce(replace(o.target_school_id, 'SCH-', ''), 'DS') + yyyy + '-' + toUpper(substring(uid, 0, 4)), \
                       e.tenant_id = 'TENANT-001', \
                       e.school_id = coalesce(o.target_school_id, ''), \
                       e.year_group = coalesce(o.target_year_group, ''), \
                       e.status = 'active', \
                       e.enrolment_date = toString(date()), \
                       e.created_at = datetime(), e.updated_at = datetime() \
         ON MATCH SET e.updated_at = datetime() \
         MERGE (s)-[:ENROLLED_AS]->(e) \
         SET s.applicantStatus = 'handed_to_sis', s.updatedAt = datetime() \
         RETURN count(s) AS enrolled_count".to_string(),
    )
    .param("lead_id", lead_id.to_string());
    match graph.execute(q).await {
        Ok(mut res) => {
            let _ = res.next().await;
            info!(lead_id=%lead_id, "students cascaded → handed_to_sis (EnrolledStudent created)");
        }
        Err(e) => {
            warn!(lead_id=%lead_id, error=%e, "failed to enrol students on enrolment_paid");
        }
    }
}

async fn cascade_students_to_test_pending(graph: &Graph, lead_id: &str) {
    let q = Query::new(
        "MATCH (:Lead {lead_id:$lead_id})-[:HAS_STUDENT]->(s:Student) \
         WHERE coalesce(s.applicantStatus, 'submitted') = 'submitted' \
         SET s.applicantStatus = 'test_pending', s.updatedAt = datetime()"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string());
    if let Err(e) = graph.run(q).await {
        warn!(lead_id=%lead_id, error=%e, "failed to cascade students to test_pending");
    } else {
        info!(lead_id=%lead_id, "students cascaded → test_pending");
    }
}

async fn count_students_for_lead(graph: &Graph, admission_id: &str) -> Result<i64, String> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$id})-[:HAS_STUDENT]->(s:Student) RETURN count(s) AS n".to_string(),
    )
    .param("id", admission_id.to_string());
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("student count: {e}"))?;
    if let Some(row) = result
        .next()
        .await
        .map_err(|e| format!("student count row: {e}"))?
    {
        Ok(row.get::<i64>("n").unwrap_or(0))
    } else {
        Ok(0)
    }
}

pub async fn fetch_payment(graph: &Graph, payment_id: &str) -> Result<Option<Payment>, String> {
    payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("payment fetch failed: {e}"))
}

/// Refresh a pending payment by asking Xendit for the authoritative status.
///
/// Why: In dev/test/staging our webhook URL isn't publicly reachable, so
/// Xendit can't call us when the invoice is paid. The frontend polls this
/// endpoint instead; on each poll we ask Xendit for the truth and persist.
///
/// No-op when the local status is already terminal (paid/expired/failed) or
/// when we have no Xendit invoice id to query.
pub async fn fetch_payment_refreshed(
    graph: &Graph,
    xendit: &XenditClient,
    payment_id: &str,
) -> Result<Option<Payment>, String> {
    let Some(current) = fetch_payment(graph, payment_id).await? else {
        return Ok(None);
    };

    if current.status != "pending" {
        return Ok(Some(current));
    }
    let Some(invoice_id) = current.invoice_ref.clone() else {
        return Ok(Some(current));
    };
    if invoice_id.is_empty() {
        return Ok(Some(current));
    }

    match xendit.get_invoice(&invoice_id).await {
        Ok(fresh) => {
            let upstream = fresh.status.to_uppercase();
            match upstream.as_str() {
                "PAID" | "SETTLED" => {
                    if let Err(e) = payment_repository::mark_paid(
                        graph,
                        payment_id,
                        fresh
                            .payment_method
                            .as_deref()
                            .or(fresh.payment_channel.as_deref()),
                        fresh.payment_id.as_deref().or(Some(&fresh.id)),
                    )
                    .await
                    {
                        warn!(error=%e, "xendit refresh: mark_paid failed");
                    } else {
                        // Best-effort settle the linked FeeObligation.
                        let _ = settle_obligation_for_payment(graph, payment_id).await;

                        // Payment-type-specific cascades. application_fee
                        // moves us into the test phase; enrolment_fee
                        // closes the admissions funnel and hands the
                        // applicant to SIS.
                        if let Some(lead_id) = current.lead_id.as_deref() {
                            match current.payment_type.as_str() {
                                "application_fee" => {
                                    set_application_status_for_lead(
                                        graph,
                                        lead_id,
                                        "submitted",
                                        "application_fee_paid",
                                    )
                                    .await;
                                    set_application_status_for_lead(
                                        graph,
                                        lead_id,
                                        "payment_pending",
                                        "application_fee_paid",
                                    )
                                    .await;
                                    cascade_students_to_test_pending(graph, lead_id).await;
                                }
                                "enrolment_fee" => {
                                    set_application_status_for_lead(
                                        graph,
                                        lead_id,
                                        "offer_stage",
                                        "completed",
                                    )
                                    .await;
                                    cascade_students_on_enrolment_paid(graph, lead_id).await;
                                }
                                _ => {
                                    // term_fee, capital_levy etc. — no
                                    // admissions-funnel side-effect.
                                }
                            }
                        }
                        info!(payment_id=%payment_id, "xendit refresh: marked paid");
                    }
                }
                "EXPIRED" => {
                    if let Err(e) =
                        payment_repository::mark_status(graph, payment_id, "expired").await
                    {
                        warn!(error=%e, "xendit refresh: mark_status expired failed");
                    } else {
                        info!(payment_id=%payment_id, "xendit refresh: marked expired");
                    }
                }
                "PENDING" => { /* no change */ }
                other => {
                    warn!(status=%other, payment_id=%payment_id, "xendit refresh: unknown status");
                }
            }
        }
        Err(e) => {
            // Don't fail the whole request; return the stale local view.
            warn!(error=%e, payment_id=%payment_id, "xendit refresh failed, returning local state");
        }
    }

    fetch_payment(graph, payment_id).await
}

pub async fn handle_webhook(
    graph: &Graph,
    payment_id: &str,
    gateway_status: &str,
    payment_method: Option<&str>,
    receipt_ref: Option<&str>,
) -> Result<(), String> {
    // Xendit statuses: PAID / EXPIRED / PENDING / SETTLED
    let normalized = gateway_status.to_uppercase();
    let payment = payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("find failed: {e}"))?
        .ok_or_else(|| format!("payment {payment_id} not found"))?;

    match normalized.as_str() {
        "PAID" | "SETTLED" => {
            payment_repository::mark_paid(graph, payment_id, payment_method, receipt_ref)
                .await
                .map_err(|e| format!("mark_paid failed: {e}"))?;
            // Also settle any FeeObligation linked via SETTLED_BY relationship
            settle_obligation_for_payment(graph, payment_id).await?;
            // And advance the Application lifecycle + per-child
            // ApplicantStudent status based on which fee was paid.
            if let Some(lead_id) = payment.lead_id.as_deref() {
                match payment.payment_type.as_str() {
                    "application_fee" => {
                        set_application_status_for_lead(
                            graph,
                            lead_id,
                            "submitted",
                            "application_fee_paid",
                        )
                        .await;
                        set_application_status_for_lead(
                            graph,
                            lead_id,
                            "payment_pending",
                            "application_fee_paid",
                        )
                        .await;
                        cascade_students_to_test_pending(graph, lead_id).await;
                    }
                    "enrolment_fee" => {
                        set_application_status_for_lead(graph, lead_id, "offer_stage", "completed")
                            .await;
                        cascade_students_on_enrolment_paid(graph, lead_id).await;
                    }
                    _ => {}
                }
            }
        }
        "EXPIRED" => {
            payment_repository::mark_status(graph, payment_id, "expired")
                .await
                .map_err(|e| format!("mark_status failed: {e}"))?;
        }
        "PENDING" => {
            // no-op; already pending
        }
        other => {
            warn!(status=%other, payment_id=%payment_id, "unknown webhook status");
        }
    }
    let _ = payment;
    Ok(())
}

async fn apply_paid_side_effects(graph: &Graph, payment: &Payment) -> Result<(), String> {
    settle_obligation_for_payment(graph, &payment.payment_id).await?;
    if let Some(lead_id) = payment.lead_id.as_deref() {
        match payment.payment_type.as_str() {
            "application_fee" => {
                set_application_status_for_lead(
                    graph,
                    lead_id,
                    "submitted",
                    "application_fee_paid",
                )
                .await;
                set_application_status_for_lead(
                    graph,
                    lead_id,
                    "payment_pending",
                    "application_fee_paid",
                )
                .await;
                cascade_students_to_test_pending(graph, lead_id).await;
            }
            "enrolment_fee" => {
                set_application_status_for_lead(graph, lead_id, "offer_stage", "completed").await;
                cascade_students_on_enrolment_paid(graph, lead_id).await;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn settle_obligation_for_payment(graph: &Graph, payment_id: &str) -> Result<(), String> {
    let q = Query::new(
        "MATCH (f:FeeObligation)-[:SETTLED_BY]->(p:Payment {payment_id:$pid}) \
         SET f.status = 'paid', f.paid_at = datetime() \
         RETURN f.fee_obligation_id AS id"
            .to_string(),
    )
    .param("pid", payment_id.to_string());
    let mut res = graph
        .execute(q)
        .await
        .map_err(|e| format!("settle failed: {e}"))?;
    let _ = res.next().await;
    Ok(())
}

fn pretty_payment_type(ty: &str) -> &str {
    match ty {
        "application_fee" => "Registration fee",
        "enrolment_fee" => "Enrolment fee",
        "capital_levy" => "Capital levy",
        "term_fee" => "Term fee",
        other => other,
    }
}

// ---- internal helpers ----

struct LeadSnapshot {
    lead_id: String,
    parent_name: String,
    email: String,
    whatsapp: String,
    target_school_preference: String,
}

/// Resolve an `admissionId` to the owning Lead. Accepts either a
/// `LEAD-xxx` id directly, or a `Student.studentId` — in which case
/// we walk back through the `HAS_STUDENT` edge to the parent Lead.
/// This lets the enrolment_fee flow (where the offer is per-student)
/// reuse the same `/invoice` endpoint as the application_fee flow.
async fn fetch_lead(graph: &Graph, admission_id: &str) -> Result<Option<LeadSnapshot>, String> {
    // Try it as a Lead id first — the common case for application_fee.
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$id}) \
         RETURN l.lead_id AS lead_id, l.parent_name AS parent_name, l.email AS email, \
                coalesce(l.whatsapp, l.mobile, '') AS whatsapp, \
                l.target_school_preference AS school \
         LIMIT 1"
            .to_string(),
    )
    .param("id", admission_id.to_string());
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("lead fetch: {e}"))?;
    if let Some(row) = result
        .next()
        .await
        .map_err(|e| format!("lead fetch row: {e}"))?
    {
        return Ok(Some(LeadSnapshot {
            lead_id: row.get("lead_id").unwrap_or_default(),
            parent_name: row.get("parent_name").unwrap_or_default(),
            email: row.get("email").unwrap_or_default(),
            whatsapp: row.get("whatsapp").unwrap_or_default(),
            target_school_preference: row.get("school").unwrap_or_default(),
        }));
    }

    // Fall back: treat it as a Student id and walk back to the Lead.
    // Enrolment-fee flow passes the student id because the Offer is
    // per-kid, not per-application.
    let q = Query::new(
        "MATCH (l:Lead)-[:HAS_STUDENT]->(Student {studentId:$id}) \
         RETURN l.lead_id AS lead_id, l.parent_name AS parent_name, l.email AS email, \
                coalesce(l.whatsapp, l.mobile, '') AS whatsapp, \
                l.target_school_preference AS school \
         LIMIT 1"
            .to_string(),
    )
    .param("id", admission_id.to_string());
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("lead fetch by student: {e}"))?;
    if let Some(row) = result
        .next()
        .await
        .map_err(|e| format!("lead fetch by student row: {e}"))?
    {
        Ok(Some(LeadSnapshot {
            lead_id: row.get("lead_id").unwrap_or_default(),
            parent_name: row.get("parent_name").unwrap_or_default(),
            email: row.get("email").unwrap_or_default(),
            whatsapp: row.get("whatsapp").unwrap_or_default(),
            target_school_preference: row.get("school").unwrap_or_default(),
        }))
    } else {
        Ok(None)
    }
}
