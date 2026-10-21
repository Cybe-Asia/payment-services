//! "Online Payment" for school invoices through DOKU Checkout (ADR 0014 §7).
//! The parent never sees the provider name; this module is the only place
//! that ties a `SchoolInvoice` to a DOKU order.
use crate::{
    clients::doku::{CreateCheckoutRequest, DokuWebhook},
    school_invoice::{
        auth,
        model::Invoice,
        repository::{self as invoices, Error},
    },
    AppState,
};
use neo4rs::{query, Graph, Row};
use serde::Serialize;

pub const PREFIX: &str = "SINV-";
const DUE_MINUTES: i64 = 60;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkout {
    pub checkout_url: String,
    pub expires_at: String,
}

fn return_url(invoice: &str) -> Option<String> {
    let base = std::env::var("SCHOOL_BILLING_RETURN_URL").ok()?;
    if !base.starts_with("https://") && !base.starts_with("http://localhost") {
        return None;
    }
    // The Parent portal keeps its screen in the fragment (`/parent#tab=payment`);
    // a query change would remount it, so the invoice joins the fragment.
    let sep = if base.contains('#') {
        if base.ends_with('#') {
            ""
        } else {
            "&"
        }
    } else if base.contains('?') {
        "&"
    } else {
        "?"
    };
    Some(format!("{base}{sep}paidInvoice={invoice}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn return_address_keeps_the_parent_fragment() {
        for (base, want) in [
            (
                "https://p.test/parent#tab=payment",
                "https://p.test/parent#tab=payment&paidInvoice=SINV-1",
            ),
            (
                "https://p.test/parent?x=1",
                "https://p.test/parent?x=1&paidInvoice=SINV-1",
            ),
            (
                "https://p.test/parent",
                "https://p.test/parent?paidInvoice=SINV-1",
            ),
        ] {
            std::env::set_var("SCHOOL_BILLING_RETURN_URL", base);
            assert_eq!(super::return_url("SINV-1").as_deref(), Some(want));
        }
        std::env::set_var("SCHOOL_BILLING_RETURN_URL", "http://evil.test/");
        assert!(super::return_url("SINV-1").is_none());
        std::env::remove_var("SCHOOL_BILLING_RETURN_URL");
    }
}

/// Starts (or reuses a still-valid) checkout for an open invoice.
pub async fn start(
    state: &AppState,
    graph: &Graph,
    a: &auth::Parent,
    id: &str,
) -> Result<Checkout, super::Failure> {
    let invoice = invoices::parent(graph, a, id)
        .await
        .map_err(super::owner_error)?;
    match invoice.status.as_str() {
        "pending" | "rejected" => {}
        "pending_verification" => {
            return Err(super::coded(
                axum::http::StatusCode::CONFLICT,
                "PAYMENT_IN_REVIEW",
            ))
        }
        _ => {
            return Err(super::coded(
                axum::http::StatusCode::CONFLICT,
                "REVISION_CONFLICT",
            ))
        }
    }
    let Some(return_url) = return_url(id).filter(|_| state.doku.configured()) else {
        return Err(super::coded(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "NOT_CONFIGURED",
        ));
    };
    let existing = graph
        .execute(
            query("MATCH (c:SchoolInvoiceCheckout {invoice_id:$id,status:'created'}) WHERE c.expires_at > datetime() + duration('PT10M') AND c.amount=$amount RETURN c.url AS url,toString(c.expires_at) AS expires ORDER BY c.created_at DESC LIMIT 1")
                .param("id", id.to_string())
                .param("amount", invoice.snapshot.amount),
        )
        .await
        .map_err(|_| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))?
        .next()
        .await
        .map_err(|_| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))?;
    if let Some(row) = existing {
        return checkout(&row);
    }
    let attempt = count(graph, id).await? + 1;
    let invoice_number = format!("{id}-{attempt}");
    let request_id = uuid::Uuid::new_v4().to_string();
    let created = state
        .doku
        .create_checkout(&CreateCheckoutRequest {
            request_id: &request_id,
            invoice_number: &invoice_number,
            amount: invoice.snapshot.amount,
            currency: "IDR",
            due_minutes: DUE_MINUTES,
            return_url: Some(&return_url),
        })
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "school invoice checkout was not created");
            super::coded(axum::http::StatusCode::BAD_GATEWAY, "BILLING_UNAVAILABLE")
        })?;
    let mut rows = graph
        .execute(
            query("MATCH (i:SchoolInvoice {id:$id}) WHERE i.status IN ['pending','rejected'] AND i.snapshot IS NOT NULL CREATE (c:SchoolInvoiceCheckout {invoice_number:$number,invoice_id:i.id,request_id:$request,amount:$amount,currency:'IDR',status:'created',url:$url,created_by:$payer,created_at:datetime(),expires_at:datetime() + duration({minutes:$minutes})}) CREATE (i)-[:PAID_ONLINE_VIA]->(c) RETURN c.url AS url,toString(c.expires_at) AS expires")
                .param("id", id.to_string())
                .param("number", invoice_number)
                .param("request", request_id)
                .param("amount", invoice.snapshot.amount)
                .param("url", created.checkout_url)
                .param("payer", a.subject.clone())
                .param("minutes", DUE_MINUTES),
        )
        .await
        .map_err(|_| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))?;
    let row = rows
        .next()
        .await
        .map_err(|_| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))?
        .ok_or_else(|| super::coded(axum::http::StatusCode::CONFLICT, "REVISION_CONFLICT"))?;
    checkout(&row)
}
fn checkout(row: &Row) -> Result<Checkout, super::Failure> {
    let get = |k| {
        row.get::<String>(k)
            .ok_or_else(|| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))
    };
    Ok(Checkout {
        checkout_url: get("url")?,
        expires_at: get("expires")?,
    })
}
async fn count(graph: &Graph, id: &str) -> Result<i64, super::Failure> {
    graph
        .execute(
            query("MATCH (c:SchoolInvoiceCheckout {invoice_id:$id}) RETURN count(c) AS n")
                .param("id", id.to_string()),
        )
        .await
        .map_err(|_| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))?
        .next()
        .await
        .ok()
        .flatten()
        .and_then(|r| r.get::<i64>("n"))
        .ok_or_else(|| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))
}

/// Normalized provider status.
fn status(webhook: &DokuWebhook) -> &'static str {
    match webhook
        .transaction
        .as_ref()
        .map(|t| t.status.as_str())
        .or(webhook.order.status.as_deref())
        .unwrap_or("PENDING")
        .to_ascii_uppercase()
        .as_str()
    {
        "SUCCESS" | "PAID" | "SETTLED" => "paid",
        "EXPIRED" | "ORDER_EXPIRED" => "expired",
        "FAILED" => "failed",
        "CANCELLED" => "cancelled",
        _ => "pending",
    }
}
fn amount(webhook: &DokuWebhook) -> Option<i64> {
    webhook.order.amount.as_i64().or_else(|| {
        webhook
            .order
            .amount
            .as_f64()
            .filter(|v| v.is_finite() && v.fract() == 0.0 && *v >= 0.0 && *v <= i64::MAX as f64)
            .map(|v| v as i64)
    })
}

/// Applies a signed DOKU callback (or status read) to a school invoice.
/// Settles at most once; replays and late callbacks are no-ops.
pub async fn apply(graph: &Graph, webhook: &DokuWebhook) -> Result<bool, String> {
    let number = webhook.order.invoice_number.as_str();
    let mut rows = graph
        .execute(
            query("MATCH (c:SchoolInvoiceCheckout {invoice_number:$number}) RETURN c.invoice_id AS invoice,c.request_id AS request,c.amount AS amount LIMIT 2")
                .param("number", number.to_string()),
        )
        .await
        .map_err(|_| "school checkout lookup failed".to_string())?;
    let row = rows
        .next()
        .await
        .map_err(|_| "school checkout lookup failed".to_string())?
        .ok_or_else(|| "DOKU payment reference not found".to_string())?;
    let expected = row
        .get::<i64>("amount")
        .ok_or("school checkout is invalid")?;
    let request = row
        .get::<String>("request")
        .ok_or("school checkout is invalid")?;
    let invoice = row
        .get::<String>("invoice")
        .ok_or("school checkout is invalid")?;
    let original = webhook
        .transaction
        .as_ref()
        .and_then(|t| t.original_request_id.as_deref());
    if amount(webhook) != Some(expected)
        || webhook.order.currency != "IDR"
        || original != Some(request.as_str())
    {
        return Err("DOKU callback amount, currency, provider, or reference mismatch".into());
    }
    let state = status(webhook);
    if state != "paid" {
        graph
            .run(
                query("MATCH (c:SchoolInvoiceCheckout {invoice_number:$number}) WHERE c.status='created' AND $status IN ['expired','failed','cancelled'] SET c.status=$status,c.updated_at=datetime()")
                    .param("number", number.to_string())
                    .param("status", state),
            )
            .await
            .map_err(|_| "school checkout update failed".to_string())?;
        return Ok(false);
    }
    // Only a live invoice whose amount still matches settles; the unique
    // settlement constraint makes a concurrent second settlement fail.
    let q = query("MATCH (c:SchoolInvoiceCheckout {invoice_number:$number})<-[:PAID_ONLINE_VIA]-(i:SchoolInvoice {id:$invoice}) SET i._lock=coalesce(i._lock,0)+1 WITH c,i WHERE i.status IN ['pending','rejected','pending_verification'] AND NOT EXISTS { MATCH (:SchoolInvoiceSettlement {invoice_id:i.id}) } SET c.status='paid',c.paid_at=datetime(),i.status='paid',i.version=i.version+1,i.paid_at=toString(datetime()),i.receipt_ref=$receipt CREATE (:SchoolInvoiceSettlement {invoice_id:i.id,id:$settlement,method:'online',checkout:c.invoice_number,amount:$amount,currency:'IDR',verified_by:'system:online-payment',created_at:datetime()}) CREATE (:SchoolInvoiceAudit {id:$audit,invoice_id:i.id,actor_id:'system:online-payment',action:'settled',version:i.version,note:'online',created_at:datetime()}) RETURN i.id AS id")
        .param("number", number.to_string())
        .param("invoice", invoice)
        .param("amount", expected)
        .param("receipt", format!("SRCPT-{}", uuid::Uuid::new_v4()))
        .param("settlement", format!("SSET-{}", uuid::Uuid::new_v4()))
        .param("audit", uuid::Uuid::new_v4().to_string());
    match invoices::write(graph, q, chrono::Utc::now().timestamp() + 60, |_| Ok(())).await {
        Ok(()) => Ok(true),
        Err(Error::Conflict) => Ok(false),
        Err(_) => Err("school settlement failed".into()),
    }
}

/// Reads the provider's own status for the newest checkout of an invoice
/// (for when the parent returns before the callback arrives).
pub async fn refresh(
    state: &AppState,
    graph: &Graph,
    a: &auth::Parent,
    id: &str,
) -> Result<Invoice, super::Failure> {
    let invoice = invoices::parent(graph, a, id)
        .await
        .map_err(super::owner_error)?;
    if invoice.status == "paid" || !state.doku.configured() {
        return Ok(invoice);
    }
    let latest = graph
        .execute(
            query("MATCH (c:SchoolInvoiceCheckout {invoice_id:$id}) RETURN c.invoice_number AS number,c.request_id AS request ORDER BY c.created_at DESC LIMIT 1")
                .param("id", id.to_string()),
        )
        .await
        .map_err(|_| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))?
        .next()
        .await
        .map_err(|_| super::failure(axum::http::StatusCode::SERVICE_UNAVAILABLE))?;
    if let Some(row) = latest {
        let number: String = row.get("number").unwrap_or_default();
        let request: String = row.get("request").unwrap_or_default();
        match state.doku.check_status(&number, &request).await {
            Ok(webhook) if webhook.order.invoice_number == number => {
                if let Err(e) = apply(graph, &webhook).await {
                    tracing::warn!(error = %e, "school checkout status was not applied");
                }
            }
            Ok(_) => tracing::warn!("school checkout status named another order"),
            Err(e) => tracing::warn!(error = %e, "school checkout status was unavailable"),
        }
    }
    invoices::parent(graph, a, id)
        .await
        .map_err(super::owner_error)
}
