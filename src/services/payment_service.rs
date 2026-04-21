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

    // 3) Look up active FeeStructure to get the amount.
    let fs = fee_structure_repository::find_active(&ctx.graph, ctx.tenant_id, &school_id, payment_type)
        .await
        .map_err(|e| format!("fee structure lookup failed: {e}"))?
        .ok_or_else(|| format!("no active FeeStructure for {} / {}", school_id, payment_type))?;

    // 4) Create (or reuse pending) FeeObligation.
    let due_at = Utc::now() + Duration::hours(ctx.default_due_hours);
    let obligation_id = format!("FEEOBL-{}", Uuid::new_v4());
    let obligation = fee_obligation_repository::upsert_for_lead(
        &ctx.graph,
        ctx.tenant_id,
        admission_id,
        payment_type,
        fs.amount,
        &fs.currency,
        &due_at.to_rfc3339(),
        &obligation_id,
    )
    .await
    .map_err(|e| format!("fee obligation upsert failed: {e}"))?;

    let payment_id = format!("PAY-{}", Uuid::new_v4());

    // 5) Create Xendit invoice.
    let description = format!("{} — {}", pretty_payment_type(payment_type), fs.school_code);
    let xendit_req = XenditInvoiceReq {
        external_id: &payment_id,
        amount: fs.amount,
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
        fs.amount,
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

    info!(
        payment_id=%payment_id, admission_id=%admission_id, amount=fs.amount, currency=%fs.currency,
        "created pending payment + xendit invoice"
    );

    Ok(CreateInvoiceOutcome {
        payment_id,
        hosted_invoice_url: invoice.invoice_url,
        amount: fs.amount,
        currency: fs.currency,
        expires_at: invoice.expiry_date.unwrap_or_else(|| due_at.to_rfc3339()),
    })
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
