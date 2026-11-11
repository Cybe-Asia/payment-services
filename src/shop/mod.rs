//! School Shop (ADR 0027). Slice 1: the spending ledger beside Student Credit.
//! Balance of a child = paid `student_credit` bills of every payer
//! + the sum of `ShopLedgerEntry.amount` (purchase negative, refund positive).
use crate::school_invoice::{
    model::identifier,
    repository::{Error, Result},
};
use neo4rs::{query, Graph, Row};
use serde::Serialize;

/// Largest single ledger movement, the same ceiling as one top-up.
const MAX_AMOUNT: i64 = crate::school_billing::model::MAX_TOP_UP;

const TOP_UPS: &str = "OPTIONAL MATCH (i:SchoolInvoice {enrolled_student_id:w.enrolled_student_id,status:'paid'})-[:BILLED_AS]->(l:SchoolBillingLine {category:'student_credit'}) WHERE l.invoice_id=i.id RETURN coalesce(sum(l.amount),0) AS topups";
const MOVED: &str = "OPTIONAL MATCH (e:ShopLedgerEntry {enrolled_student_id:w.enrolled_student_id}) RETURN coalesce(sum(e.amount),0) AS moved";

fn unavailable(_: impl std::fmt::Debug) -> Error {
    Error::Unavailable
}
fn s(row: &Row, k: &str) -> Result<String> {
    row.get::<String>(k).ok_or(Error::Unavailable)
}
/// Read through `toString`: neo4rs 0.6 decodes -16..-1 as 240..255.
fn signed(row: &Row, k: &str) -> Result<i64> {
    s(row, k)?.parse().map_err(unavailable)
}

pub async fn init(graph: &Graph) -> Result<()> {
    for (name, label, prop) in [
        ("shop_wallet_student", "ShopWallet", "enrolled_student_id"),
        ("shop_ledger_entry_id", "ShopLedgerEntry", "id"),
        ("shop_ledger_entry_key", "ShopLedgerEntry", "key"),
    ] {
        graph
            .run(query(&format!(
                "CREATE CONSTRAINT {name} IF NOT EXISTS FOR (n:{label}) REQUIRE n.{prop} IS UNIQUE"
            )))
            .await
            .map_err(unavailable)?;
    }
    graph
        .run(query("CREATE INDEX shop_ledger_entry_student IF NOT EXISTS FOR (n:ShopLedgerEntry) ON (n.enrolled_student_id)"))
        .await
        .map_err(unavailable)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Purchase,
    Refund,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Purchase => "purchase",
            Kind::Refund => "refund",
        }
    }
}

/// One movement to record. `amount` is always positive; the kind gives the sign.
pub struct Movement<'a> {
    /// Idempotency key: the same key never moves money twice.
    pub key: &'a str,
    pub tenant_id: &'a str,
    pub school_id: &'a str,
    pub enrolled_student_id: &'a str,
    pub kind: Kind,
    pub amount: i64,
    /// What the money was for (order or ticket id).
    pub ref_id: &'a str,
    pub actor_id: &'a str,
    /// A refund names the purchase it returns; refunds of one purchase never exceed it.
    pub purchase_key: Option<&'a str>,
}

#[derive(Debug, PartialEq)]
pub enum Posting {
    /// Recorded now or by an earlier request with the same key.
    Done { balance: i64 },
    /// Nothing was recorded.
    Insufficient { balance: i64 },
}

/// Records one movement. The wallet node is write-locked first, so two debits
/// of one child never both read the old balance; the balance cannot go below 0.
pub async fn post(graph: &Graph, m: &Movement<'_>) -> Result<Posting> {
    if !(1..=MAX_AMOUNT).contains(&m.amount)
        || ![m.key, m.enrolled_student_id, m.ref_id, m.actor_id]
            .iter()
            .all(|v| identifier(v))
        || m.purchase_key.is_some_and(|k| !identifier(k))
        || (m.kind == Kind::Refund) != m.purchase_key.is_some()
    {
        return Err(Error::Denied);
    }
    let amount = match m.kind {
        Kind::Purchase => -m.amount,
        Kind::Refund => m.amount,
    };
    let q = query(&format!("MERGE (w:ShopWallet {{enrolled_student_id:$enrolled}}) SET w._lock=coalesce(w._lock,0)+1 WITH w OPTIONAL MATCH (x:ShopLedgerEntry {{key:$key}}) OPTIONAL MATCH (p:ShopLedgerEntry {{key:$purchase,kind:'purchase',enrolled_student_id:$enrolled}}) CALL {{ OPTIONAL MATCH (r:ShopLedgerEntry {{purchase_key:$purchase,kind:'refund'}}) RETURN coalesce(sum(r.amount),0) AS refunded }} CALL {{ WITH w {TOP_UPS} }} CALL {{ WITH w {MOVED} }} WITH x,topups+moved AS balance,($kind='purchase' OR (p IS NOT NULL AND refunded+$amount<=-p.amount)) AS bound FOREACH (_ IN CASE WHEN x IS NULL AND bound AND balance+$amount>=0 THEN [1] ELSE [] END | CREATE (:ShopLedgerEntry {{id:$id,key:$key,tenant_id:$tenant,school_id:$school,enrolled_student_id:$enrolled,kind:$kind,amount:$amount,ref_id:$ref,purchase_key:CASE WHEN $purchase='' THEN null ELSE $purchase END,actor_id:$actor,created_at:datetime()}})) RETURN x IS NOT NULL AS replay,bound,coalesce(x.enrolled_student_id,'') AS x_enrolled,toString(coalesce(x.amount,0)) AS x_amount,toString(balance) AS balance"))
        .param("enrolled", m.enrolled_student_id.to_string())
        .param("key", m.key.to_string())
        .param("id", format!("SLE-{}", uuid::Uuid::new_v4().simple()))
        .param("tenant", m.tenant_id.to_string())
        .param("school", m.school_id.to_string())
        .param("kind", m.kind.name())
        .param("amount", amount)
        .param("ref", m.ref_id.to_string())
        .param("actor", m.actor_id.to_string())
        .param("purchase", m.purchase_key.unwrap_or_default().to_string());
    let tx = graph.start_txn().await.map_err(unavailable)?;
    let result = async {
        let mut rows = tx.execute(q).await.map_err(unavailable)?;
        let row = rows
            .next()
            .await
            .map_err(unavailable)?
            .ok_or(Error::Unavailable)?;
        let balance = signed(&row, "balance")?;
        if row.get::<bool>("replay").ok_or(Error::Unavailable)? {
            // A reused key must describe the same movement.
            if s(&row, "x_enrolled")? != m.enrolled_student_id
                || signed(&row, "x_amount")? != amount
            {
                return Err(Error::Conflict);
            }
            return Ok((false, Posting::Done { balance }));
        }
        if !row.get::<bool>("bound").ok_or(Error::Unavailable)? {
            // No such purchase for this child, or its refunds would exceed it.
            return Err(Error::Conflict);
        }
        if balance + amount < 0 {
            return Ok((false, Posting::Insufficient { balance }));
        }
        Ok((
            true,
            Posting::Done {
                balance: balance + amount,
            },
        ))
    }
    .await;
    match result {
        Ok((true, posting)) => {
            tx.commit().await.map_err(unavailable)?;
            Ok(posting)
        }
        Ok((false, posting)) => {
            tx.rollback().await.map_err(unavailable)?;
            Ok(posting)
        }
        Err(error) => {
            tx.rollback().await.map_err(unavailable)?;
            Err(error)
        }
    }
}

/// The signed amount a key moved, or `None` when no movement was ever recorded under it. Lets the
/// Shop tell a debit that happened from one that never reached the ledger, without moving money.
pub async fn recorded(graph: &Graph, key: &str) -> Result<Option<i64>> {
    if !identifier(key) {
        return Err(Error::Denied);
    }
    let mut rows = graph
        .execute(
            query(
                "MATCH (e:ShopLedgerEntry {key:$key}) RETURN toString(e.amount) AS amount LIMIT 1",
            )
            .param("key", key.to_string()),
        )
        .await
        .map_err(unavailable)?;
    match rows.next().await.map_err(unavailable)? {
        Some(row) => Ok(Some(signed(&row, "amount")?)),
        None => Ok(None),
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub kind: String,
    /// Signed: a purchase is negative.
    pub amount: i64,
    pub ref_id: String,
    pub created_at: String,
}

/// The child's balance and ledger entries, newest first.
pub async fn wallet(graph: &Graph, enrolled: &str) -> Result<(i64, Vec<Entry>)> {
    let mut rows = graph
        .execute(
            query(&format!("WITH {{enrolled_student_id:$enrolled}} AS w CALL {{ WITH w {TOP_UPS} }} CALL {{ WITH w {MOVED} }} CALL {{ WITH w OPTIONAL MATCH (e:ShopLedgerEntry {{enrolled_student_id:w.enrolled_student_id}}) WITH e ORDER BY e.created_at DESC,e.id LIMIT 500 RETURN collect(CASE WHEN e IS NULL THEN null ELSE [e.id,e.kind,toString(e.amount),e.ref_id,toString(e.created_at)] END) AS entries }} RETURN toString(topups+moved) AS balance,entries"))
                .param("enrolled", enrolled.to_string()),
        )
        .await
        .map_err(unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(unavailable)?
        .ok_or(Error::Unavailable)?;
    let entries = row
        .get::<Vec<Vec<String>>>("entries")
        .ok_or(Error::Unavailable)?
        .into_iter()
        .map(|e| match <[String; 5]>::try_from(e) {
            Ok([id, kind, amount, ref_id, created_at]) => Ok(Entry {
                id,
                kind,
                amount: amount.parse().map_err(unavailable)?,
                ref_id,
                created_at,
            }),
            Err(_) => Err(Error::Unavailable),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((signed(&row, "balance")?, entries))
}

// ------------------------------------------------------------------ HTTP
// Called by shop-services only, never by a browser: guarded by a shared secret.

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post as http_post},
    Json, Router,
};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

type Failure = (StatusCode, Json<Value>);
fn coded(status: StatusCode, code: &str) -> Failure {
    (status, Json(json!({"error":{"code":code}})))
}
/// The caller must present `SHOP_INTERNAL_TOKEN`. Not configured: the routes answer 503.
fn authorised(s: &AppState, h: &HeaderMap) -> std::result::Result<(), Failure> {
    let expected = s
        .shop_internal_token
        .as_deref()
        .ok_or_else(|| coded(StatusCode::SERVICE_UNAVAILABLE, "SHOP_LEDGER_UNAVAILABLE"))?;
    let given = h
        .get("x-internal-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if bool::from(given.as_bytes().ct_eq(expected.as_bytes())) {
        Ok(())
    } else {
        Err(coded(StatusCode::UNAUTHORIZED, "UNAUTHORIZED"))
    }
}
fn failed(e: Error) -> Failure {
    match e {
        Error::Denied => coded(StatusCode::BAD_REQUEST, "INVALID_INPUT"),
        Error::Conflict => coded(StatusCode::CONFLICT, "KEY_REUSED"),
        _ => coded(StatusCode::SERVICE_UNAVAILABLE, "SHOP_LEDGER_UNAVAILABLE"),
    }
}
fn graph(s: &AppState) -> std::result::Result<&Graph, Failure> {
    s.graph
        .as_deref()
        .ok_or_else(|| coded(StatusCode::SERVICE_UNAVAILABLE, "SHOP_LEDGER_UNAVAILABLE"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MovementInput {
    key: String,
    tenant_id: String,
    school_id: String,
    enrolled_student_id: String,
    /// `purchase` or `refund`.
    kind: String,
    amount: i64,
    ref_id: String,
    actor_id: String,
    /// Required for a refund: the key of the purchase it returns.
    purchase_key: Option<String>,
}
async fn movement(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(b): Json<MovementInput>,
) -> std::result::Result<Json<Value>, Failure> {
    authorised(&s, &h)?;
    let kind = match b.kind.as_str() {
        "purchase" => Kind::Purchase,
        "refund" => Kind::Refund,
        _ => return Err(coded(StatusCode::BAD_REQUEST, "INVALID_INPUT")),
    };
    let posting = post(
        graph(&s)?,
        &Movement {
            key: &b.key,
            tenant_id: &b.tenant_id,
            school_id: &b.school_id,
            enrolled_student_id: &b.enrolled_student_id,
            kind,
            amount: b.amount,
            ref_id: &b.ref_id,
            actor_id: &b.actor_id,
            purchase_key: b.purchase_key.as_deref(),
        },
    )
    .await
    .map_err(failed)?;
    match posting {
        Posting::Done { balance } => Ok(Json(json!({"data":{"balance":balance}}))),
        Posting::Insufficient { balance } => Err((
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({"error":{"code":"INSUFFICIENT_BALANCE","balance":balance}})),
        )),
    }
}
async fn balance(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(enrolled): Path<String>,
) -> std::result::Result<Json<Value>, Failure> {
    authorised(&s, &h)?;
    if !identifier(&enrolled) {
        return Err(coded(StatusCode::BAD_REQUEST, "INVALID_INPUT"));
    }
    let (balance, _) = wallet(graph(&s)?, &enrolled).await.map_err(failed)?;
    Ok(Json(json!({"data":{"balance":balance}})))
}

async fn movement_by_key(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(key): Path<String>,
) -> std::result::Result<Json<Value>, Failure> {
    authorised(&s, &h)?;
    let amount = recorded(graph(&s)?, &key).await.map_err(failed)?;
    Ok(Json(
        json!({"data":{"recorded":amount.is_some(),"amount":amount}}),
    ))
}

pub fn router() -> Router<AppState> {
    const P: &str = "/api/v1/payments/internal/shop";
    Router::new()
        .route(&format!("{P}/movements"), http_post(movement))
        .route(&format!("{P}/movements/:key"), get(movement_by_key))
        .route(&format!("{P}/wallets/:enrolled"), get(balance))
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024))
}

#[cfg(test)]
mod tests;
