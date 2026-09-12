-- The Thock Plus store: plans as editable rows, users with their gateway key
-- and allowance state, invites, and the usage ledger. Applied once by db.go;
-- later changes go in new numbered files, never edits to this one.

create table settings (
    -- A single row: the check keeps a second one from ever being inserted.
    id boolean primary key default true check (id),
    -- How many normalized units one dollar of gateway spend costs. 100 makes
    -- a unit a cent.
    units_per_dollar double precision not null default 100 check (units_per_dollar > 0)
);

insert into settings default values;

create table plans (
    id text primary key,
    name text not null,
    allowance_units bigint not null check (allowance_units >= 0),
    cycle_days integer not null default 30 check (cycle_days > 0),
    default_model text not null,
    fast_model text not null,
    warn_at_percent integer not null default 80 check (warn_at_percent between 1 and 100),
    max_turns_per_session integer not null default 0 check (max_turns_per_session >= 0),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table invites (
    code text primary key,
    plan_id text not null references plans (id),
    -- 0 means unlimited.
    max_uses integer not null default 0 check (max_uses >= 0),
    uses integer not null default 0 check (uses >= 0),
    note text not null default '',
    created_at timestamptz not null default now()
);

create table users (
    id text primary key,
    plan_id text not null references plans (id),
    device text not null default '',
    invite_code text references invites (code),
    credential_hash text not null unique,
    status text not null check (status in ('active', 'revoked')),
    created_at timestamptz not null,
    cycle_started_at timestamptz not null,
    gateway_key_hash text not null,
    -- The provisioned key itself. Extractable by design (spec decision 12):
    -- the budget cap bounds the damage, not secrecy of this row. Emptied on
    -- revocation.
    gateway_key_secret text not null,
    gateway_limit_usd double precision not null default 0,
    -- Gateway spend (dollars) when the current cycle started; usage in this
    -- cycle is whatever the gateway reports above it.
    usage_baseline_usd double precision not null default 0,
    used_units bigint not null default 0,
    adjust_units bigint not null default 0,
    last_sync_at timestamptz not null,
    exhausted boolean not null default false
);

create table ledger (
    id bigint generated always as identity primary key,
    at timestamptz not null,
    user_id text not null references users (id),
    units bigint not null default 0,
    -- connect, sync, adjust, reset, revoke.
    source text not null,
    note text not null default ''
);

create index ledger_user_id_at on ledger (user_id, at);

insert into plans (id, name, allowance_units, cycle_days, default_model, fast_model, warn_at_percent, max_turns_per_session)
values
    ('plus', 'Thock Plus', 1000, 30, 'google/gemini-2.5-flash', 'google/gemini-2.5-flash-lite', 80, 200),
    ('dev', 'Thock Plus (dev)', 300, 30, 'google/gemini-2.5-flash', 'google/gemini-2.5-flash-lite', 80, 200);
