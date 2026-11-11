use super::*;
use neo4rs::{query, Graph};

async fn graph() -> Graph {
    let uri = std::env::var("SCHOOL_INVOICE_TEST_NEO4J_URI").expect("explicit test graph");
    assert!(uri.starts_with("bolt://127.0.0.1:"));
    let g = Graph::new(
        &uri,
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_USER").unwrap(),
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_PASSWORD").unwrap(),
    )
    .await
    .unwrap();
    // Tests run in parallel; creating the same constraint twice at once fails.
    static SCHEMA: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    SCHEMA
        .get_or_init(|| async { init(&g).await.unwrap() })
        .await;
    g
}
/// A child with one paid top-up and one unpaid one (which must not count).
async fn child(g: &Graph, paid: i64) -> String {
    let enrolled = format!("se-{}", uuid::Uuid::new_v4().simple());
    g.run(query("UNWIND [['paid',$paid],['pending',900000]] AS t CREATE (:SchoolInvoice {id:$enrolled+'-'+t[0],enrolled_student_id:$enrolled,status:t[0]})-[:BILLED_AS]->(:SchoolBillingLine {invoice_id:$enrolled+'-'+t[0],category:'student_credit',amount:t[1]})")
        .param("enrolled", enrolled.clone())
        .param("paid", paid))
        .await
        .unwrap();
    enrolled
}
fn movement<'a>(enrolled: &'a str, key: &'a str, kind: Kind, amount: i64) -> Movement<'a> {
    Movement {
        key,
        tenant_id: "t",
        school_id: "s",
        enrolled_student_id: enrolled,
        kind,
        amount,
        ref_id: "order-1",
        actor_id: "actor-1",
        purchase_key: None,
    }
}

#[tokio::test]
#[ignore = "requires explicitly owned disposable SCHOOL_INVOICE_TEST_NEO4J_URI"]
async fn ledger_moves_the_balance_once_and_never_below_zero() {
    let g = graph().await;
    let e = child(&g, 50_000).await;
    assert_eq!(wallet(&g, &e).await.unwrap().0, 50_000);
    // Keys are unique in the whole graph, so each run needs its own.
    let k = |name: &str| format!("{e}-{name}");
    let (k_buy, k_big, k_back, k_zero) = (k("buy"), k("big"), k("back"), k("zero"));

    let buy = movement(&e, &k_buy, Kind::Purchase, 23_000);
    assert_eq!(
        post(&g, &buy).await.unwrap(),
        Posting::Done { balance: 27_000 }
    );
    // Same key again: no second debit.
    assert_eq!(
        post(&g, &buy).await.unwrap(),
        Posting::Done { balance: 27_000 }
    );
    // Same key for another amount is refused.
    assert!(matches!(
        post(&g, &movement(&e, &k_buy, Kind::Purchase, 1)).await,
        Err(Error::Conflict)
    ));
    // More than the balance: nothing recorded.
    assert_eq!(
        post(&g, &movement(&e, &k_big, Kind::Purchase, 27_001))
            .await
            .unwrap(),
        Posting::Insufficient { balance: 27_000 }
    );
    // A refund returns a recorded purchase of the same child, and never more than it.
    let refund = |key, of, amount| Movement {
        purchase_key: Some(of),
        ..movement(&e, key, Kind::Refund, amount)
    };
    assert!(matches!(
        post(&g, &movement(&e, &k_back, Kind::Refund, 5_000)).await,
        Err(Error::Denied)
    ));
    assert!(matches!(
        post(&g, &refund(&k_back, &k_big, 5_000)).await,
        Err(Error::Conflict)
    ));
    assert_eq!(
        post(&g, &refund(&k_back, &k_buy, 5_000)).await.unwrap(),
        Posting::Done { balance: 32_000 }
    );
    assert_eq!(
        post(&g, &refund(&k_back, &k_buy, 5_000)).await.unwrap(),
        Posting::Done { balance: 32_000 }
    );
    let k_over = k("over");
    assert!(matches!(
        post(&g, &refund(&k_over, &k_buy, 18_001)).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        post(&g, &movement(&e, &k_zero, Kind::Purchase, 0)).await,
        Err(Error::Denied)
    ));
    // The Shop can ask whether a key ever moved money.
    assert_eq!(recorded(&g, &k_buy).await.unwrap(), Some(-23_000));
    assert_eq!(recorded(&g, &k_big).await.unwrap(), None);
    let (balance, entries) = wallet(&g, &e).await.unwrap();
    assert_eq!(balance, 32_000);
    assert_eq!(
        entries
            .iter()
            .map(|x| (x.kind.as_str(), x.amount))
            .collect::<Vec<_>>(),
        [("refund", 5_000), ("purchase", -23_000)]
    );
}

#[tokio::test]
#[ignore = "requires explicitly owned disposable SCHOOL_INVOICE_TEST_NEO4J_URI"]
async fn concurrent_debits_cannot_overdraw() {
    let g = std::sync::Arc::new(graph().await);
    let e = child(&g, 50_000).await;
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..8 {
        let (g, e) = (g.clone(), e.clone());
        tasks.spawn(async move {
            post(
                &g,
                &movement(&e, &format!("{e}-c-{i}"), Kind::Purchase, 20_000),
            )
            .await
        });
    }
    let done = tasks
        .join_all()
        .await
        .into_iter()
        .filter(|r| matches!(r, Ok(Posting::Done { .. })))
        .count();
    assert_eq!(done, 2, "50.000 pays for exactly two 20.000 debits");
    assert_eq!(wallet(&g, &e).await.unwrap().0, 10_000);
}
