# payments-ledger

[![CI](https://github.com/abiodunosagie/payments-ledger/actions/workflows/ci.yml/badge.svg)](https://github.com/abiodunosagie/payments-ledger/actions/workflows/ci.yml)

A double-entry payments ledger written in Rust (Axum, SQLx, Postgres). It holds customer wallets, moves money between them, and credits deposits from Paystack, the way a fintech app's back end has to:

- **Every journal entry balances.** Money only moves between accounts; the database refuses an entry whose postings do not sum to zero.
- **Balances never go negative.** Concurrent transfers lock the accounts involved, and a Postgres constraint backs that up.
- **Retries are safe.** Every write takes an `Idempotency-Key`. A retried request returns the original response and moves no money.
- **Webhooks are verified.** A Paystack `charge.success` is accepted only with a valid HMAC-SHA512 signature. It credits its deposit once, and only if the amount and currency match; a mismatch is held for review.
- **History is append-only.** Postings and journal entries cannot be updated or deleted, even with direct SQL.

## Quick start

You need Rust 1.94 or later and Postgres.

```bash
git clone https://github.com/abiodunosagie/payments-ledger.git
cd payments-ledger
createdb ledger

export DATABASE_URL=postgres://localhost/ledger
export LEDGER_API_KEY=$(openssl rand -hex 32)      # bearer key for the API, 32+ characters
export PAYSTACK_SECRET_KEY=sk_test_xxx             # your Paystack secret key
cargo run --release                                # migrates the database, listens on 127.0.0.1:8080
```

Configuration comes from the environment only. Error messages name the setting that is missing, never its value.

## Example

A real session against the server (ids shortened).

```bash
# Open a deposit for Ada. The reference goes to Paystack when the payment is initialised.
$ curl -s localhost:8080/deposits -H "Authorization: Bearer $LEDGER_API_KEY" \
    -H "Idempotency-Key: dep-1" -d '{"account_id":"0c0b6efd-…","amount":500000}'
{"id":"1d2ee081-…","reference":"dep_d01c89c2…","amount":500000,"currency":"NGN","status":"pending",…}

# Paystack calls the webhook; the signature is checked before anything is read.
$ curl -s -o /dev/null -w "%{http_code}\n" localhost:8080/webhooks/paystack \
    -H "x-paystack-signature: $SIG" -d "$BODY"
200                                   # credited
200                                   # the same delivery again: already settled, not credited

$ curl -s localhost:8080/webhooks/paystack -H "x-paystack-signature: deadbeef" -d "$BODY"
{"error":{"code":"invalid_signature","message":"webhook signature check failed"}}

# Pay the rent. The retry with the same key replays the first response.
$ curl -si localhost:8080/transfers -H "Idempotency-Key: rent-oct" … \
    -d '{"from_account":"0c0b6efd-…","to_account":"e0102c76-…","amount":150000,"description":"October rent"}'
HTTP/1.1 201 Created
{"amount":150000,"entry_id":"5eb83c9b-…",…}

$ curl -si localhost:8080/transfers -H "Idempotency-Key: rent-oct" …   # same request again
HTTP/1.1 201 Created
idempotent-replayed: true
{"amount":150000,"entry_id":"5eb83c9b-…",…}

$ curl -s localhost:8080/transfers -H "Idempotency-Key: big" … -d '{…,"amount":99900000}'
{"error":{"code":"insufficient_funds","message":"the source account does not have enough funds"}}

$ curl -s localhost:8080/accounts/0c0b6efd-… -H "Authorization: Bearer $LEDGER_API_KEY"
{"id":"0c0b6efd-…","name":"Ada","currency":"NGN","kind":"user","balance":350000,…}
```

Amounts are integers in the currency's minor unit: `500000` is ₦5,000.00 in kobo. Floats never touch money.

## API

All routes except `/health` and the webhook need `Authorization: Bearer <LEDGER_API_KEY>`. All writes need an `Idempotency-Key` header.

| Method and path | What it does |
|---|---|
| `POST /accounts` | Opens a customer account: `{"name","currency"}`. |
| `GET /accounts/{id}` | The account and its balance. |
| `GET /accounts/{id}/postings?limit=&before=` | Statement, newest first, paged by posting id. |
| `POST /transfers` | Moves money: `{"from_account","to_account","amount","description"?}`. |
| `POST /deposits` | Opens a pending deposit and returns its Paystack `reference`: `{"account_id","amount"}`. |
| `GET /deposits/{id}` | Deposit status: `pending`, `completed` or `needs_review`. |
| `POST /webhooks/paystack` | Paystack events. Authenticated by signature, not by API key. |
| `GET /health` | Liveness. |

Errors always have the same shape, `{"error":{"code","message"}}`, with stable codes such as `insufficient_funds`, `currency_mismatch`, `idempotency_key_reused` and `invalid_signature`. Database errors are logged on the server and answered with a generic `internal` error; no SQL or database text reaches the client. A malformed body gets the JSON parser's description of what was wrong with it.

## How it works

### Double entry, enforced by Postgres

Accounts hold a balance. Money moves only as a **journal entry** of two or more **postings** that sum to zero: a transfer of ₦1,500 is −150000 on the payer and +150000 on the payee. A Paystack deposit debits a `paystack clearing` system account and credits the customer, so the clearing account's balance is always the total Paystack has sent in.

The database enforces the rules, so a bug in the application cannot break the books:

- A deferred constraint trigger checks at commit that each entry sums to zero per currency, and that every posting is in its account's currency.
- `check (allow_negative or balance >= 0)` on accounts. Only system accounts may go negative.
- Triggers reject `UPDATE` and `DELETE` on postings and journal entries. Corrections are new entries.

`accounts.balance` is a cache of the account's postings, updated in the same transaction, so reading a balance never scans history. The tests check after every scenario that no cached balance has drifted from its postings.

### Concurrency

A transfer locks both accounts with `SELECT … FOR UPDATE`, always in id order, then checks funds and writes. Concurrent transfers from one account queue on the lock and each sees the balance the previous one left, and two opposite transfers cannot deadlock because they lock in the same order. The test suite races 50 one-kobo transfers against a balance of 10: exactly 10 succeed, 40 are refused, and the balance ends at zero. I checked that the test bites: with `FOR UPDATE` removed it fails, because some transfers read a stale balance and only the database constraint stops them.

Deposits in one currency all post against the same clearing account, so they settle one at a time. That is fine at webhook rates; a high-volume system would shard the clearing account.

### Idempotency

The first request with a key runs, and its response is stored in the same transaction as its effects. A retry with the same key and body gets that response back, marked `idempotent-replayed: true`. The same key with a different body is refused with `idempotency_key_reused`.

- **Simultaneous copies.** Requests with one key are serialised with a transaction-scoped advisory lock (`pg_advisory_xact_lock`), so ten copies of a request sent at once apply exactly once. A test does exactly that.
- **Refusals replay too.** A 4xx answer, such as insufficient funds, is stored and replayed: a client never sees one request change its outcome. The work runs in a savepoint, so a refused request leaves nothing behind.
- **Failures can be retried.** A 5xx stores nothing and rolls back, so the client can retry.

### Paystack deposits

1. The app calls `POST /deposits` and gets a `reference`, which it passes to Paystack when initialising the payment.
2. Paystack sends `charge.success` to `/webhooks/paystack` with an `x-paystack-signature` header: the hex HMAC-SHA512 of the raw body, keyed with the secret key. The signature is verified on the raw bytes, in constant time, before the JSON is parsed. Without a valid signature the answer is 401 and nothing is read.
3. The deposit row is locked by reference, so a redelivery that arrives mid-processing waits and then sees the result. A pending deposit is credited when the charge status is `success` and the amount and currency match what was asked for.
   - **Charge not successful:** nothing changes and the deposit stays pending, so the customer's next attempt with the same reference still works.
   - **Amount or currency differs:** the deposit is marked `needs_review` and nothing is credited. A person decides, instead of the money being silently dropped.
   - **Already completed or under review:** nothing changes.
4. Every authentic delivery is answered 200, whatever its event or outcome, so Paystack stops retrying. Events this service does not act on, and even ones without charge fields, are read by name first and recorded as `ignored`. What happened is stored in `webhook_events`: `credited`, `already_settled`, `not_successful`, `needs_review`, `unknown_reference`, `ignored` or `malformed`. A repeat of the same outcome is stored once, so replaying a captured webhook cannot grow the table.

### Security

- **API key.** Requests are checked against a SHA-256 digest of the key with `subtle::ConstantTimeEq`, so timing reveals neither its contents nor its length.
- **Secrets.** They come from the environment only, never from a file in the repo and never in logs.
- **Request bodies.** They are parsed with `deny_unknown_fields`, so a client cannot slip in a `balance` or `status`. Free text is length-limited and may not contain control characters. Amounts are capped at 10^15 minor units, so sums cannot overflow.
- **Limits.** Bodies are capped at 64 KB and requests at 10 seconds; the statement returns at most 100 postings a page.
- **Errors.** Every error maps to a fixed JSON shape, and internal details stay in the server log.
- **Transport.** The server speaks plain HTTP and binds to `127.0.0.1` by default. Put it behind a proxy that terminates TLS, since the API key and webhook signatures cross the wire. The database connection supports TLS (`sslmode=require` in `DATABASE_URL`).

## Tests

```bash
export DATABASE_URL=postgres://localhost/ledger   # a server where the user may create databases
cargo test
```

`#[sqlx::test]` gives every test its own fresh database with the migrations applied. Nothing calls Paystack; webhooks are signed locally with a test secret.

- Every route except `/health` and the webhook rejects a missing or wrong API key.
- Bad input gets a JSON 400, never internal detail; a client cannot set a balance.
- A Paystack deposit credits exactly once, however often the webhook is delivered.
- Forged, unsigned, mismatched, unknown and unrelated webhooks credit nothing; a mismatch is held for review.
- Every authentic event gets a 200, however sparse its payload, and a malformed charge is recorded, not retried.
- A failed charge keeps the deposit open, so the next successful one is credited.
- Replaying a webhook five times neither credits again nor grows the audit log.
- A NUL character in text is a 400 (stored and replayed), and an oversized body is a 413.
- Transfers move money, and refuse overdrafts, currency mismatches, self-transfers, zero amounts and unknown accounts.
- A retried request replays instead of moving money twice; a reused key with a new body is refused; a refusal replays too.
- Ten simultaneous copies of one request apply once.
- 50 concurrent transfers never overdraw a balance of 10.
- 40 opposite transfers at once finish without a deadlock.
- The statement pages newest first. Pages follow posting ids, which are assigned at write time, not commit time; a client reconciling while transfers are in flight should re-read from the top.
- The database itself refuses an unbalanced entry, a negative customer balance, and edits or deletes of history.

After each scenario the books are checked: cached balances equal their postings, every entry sums to zero, and no customer balance is negative.

CI runs formatting, Clippy with warnings denied, the full suite against a Postgres 17 service, and a build on the minimum supported Rust version.

## Project layout

```
migrations/0001_init.sql   schema, balance and append-only rules
src/
  main.rs         configuration from the environment, migrations, server
  lib.rs          router and shared state
  routes.rs       HTTP handlers and the idempotent-write wrapper
  ledger.rs       accounts, locking, balanced journal entries, statements
  deposits.rs     opening deposits, settling Paystack charges, the webhook log
  idempotency.rs  key lock, stored responses
  paystack.rs     webhook signature and payload
  auth.rs         bearer API key
  error.rs        JSON errors that never leak internals
tests/api.rs      end-to-end tests against Postgres
```

## Not included

This is the ledger, not the payment page. Initialising a Paystack transaction, refunds, payouts, multi-currency conversion, per-user API keys and expiry of old idempotency keys are out of scope. Keys are indexed by age, so pruning is one statement, such as `delete from idempotency_keys where created_at < now() - interval '7 days'`.

## License

MIT. See [LICENSE](LICENSE).

## Author

Abiodun Osagie, mobile, front-end and full-stack developer.
[Portfolio](https://smithportfolio.smitho.workers.dev) · [GitHub](https://github.com/abiodunosagie) · [LinkedIn](https://linkedin.com/in/osagieabiodun)
