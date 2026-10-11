-- Double-entry ledger. Money is always an integer number of minor units (kobo, cents), never a float.
-- The database enforces the rules that matter, so a bug in the application cannot break them:
-- every journal entry balances to zero, balances never go negative unless the account allows it,
-- and history is append-only.

create table accounts (
    id             uuid primary key,
    name           text not null check (length(name) between 1 and 100),
    currency       text not null check (currency ~ '^[A-Z]{3}$'),
    -- 'user' accounts hold customer money; 'system' accounts are the other side of money moving
    -- in or out of the ledger, such as the Paystack clearing account.
    kind           text not null check (kind in ('user', 'system')),
    allow_negative boolean not null default false,
    -- A cache of sum(postings.amount) for this account, updated in the same transaction as the
    -- postings so a balance read never has to scan history. Tests check it never drifts.
    balance        bigint not null default 0,
    created_at     timestamptz not null default now(),
    constraint balance_not_negative check (allow_negative or balance >= 0)
);

-- One clearing account per provider and currency.
create unique index accounts_system_name_currency on accounts (name, currency) where kind = 'system';

create table journal_entries (
    id          uuid primary key,
    kind        text not null check (kind in ('transfer', 'deposit')),
    description text check (length(description) <= 200),
    created_at  timestamptz not null default now()
);

create table postings (
    id         bigserial primary key,
    entry_id   uuid not null references journal_entries (id),
    account_id uuid not null references accounts (id),
    -- Positive credits the account, negative debits it.
    amount     bigint not null check (amount <> 0),
    currency   text not null
);

create index postings_account on postings (account_id, id);
create index postings_entry on postings (entry_id);

-- Every journal entry must sum to zero per currency, and each posting must be in its account's
-- currency. Deferred to commit time so all postings of an entry can be inserted first.
create function check_entry_balanced() returns trigger
language plpgsql as $$
declare
    unbalanced boolean;
    wrong_currency boolean;
begin
    select exists (
        select 1 from postings where entry_id = new.entry_id
        group by currency having sum(amount) <> 0
    ) into unbalanced;
    if unbalanced then
        raise exception 'journal entry % does not balance', new.entry_id
            using errcode = 'check_violation';
    end if;

    select exists (
        select 1 from postings p join accounts a on a.id = p.account_id
        where p.entry_id = new.entry_id and p.currency <> a.currency
    ) into wrong_currency;
    if wrong_currency then
        raise exception 'journal entry % posts in the wrong currency', new.entry_id
            using errcode = 'check_violation';
    end if;
    return null;
end;
$$;

create constraint trigger postings_balanced
    after insert on postings
    deferrable initially deferred
    for each row execute function check_entry_balanced();

-- History is never edited or deleted; corrections are new entries.
create function forbid_change() returns trigger
language plpgsql as $$
begin
    raise exception '% is append-only', tg_table_name using errcode = 'insufficient_privilege';
end;
$$;

create trigger postings_append_only
    before update or delete on postings
    for each row execute function forbid_change();

create trigger journal_entries_append_only
    before update or delete on journal_entries
    for each row execute function forbid_change();

-- A deposit is created before the customer pays. Its reference is passed to Paystack, and the
-- webhook credits the account once, only if the amount and currency match.
create table deposits (
    id           uuid primary key,
    account_id   uuid not null references accounts (id),
    reference    text not null unique,
    amount       bigint not null check (amount > 0),
    currency     text not null,
    status       text not null default 'pending' check (status in ('pending', 'completed', 'rejected')),
    entry_id     uuid references journal_entries (id),
    created_at   timestamptz not null default now(),
    settled_at   timestamptz
);

-- Every webhook delivery that passed the signature check, and what was done with it.
create table webhook_events (
    id          bigserial primary key,
    provider    text not null,
    event       text not null,
    reference   text,
    outcome     text not null,
    received_at timestamptz not null default now()
);

-- Responses to write requests, keyed by the client's Idempotency-Key, so a retried request
-- returns the original response instead of moving money twice.
create table idempotency_keys (
    key          text primary key check (length(key) between 1 and 200),
    request_hash text not null,
    status_code  integer not null,
    response     jsonb not null,
    created_at   timestamptz not null default now()
);
