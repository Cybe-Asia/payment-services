use std::sync::Arc;

use chrono::{Duration, Utc};
use neo4rs::{Graph, Query};
use tracing::{info, warn};
use uuid::Uuid;

use crate::clients::xendit::{CreateInvoiceRequest as XenditInvoiceReq, XenditClient};
use crate::models::payment::Payment;
use crate::repositories::{fee_obligation_repository, fee_structure_repository, payment_repository};

#[derive(Debug)]
pub struct CreateInvoiceOutcome {
    pub payment_id: String,
    pub hosted_invoice_url: String,
    pub amount: i64,
    pub currency: String,
    pub expires_at: String,
}

pub struct PaymentContext<'a> {
    pub graph: Arc<Graph>,
    pub xendit: &'a XenditClient,
    pub tenant_id: &'a str,
    pub default_currency: &'a str,
    pub default_due_hours: i64,
}

pub async fn create_invoice(
    ctx: PaymentContext<'_>,
    admission_id: &str,
    payment_type: &str,
) -> Result<CreateInvoiceOutcome, String> {
    // 1) Look up the Lead (parent contact info + school selection).
    let lead = fetch_lead(&ctx.graph, admission_id).await?
        .ok_or_else(|| "Lead not found".to_string())?;

    // 2) Resolve school_id from the lead's target_school_preference (code).
    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        &ctx.graph,
        ctx.tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| format!("school not found for code {}", lead.target_school_preference))?;

    // 3) Look up active FeeStructure to get the *per-student* amount.
    let fs = fee_structure_repository::find_active(&ctx.graph, ctx.tenant_id, &school_id, payment_type)
        .await
        .map_err(|e| format!("fee structure lookup failed: {e}"))?
        .ok_or_else(|| format!("no active FeeStructure for {} / {}", school_id, payment_type))?;

    // 3b) Count students on this Lead. Fee is per-student, so 2 kids = 2×.
    //     Reject with a clear error if there are no students — otherwise we'd
    //     create a Rp 0 invoice which Xendit would reject anyway.
    let student_count = count_students_for_lead(&ctx.graph, admission_id).await?;
    if student_count == 0 {
        return Err("no students registered for this application — add at least one student before paying".to_string());
    }
    let total_amount = fs.amount * student_count;

    // 4) Create (or reuse pending) FeeObligation with the *scaled* total.
    let due_at = Utc::now() + Duration::hours(ctx.default_due_hours);
    let obligation_id = format!("FEEOBL-{}", Uuid::new_v4());
    let obligation = fee_obligation_repository::upsert_for_lead(
        &ctx.graph,
        ctx.tenant_id,
        admission_id,
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
        &invoice.expiry_date.clone().unwrap_or_else(|| due_at.to_rfc3339()),
        &obligation.fee_obligation_id,
        admission_id,
    )
    .await
    .map_err(|e| format!("payment persist failed: {e}"))?;

    // 7) Advance the Application lifecycle `submitted → payment_pending`
    //    (best-effort — the Application node is owned by admission-service
    //    but we share the neo4j, so a direct cypher is simpler than an
    //    HTTP hop. If the Application doesn't exist yet, the UPDATE just
    //    matches zero rows.)
    set_application_status_for_lead(&ctx.graph, admission_id, "submitted", "payment_pending").await;

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

/// Preview what this Lead would be charged without actually creating an
/// invoice. Powers the frontend's payment page breakdown:
/// "Rp 1.000.000 × 2 students = Rp 2.000.000".
pub async fn preview_invoice(
    graph: &Graph,
    tenant_id: &str,
    admission_id: &str,
    payment_type: &str,
) -> Result<InvoicePreview, String> {
    let lead = fetch_lead(graph, admission_id).await?
        .ok_or_else(|| "Lead not found".to_string())?;

    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        graph,
        tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| format!("school not found for code {}", lead.target_school_preference))?;

    let fs = fee_structure_repository::find_active(graph, tenant_id, &school_id, payment_type)
        .await
        .map_err(|e| format!("fee structure lookup failed: {e}"))?
        .ok_or_else(|| format!("no active FeeStructure for {} / {}", school_id, payment_type))?;

    let student_count = count_students_for_lead(graph, admission_id).await?;
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
         RETURN a.application_id AS id".to_string(),
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

async fn count_students_for_lead(graph: &Graph, admission_id: &str) -> Result<i64, String> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$id})-[:HAS_STUDENT]->(s:Student) RETURN count(s) AS n".to_string(),
    )
    .param("id", admission_id.to_string());
    let mut result = graph.execute(q).await.map_err(|e| format!("student count: {e}"))?;
    if let Some(row) = result.next().await.map_err(|e| format!("student count row: {e}"))? {
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
                        fresh.payment_method.as_deref().or(fresh.payment_channel.as_deref()),
                        fresh.payment_id.as_deref().or(Some(&fresh.id)),
                    )
                    .await
                    {
                        warn!(error=%e, "xendit refresh: mark_paid failed");
                    } else {
                        // Best-effort settle the linked FeeObligation.
                        let _ = settle_obligation_for_payment(graph, payment_id).await;
                        // Advance Application: any pre-paid status → application_fee_paid.
                        if let Some(lead_id) = current.lead_id.as_deref() {
                            set_application_status_for_lead(graph, lead_id, "submitted", "application_fee_paid").await;
                            set_application_status_for_lead(graph, lead_id, "payment_pending", "application_fee_paid").await;
                        }
                        info!(payment_id=%payment_id, "xendit refresh: marked paid");
                    }
                }
                "EXPIRED" => {
                    if let Err(e) = payment_repository::mark_status(graph, payment_id, "expired").await {
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
            // And advance the Application lifecycle for the parent Lead.
            if let Some(lead_id) = payment.lead_id.as_deref() {
                set_application_status_for_lead(graph, lead_id, "submitted", "application_fee_paid").await;
                set_application_status_for_lead(graph, lead_id, "payment_pending", "application_fee_paid").await;
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

async fn settle_obligation_for_payment(graph: &Graph, payment_id: &str) -> Result<(), String> {
    let q = Query::new(
        "MATCH (f:FeeObligation)-[:SETTLED_BY]->(p:Payment {payment_id:$pid}) \
         SET f.status = 'paid', f.paid_at = datetime() \
         RETURN f.fee_obligation_id AS id".to_string(),
    )
    .param("pid", payment_id.to_string());
    let mut res = graph.execute(q).await.map_err(|e| format!("settle failed: {e}"))?;
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
    parent_name: String,
    email: String,
    whatsapp: String,
    target_school_preference: String,
}

async fn fetch_lead(graph: &Graph, lead_id: &str) -> Result<Option<LeadSnapshot>, String> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$id}) \
         RETURN l.parent_name AS parent_name, l.email AS email, \
                coalesce(l.whatsapp, l.mobile, '') AS whatsapp, \
                l.target_school_preference AS school \
         LIMIT 1".to_string(),
    )
    .param("id", lead_id.to_string());
    let mut result = graph.execute(q).await.map_err(|e| format!("lead fetch: {e}"))?;
    if let Some(row) = result.next().await.map_err(|e| format!("lead fetch row: {e}"))? {
        Ok(Some(LeadSnapshot {
            parent_name: row.get("parent_name").unwrap_or_default(),
            email: row.get("email").unwrap_or_default(),
            whatsapp: row.get("whatsapp").unwrap_or_default(),
            target_school_preference: row.get("school").unwrap_or_default(),
        }))
    } else {
        Ok(None)
    }
}
