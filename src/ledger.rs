//! The double-entry core. Every function takes an open connection inside a transaction, so a
//! caller can combine several steps (and its idempotency record) into one atomic commit.

use crate::error::ApiError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

/// Largest amount accepted in one request, in minor units: 10^15 (ten trillion naira in kobo).
/// Far below `i64::MAX`, so summing many postings cannot overflow.
pub const MAX_AMOUNT: i64 = 1_000_000_000_000_000;
/// Mirrors the `accounts.name` check in the migration.
pub const MAX_NAME_CHARS: usize = 100;
/// Mirrors the `journal_entries.description` check in the migration.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// The columns of [`Account`], shared by every query that returns one.
macro_rules! account_columns {
    () => {
        "id, name, currency, kind, balance, created_at"
    };
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub currency: String,
    pub kind: String,
    pub balance: i64,
    pub created_at: DateTime<Utc>,
}

/// One leg of a journal entry: `amount` is added to `account_id` (negative takes money out).
#[derive(Debug, Clone, Copy)]
pub struct Leg {
    pub account_id: Uuid,
    pub amount: i64,
}

/// Why money moved. Mirrors the `journal_entries.kind` check in the migration.
#[derive(Debug, Clone, Copy)]
pub enum EntryKind {
    Transfer,
    Deposit,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            EntryKind::Transfer => "transfer",
            EntryKind::Deposit => "deposit",
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Posting {
    pub id: i64,
    pub entry_id: Uuid,
    pub kind: String,
    pub description: Option<String>,
    pub amount: i64,
    pub created_at: DateTime<Utc>,
}

pub fn validate_amount(amount: i64) -> Result<(), ApiError> {
    if amount <= 0 || amount > MAX_AMOUNT {
        return Err(ApiError::bad_request(format!(
            "amount must be a whole number of minor units between 1 and {MAX_AMOUNT}"
        )));
    }
    Ok(())
}

pub fn validate_currency(currency: &str) -> Result<(), ApiError> {
    if currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "currency must be a three-letter ISO 4217 code such as NGN",
        ))
    }
}

/// Free text from a client: bounded, and without control characters. Postgres rejects NUL in
/// `text`, so without this check a NUL would surface as a server error instead of a 400.
fn validate_text(field: &str, value: &str, max_chars: usize) -> Result<(), ApiError> {
    if value.chars().count() > max_chars {
        return Err(ApiError::bad_request(format!(
            "{field} must be at most {max_chars} characters"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

pub async fn create_account(
    conn: &mut PgConnection,
    name: &str,
    currency: &str,
) -> Result<Account, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    validate_text("name", name, MAX_NAME_CHARS)?;
    validate_currency(currency)?;
    let account = sqlx::query_as::<_, Account>(concat!(
        "insert into accounts (id, name, currency, kind) values ($1, $2, $3, 'user') returning ",
        account_columns!()
    ))
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(currency)
    .fetch_one(&mut *conn)
    .await?;
    Ok(account)
}

pub async fn find_account(conn: &mut PgConnection, id: Uuid) -> Result<Account, ApiError> {
    sqlx::query_as::<_, Account>(concat!(
        "select ",
        account_columns!(),
        " from accounts where id = $1 and kind = 'user'"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ApiError::not_found("account"))
}

/// The clearing account for money arriving from `provider` in `currency`. It is allowed to go
/// negative: its balance is the total that provider has sent into the ledger.
pub async fn clearing_account(
    conn: &mut PgConnection,
    provider: &str,
    currency: &str,
) -> Result<Uuid, ApiError> {
    let name = format!("{provider} clearing");
    sqlx::query(
        "insert into accounts (id, name, currency, kind, allow_negative)
         values ($1, $2, $3, 'system', true)
         on conflict (name, currency) where kind = 'system' do nothing",
    )
    .bind(Uuid::new_v4())
    .bind(&name)
    .bind(currency)
    .execute(&mut *conn)
    .await?;
    let id = sqlx::query_scalar::<_, Uuid>(
        "select id from accounts where kind = 'system' and name = $1 and currency = $2",
    )
    .bind(&name)
    .bind(currency)
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

/// Moves `amount` from one customer account to another, atomically.
///
/// Both rows are locked with `FOR UPDATE` in a fixed order (by id), so two opposite transfers
/// cannot deadlock and concurrent transfers from one account cannot overdraw it: each one waits
/// for the previous to commit, then sees the new balance.
pub async fn transfer(
    conn: &mut PgConnection,
    from: Uuid,
    to: Uuid,
    amount: i64,
    description: Option<&str>,
) -> Result<Uuid, ApiError> {
    validate_amount(amount)?;
    if from == to {
        return Err(ApiError::bad_request(
            "from_account and to_account must differ",
        ));
    }
    if let Some(d) = description {
        validate_text("description", d, MAX_DESCRIPTION_CHARS)?;
    }
    let locked = sqlx::query_as::<_, Account>(concat!(
        "select ",
        account_columns!(),
        " from accounts where id = any($1) and kind = 'user' order by id for update"
    ))
    .bind(vec![from, to])
    .fetch_all(&mut *conn)
    .await?;
    let source = locked
        .iter()
        .find(|a| a.id == from)
        .ok_or_else(|| ApiError::not_found("from_account"))?;
    let target = locked
        .iter()
        .find(|a| a.id == to)
        .ok_or_else(|| ApiError::not_found("to_account"))?;
    if source.currency != target.currency {
        return Err(ApiError::unprocessable(
            "currency_mismatch",
            format!(
                "cannot transfer {} to a {} account",
                source.currency, target.currency
            ),
        ));
    }
    if source.balance < amount {
        return Err(ApiError::unprocessable(
            "insufficient_funds",
            "the source account does not have enough funds",
        ));
    }
    post_entry(
        conn,
        EntryKind::Transfer,
        description,
        &source.currency,
        &[
            Leg {
                account_id: from,
                amount: -amount,
            },
            Leg {
                account_id: to,
                amount,
            },
        ],
    )
    .await
}

/// Writes one balanced journal entry and updates the cached balances. The database re-checks
/// that the legs sum to zero (at commit) and that no balance goes below zero.
pub async fn post_entry(
    conn: &mut PgConnection,
    kind: EntryKind,
    description: Option<&str>,
    currency: &str,
    legs: &[Leg],
) -> Result<Uuid, ApiError> {
    // Callers build the legs, so an imbalance here is a bug; refuse it in release builds too,
    // before the database trigger would.
    if legs.len() < 2 || legs.iter().map(|l| l.amount).sum::<i64>() != 0 {
        tracing::error!(?legs, "refusing an unbalanced journal entry");
        return Err(ApiError::internal());
    }
    let entry_id = Uuid::new_v4();
    sqlx::query("insert into journal_entries (id, kind, description) values ($1, $2, $3)")
        .bind(entry_id)
        .bind(kind.as_str())
        .bind(description)
        .execute(&mut *conn)
        .await?;
    for leg in legs {
        sqlx::query(
            "insert into postings (entry_id, account_id, amount, currency) values ($1, $2, $3, $4)",
        )
        .bind(entry_id)
        .bind(leg.account_id)
        .bind(leg.amount)
        .bind(currency)
        .execute(&mut *conn)
        .await?;
        sqlx::query("update accounts set balance = balance + $2 where id = $1")
            .bind(leg.account_id)
            .bind(leg.amount)
            .execute(&mut *conn)
            .await?;
    }
    Ok(entry_id)
}

/// A page of an account's postings, newest first. `before` is the last posting id of the
/// previous page.
///
/// Pages follow posting ids, which are assigned when a transaction writes, not when it commits.
/// A transfer that commits while a client is paging can land behind its cursor; a client that
/// reconciles should re-read from the top rather than trust one pass.
pub async fn statement(
    conn: &mut PgConnection,
    account_id: Uuid,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<Posting>, ApiError> {
    let rows = sqlx::query_as::<_, Posting>(
        "select p.id, p.entry_id, e.kind, e.description, p.amount, e.created_at
         from postings p join journal_entries e on e.id = p.entry_id
         where p.account_id = $1 and ($2::bigint is null or p.id < $2)
         order by p.id desc limit $3",
    )
    .bind(account_id)
    .bind(before)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows)
}
