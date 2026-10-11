//! Deposits: money arriving from Paystack.
//!
//! 1. [`open`] creates a pending deposit with a unique `reference`. The app passes that reference
//!    to Paystack when it initialises the payment.
//! 2. Paystack reports the charge to the webhook, which calls [`settle`]. The deposit is credited
//!    once, and only if the charge succeeded for exactly the amount and currency asked for.

use crate::error::ApiError;
use crate::ledger::{self, EntryKind, Leg};
use crate::paystack::ChargeData;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

/// The columns of [`Deposit`], shared by every query that returns one.
macro_rules! deposit_columns {
    () => {
        "id, account_id, reference, amount, currency, status, entry_id, created_at, settled_at"
    };
}

/// Mirrors the `deposits.status` check in the migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Completed,
    /// Paystack reported a successful charge that does not match the deposit. Nothing was
    /// credited; a person decides what to do.
    NeedsReview,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Completed => "completed",
            Status::NeedsReview => "needs_review",
        }
    }
}

/// What a webhook delivery did, as recorded in `webhook_events.outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Credited,
    /// The deposit was already completed or held for review; nothing changed.
    AlreadySettled,
    /// The charge did not succeed. The deposit stays pending for a later successful charge.
    NotSuccessful,
    /// The charge succeeded but its amount or currency differs from the deposit.
    NeedsReview,
    UnknownReference,
    /// Signed by Paystack but not a `charge.success` this service acts on.
    Ignored,
    /// A `charge.success` whose payload lacks the fields this service needs.
    Malformed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Credited => "credited",
            Outcome::AlreadySettled => "already_settled",
            Outcome::NotSuccessful => "not_successful",
            Outcome::NeedsReview => "needs_review",
            Outcome::UnknownReference => "unknown_reference",
            Outcome::Ignored => "ignored",
            Outcome::Malformed => "malformed",
        }
    }
}

#[derive(Debug, Serialize, FromRow)]
pub struct Deposit {
    pub id: Uuid,
    pub account_id: Uuid,
    pub reference: String,
    pub amount: i64,
    pub currency: String,
    pub status: String,
    pub entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
}

pub async fn open(
    conn: &mut PgConnection,
    account_id: Uuid,
    amount: i64,
) -> Result<Deposit, ApiError> {
    ledger::validate_amount(amount)?;
    let account = ledger::find_account(conn, account_id).await?;
    let deposit = sqlx::query_as::<_, Deposit>(concat!(
        "insert into deposits (id, account_id, reference, amount, currency)
         values ($1, $2, $3, $4, $5) returning ",
        deposit_columns!()
    ))
    .bind(Uuid::new_v4())
    .bind(account.id)
    .bind(format!("dep_{}", Uuid::new_v4().simple()))
    .bind(amount)
    .bind(&account.currency)
    .fetch_one(&mut *conn)
    .await?;
    Ok(deposit)
}

pub async fn find(conn: &mut PgConnection, id: Uuid) -> Result<Deposit, ApiError> {
    sqlx::query_as::<_, Deposit>(concat!(
        "select ",
        deposit_columns!(),
        " from deposits where id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ApiError::not_found("deposit"))
}

/// Applies a Paystack charge to its deposit.
pub async fn settle(conn: &mut PgConnection, charge: &ChargeData) -> Result<Outcome, ApiError> {
    // The row lock makes a redelivery that arrives mid-processing wait, then see `completed`.
    let Some(deposit) = sqlx::query_as::<_, Deposit>(concat!(
        "select ",
        deposit_columns!(),
        " from deposits where reference = $1 for update"
    ))
    .bind(&charge.reference)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(Outcome::UnknownReference);
    };
    if deposit.status != Status::Pending.as_str() {
        return Ok(Outcome::AlreadySettled);
    }
    if charge.status != "success" {
        return Ok(Outcome::NotSuccessful);
    }
    if charge.amount != deposit.amount || charge.currency != deposit.currency {
        set_status(conn, deposit.id, Status::NeedsReview, None).await?;
        tracing::warn!(reference = %charge.reference, "paystack charge does not match its deposit; held for review");
        return Ok(Outcome::NeedsReview);
    }
    let clearing = ledger::clearing_account(conn, "paystack", &deposit.currency).await?;
    let entry_id = ledger::post_entry(
        conn,
        EntryKind::Deposit,
        Some("Paystack deposit"),
        &deposit.currency,
        &[
            Leg {
                account_id: clearing,
                amount: -deposit.amount,
            },
            Leg {
                account_id: deposit.account_id,
                amount: deposit.amount,
            },
        ],
    )
    .await?;
    set_status(conn, deposit.id, Status::Completed, Some(entry_id)).await?;
    Ok(Outcome::Credited)
}

async fn set_status(
    conn: &mut PgConnection,
    id: Uuid,
    status: Status,
    entry_id: Option<Uuid>,
) -> Result<(), ApiError> {
    sqlx::query("update deposits set status = $2, entry_id = $3, settled_at = now() where id = $1")
        .bind(id)
        .bind(status.as_str())
        .bind(entry_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Records what a webhook delivery did. A repeat of the same outcome is not stored twice.
pub async fn record_webhook(
    conn: &mut PgConnection,
    event: &str,
    reference: Option<&str>,
    outcome: Outcome,
) -> Result<(), ApiError> {
    sqlx::query(
        "insert into webhook_events (provider, event, reference, outcome)
         values ('paystack', $1, $2, $3) on conflict do nothing",
    )
    .bind(event)
    .bind(reference)
    .bind(outcome.as_str())
    .execute(&mut *conn)
    .await?;
    Ok(())
}
