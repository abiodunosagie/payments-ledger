//! End to end through the HTTP API, against a real Postgres. Each test gets a fresh database from
//! `#[sqlx::test]` (needs `DATABASE_URL`). Nothing touches the network or Paystack itself:
//! webhooks are signed locally with a test secret.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use payments_ledger::{AppState, app, idempotency, paystack};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const API_KEY: &str = "test-api-key-0123456789abcdef0123456789";
const SECRET: &str = "sk_test_ledger_secret";

struct Api {
    router: Router,
}

struct Reply {
    status: StatusCode,
    body: Value,
    replayed: bool,
}

impl Api {
    fn new(pool: PgPool) -> Self {
        Self {
            router: app(AppState::new(pool, API_KEY, SECRET)),
        }
    }

    async fn send(&self, req: Request<Body>) -> Reply {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let replayed = res.headers().get(idempotency::REPLAYED_HEADER).is_some();
        let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            body,
            replayed,
        }
    }

    async fn get(&self, path: &str) -> Reply {
        self.send(
            Request::get(path)
                .header("authorization", format!("Bearer {API_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post(&self, path: &str, key: &str, body: Value) -> Reply {
        self.send(
            Request::post(path)
                .header("authorization", format!("Bearer {API_KEY}"))
                .header("content-type", "application/json")
                .header(idempotency::HEADER, key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn webhook(&self, payload: &Value, secret: &str) -> Reply {
        let body = payload.to_string();
        self.send(
            Request::post("/webhooks/paystack")
                .header(
                    paystack::SIGNATURE_HEADER,
                    paystack::sign(secret.as_bytes(), body.as_bytes()),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }

    async fn account(&self, name: &str, currency: &str) -> Uuid {
        let r = self
            .post(
                "/accounts",
                &Uuid::new_v4().to_string(),
                json!({ "name": name, "currency": currency }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
        r.body["id"].as_str().unwrap().parse().unwrap()
    }

    async fn balance(&self, id: Uuid) -> i64 {
        self.get(&format!("/accounts/{id}")).await.body["balance"]
            .as_i64()
            .unwrap()
    }

    /// Opens a deposit and settles it with a signed `charge.success`, as Paystack would.
    async fn fund(&self, account: Uuid, amount: i64) -> String {
        let r = self
            .post(
                "/deposits",
                &Uuid::new_v4().to_string(),
                json!({ "account_id": account, "amount": amount }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
        let reference = r.body["reference"].as_str().unwrap().to_owned();
        let w = self
            .webhook(&charge(&reference, amount, "NGN"), SECRET)
            .await;
        assert_eq!(w.status, StatusCode::OK);
        reference
    }

    /// Takes the key by value so many transfers can be built in a loop and awaited together.
    async fn transfer(&self, key: impl AsRef<str>, from: Uuid, to: Uuid, amount: i64) -> Reply {
        self.post(
            "/transfers",
            key.as_ref(),
            json!({ "from_account": from, "to_account": to, "amount": amount }),
        )
        .await
    }
}

fn charge(reference: &str, amount: i64, currency: &str) -> Value {
    json!({
        "event": "charge.success",
        "data": { "id": 1, "reference": reference, "amount": amount, "currency": currency,
                  "status": "success", "channel": "card" }
    })
}

async fn outcomes(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select outcome from webhook_events order by id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The ledger's books: every cached balance equals the sum of its postings, every journal entry
/// sums to zero, and no customer balance is negative.
async fn assert_books_balance(pool: &PgPool) {
    let drift: i64 = sqlx::query_scalar(
        "select count(*) from accounts a
         where a.balance <> coalesce((select sum(amount) from postings p where p.account_id = a.id), 0)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(drift, 0, "a cached balance drifted from its postings");
    let unbalanced: i64 = sqlx::query_scalar(
        "select count(*) from (select entry_id from postings group by entry_id having sum(amount) <> 0) e",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(unbalanced, 0, "a journal entry does not sum to zero");
    let negative: i64 =
        sqlx::query_scalar("select count(*) from accounts where kind = 'user' and balance < 0")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(negative, 0);
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn every_route_but_health_and_webhook_needs_the_api_key(pool: PgPool) {
    let api = Api::new(pool);
    let health = api
        .send(Request::get("/health").body(Body::empty()).unwrap())
        .await;
    assert_eq!(health.status, StatusCode::OK);

    let anonymous = api
        .send(
            Request::get(format!("/accounts/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.body["error"]["code"], "unauthorized");

    let wrong = api
        .send(
            Request::post("/accounts")
                .header("authorization", "Bearer not-the-key")
                .header("idempotency-key", "k")
                .body(Body::from(r#"{"name":"x","currency":"NGN"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn bad_input_gets_a_json_error_never_internal_detail(pool: PgPool) {
    let api = Api::new(pool);
    let bad_currency = api
        .post(
            "/accounts",
            "a1",
            json!({ "name": "Ada", "currency": "naira" }),
        )
        .await;
    assert_eq!(bad_currency.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_currency.body["error"]["code"], "bad_request");

    let bad_id = api.get("/accounts/not-a-uuid").await;
    assert_eq!(bad_id.status, StatusCode::BAD_REQUEST);
    assert!(
        bad_id.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("UUID")
    );

    let missing = api.get(&format!("/accounts/{}", Uuid::new_v4())).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let unknown_field = api
        .post(
            "/accounts",
            "a2",
            json!({ "name": "Ada", "currency": "NGN", "balance": 1000000 }),
        )
        .await;
    assert_eq!(
        unknown_field.status,
        StatusCode::BAD_REQUEST,
        "clients cannot set a balance"
    );

    let no_key = api
        .send(
            Request::post("/accounts")
                .header("authorization", format!("Bearer {API_KEY}"))
                .body(Body::from(r#"{"name":"Ada","currency":"NGN"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(no_key.status, StatusCode::BAD_REQUEST);
    assert!(
        no_key.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Idempotency-Key")
    );
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn a_paystack_deposit_credits_the_account_exactly_once(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let reference = api.fund(ada, 500_000).await;
    assert_eq!(api.balance(ada).await, 500_000);

    // Paystack retries deliveries, and anyone holding a captured webhook can replay it. Copies
    // must neither credit again nor grow the audit log.
    for _ in 0..5 {
        let again = api
            .webhook(&charge(&reference, 500_000, "NGN"), SECRET)
            .await;
        assert_eq!(again.status, StatusCode::OK);
    }
    assert_eq!(api.balance(ada).await, 500_000);
    assert_eq!(outcomes(&pool).await, ["credited", "already_settled"]);
    assert_books_balance(&pool).await;
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn forged_mismatched_and_unknown_webhooks_credit_nothing(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let d = api
        .post(
            "/deposits",
            "d1",
            json!({ "account_id": ada, "amount": 10_000 }),
        )
        .await;
    let reference = d.body["reference"].as_str().unwrap().to_owned();

    let forged = api
        .webhook(&charge(&reference, 10_000, "NGN"), "sk_test_attacker")
        .await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
    assert_eq!(api.balance(ada).await, 0);

    let unsigned = api
        .send(
            Request::post("/webhooks/paystack")
                .body(Body::from(charge(&reference, 10_000, "NGN").to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(unsigned.status, StatusCode::UNAUTHORIZED);

    let unknown = api
        .webhook(&charge("dep_nobody", 10_000, "NGN"), SECRET)
        .await;
    assert_eq!(unknown.status, StatusCode::OK);

    let other_event = api
        .webhook(
            &json!({ "event": "transfer.success", "data": { "reference": reference, "amount": 10_000,
                     "currency": "NGN", "status": "success" } }),
            SECRET,
        )
        .await;
    assert_eq!(other_event.status, StatusCode::OK);
    assert_eq!(api.balance(ada).await, 0);

    // The customer paid less than the deposit asked for: hold for review, never credit.
    let short = api.webhook(&charge(&reference, 100, "NGN"), SECRET).await;
    assert_eq!(short.status, StatusCode::OK);
    assert_eq!(api.balance(ada).await, 0);
    let deposit = api
        .get(&format!("/deposits/{}", d.body["id"].as_str().unwrap()))
        .await;
    assert_eq!(deposit.body["status"], "needs_review");

    // A later delivery for a deposit under review changes nothing; a person decides.
    let late = api
        .webhook(&charge(&reference, 10_000, "NGN"), SECRET)
        .await;
    assert_eq!(late.status, StatusCode::OK);
    assert_eq!(api.balance(ada).await, 0);
    assert_eq!(
        outcomes(&pool).await,
        [
            "unknown_reference",
            "ignored",
            "needs_review",
            "already_settled"
        ]
    );
}

/// Paystack sends many event types, most without charge fields. Every authentic one gets a 200
/// so Paystack does not retry it for days.
#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn every_authentic_event_gets_a_200_whatever_its_shape(pool: PgPool) {
    let api = Api::new(pool.clone());
    let sparse = api
        .webhook(
            &json!({ "event": "subscription.create", "data": { "plan": {} } }),
            SECRET,
        )
        .await;
    assert_eq!(sparse.status, StatusCode::OK);
    let no_data = api
        .webhook(&json!({ "event": "customeridentification.failed" }), SECRET)
        .await;
    assert_eq!(no_data.status, StatusCode::OK);
    let broken_charge = api
        .webhook(
            &json!({ "event": "charge.success", "data": { "reference": "dep_x" } }),
            SECRET,
        )
        .await;
    assert_eq!(broken_charge.status, StatusCode::OK);
    assert_eq!(outcomes(&pool).await, ["ignored", "ignored", "malformed"]);
}

/// A charge reported as not successful leaves the deposit open, so the customer's next,
/// successful attempt with the same reference is still credited.
#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn a_failed_charge_keeps_the_deposit_open_for_a_successful_one(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let d = api
        .post(
            "/deposits",
            "d1",
            json!({ "account_id": ada, "amount": 7_000 }),
        )
        .await;
    let reference = d.body["reference"].as_str().unwrap().to_owned();
    let mut failed = charge(&reference, 7_000, "NGN");
    failed["data"]["status"] = json!("failed");
    api.webhook(&failed, SECRET).await;
    assert_eq!(api.balance(ada).await, 0);
    api.webhook(&charge(&reference, 7_000, "NGN"), SECRET).await;
    assert_eq!(api.balance(ada).await, 7_000);
    assert_eq!(outcomes(&pool).await, ["not_successful", "credited"]);
    assert_books_balance(&pool).await;
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn control_characters_and_oversized_bodies_are_client_errors(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let bayo = api.account("Bayo", "NGN").await;
    api.fund(ada, 100).await;

    // Postgres cannot store NUL in text; it must be a 400 (stored and replayed), not a 500.
    let nul =
        json!({ "from_account": ada, "to_account": bayo, "amount": 1, "description": "rent\u{0}" });
    let first = api.post("/transfers", "nul", nul.clone()).await;
    assert_eq!(first.status, StatusCode::BAD_REQUEST, "{:?}", first.body);
    let retry = api.post("/transfers", "nul", nul).await;
    assert!(retry.replayed);
    let named = api
        .post(
            "/accounts",
            "nul-name",
            json!({ "name": "A\u{0}da", "currency": "NGN" }),
        )
        .await;
    assert_eq!(named.status, StatusCode::BAD_REQUEST);

    let huge = api
        .post(
            "/accounts",
            "huge",
            json!({ "name": "x".repeat(100_000), "currency": "NGN" }),
        )
        .await;
    assert_eq!(huge.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(api.balance(ada).await, 100);
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn transfers_move_money_and_refuse_what_they_cannot_do(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let bayo = api.account("Bayo", "NGN").await;
    let carl = api.account("Carl", "USD").await;
    api.fund(ada, 1_000).await;

    let ok = api.transfer("t1", ada, bayo, 300).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{:?}", ok.body);
    assert_eq!(api.balance(ada).await, 700);
    assert_eq!(api.balance(bayo).await, 300);

    let too_much = api.transfer("t2", ada, bayo, 701).await;
    assert_eq!(too_much.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(too_much.body["error"]["code"], "insufficient_funds");

    let fx = api.transfer("t3", ada, carl, 1).await;
    assert_eq!(fx.body["error"]["code"], "currency_mismatch");

    let to_self = api.transfer("t4", ada, ada, 1).await;
    assert_eq!(to_self.status, StatusCode::BAD_REQUEST);

    let zero = api.transfer("t5", ada, bayo, 0).await;
    assert_eq!(zero.status, StatusCode::BAD_REQUEST);

    let ghost = api.transfer("t6", ada, Uuid::new_v4(), 1).await;
    assert_eq!(ghost.status, StatusCode::NOT_FOUND);

    assert_eq!(api.balance(ada).await, 700);
    assert_books_balance(&pool).await;
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn a_retried_request_replays_instead_of_moving_money_twice(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let bayo = api.account("Bayo", "NGN").await;
    api.fund(ada, 1_000).await;

    let first = api.transfer("pay-rent", ada, bayo, 400).await;
    let retry = api.transfer("pay-rent", ada, bayo, 400).await;
    assert_eq!(first.status, StatusCode::CREATED);
    assert_eq!(retry.status, StatusCode::CREATED);
    assert!(!first.replayed && retry.replayed);
    assert_eq!(first.body, retry.body, "the same entry comes back");
    assert_eq!(api.balance(ada).await, 600);

    let reused = api.transfer("pay-rent", ada, bayo, 999).await;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reused.body["error"]["code"], "idempotency_key_reused");

    // A refusal is an answer too: retrying it returns the same refusal, even after the
    // account is topped up, so a client never sees a request change its outcome.
    let refused = api.transfer("big", ada, bayo, 5_000).await;
    assert_eq!(refused.body["error"]["code"], "insufficient_funds");
    api.fund(ada, 10_000).await;
    let refused_again = api.transfer("big", ada, bayo, 5_000).await;
    assert!(refused_again.replayed);
    assert_eq!(refused_again.body["error"]["code"], "insufficient_funds");
    assert_books_balance(&pool).await;
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn concurrent_copies_of_one_request_apply_once(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let bayo = api.account("Bayo", "NGN").await;
    api.fund(ada, 1_000).await;

    let replies =
        futures::future::join_all((0..10).map(|_| api.transfer("double-tap", ada, bayo, 100)))
            .await;
    assert!(replies.iter().all(|r| r.status == StatusCode::CREATED));
    assert_eq!(replies.iter().filter(|r| !r.replayed).count(), 1);
    assert_eq!(api.balance(ada).await, 900);
    assert_books_balance(&pool).await;
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn concurrent_transfers_never_overdraw(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let bayo = api.account("Bayo", "NGN").await;
    api.fund(ada, 10).await;

    // 50 transfers of 1 kobo race for a balance of 10.
    let replies =
        futures::future::join_all((0..50).map(|i| api.transfer(format!("race-{i}"), ada, bayo, 1)))
            .await;
    let created = replies
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count();
    let refused = replies
        .iter()
        .filter(|r| r.body["error"]["code"] == "insufficient_funds")
        .count();
    assert_eq!((created, refused), (10, 40));
    assert_eq!(api.balance(ada).await, 0);
    assert_eq!(api.balance(bayo).await, 10);
    assert_books_balance(&pool).await;
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn opposite_transfers_at_once_do_not_deadlock(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;
    let bayo = api.account("Bayo", "NGN").await;
    api.fund(ada, 1_000).await;
    api.fund(bayo, 1_000).await;
    let replies = futures::future::join_all((0..40).map(|i| {
        let (from, to) = if i % 2 == 0 { (ada, bayo) } else { (bayo, ada) };
        api.transfer(format!("swap-{i}"), from, to, 10)
    }))
    .await;
    assert!(replies.iter().all(|r| r.status == StatusCode::CREATED));
    assert_eq!(api.balance(ada).await, 1_000);
    assert_eq!(api.balance(bayo).await, 1_000);
    assert_books_balance(&pool).await;
}

#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn the_statement_pages_newest_first(pool: PgPool) {
    let api = Api::new(pool);
    let ada = api.account("Ada", "NGN").await;
    let bayo = api.account("Bayo", "NGN").await;
    api.fund(ada, 1_000).await;
    for i in 1..=4 {
        api.transfer(format!("s{i}"), ada, bayo, i).await;
    }
    let page1 = api.get(&format!("/accounts/{ada}/postings?limit=3")).await;
    let amounts: Vec<i64> = page1.body["postings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["amount"].as_i64().unwrap())
        .collect();
    assert_eq!(amounts, [-4, -3, -2]);
    let before = page1.body["next_before"].as_i64().unwrap();
    let page2 = api
        .get(&format!("/accounts/{ada}/postings?limit=3&before={before}"))
        .await;
    let amounts: Vec<i64> = page2.body["postings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["amount"].as_i64().unwrap())
        .collect();
    assert_eq!(amounts, [-1, 1_000]);
    assert!(page2.body["next_before"].is_null());

    let too_many = api
        .get(&format!("/accounts/{ada}/postings?limit=500"))
        .await;
    assert_eq!(too_many.status, StatusCode::BAD_REQUEST);
}

/// The money rules hold even for code that bypasses the API and writes SQL directly.
#[sqlx::test(migrator = "payments_ledger::MIGRATOR")]
async fn the_database_itself_refuses_bad_books(pool: PgPool) {
    let api = Api::new(pool.clone());
    let ada = api.account("Ada", "NGN").await;

    // An entry that does not balance fails at commit.
    let mut tx = pool.begin().await.unwrap();
    let entry = Uuid::new_v4();
    sqlx::query("insert into journal_entries (id, kind) values ($1, 'transfer')")
        .bind(entry)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "insert into postings (entry_id, account_id, amount, currency) values ($1, $2, 500, 'NGN')",
    )
    .bind(entry)
    .bind(ada)
    .execute(&mut *tx)
    .await
    .unwrap();
    assert!(tx.commit().await.is_err(), "an unbalanced entry committed");

    // A customer balance cannot go below zero.
    let overdraw = sqlx::query("update accounts set balance = -1 where id = $1")
        .bind(ada)
        .execute(&pool)
        .await;
    assert!(overdraw.is_err());

    // History cannot be edited.
    api.fund(ada, 100).await;
    let edit = sqlx::query("update postings set amount = amount * 100")
        .execute(&pool)
        .await;
    assert!(edit.is_err(), "postings were edited");
    let delete = sqlx::query("delete from journal_entries")
        .execute(&pool)
        .await;
    assert!(delete.is_err(), "journal entries were deleted");
    assert_books_balance(&pool).await;
}
