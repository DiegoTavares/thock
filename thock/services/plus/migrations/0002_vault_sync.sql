-- Vault sync (spec: thock/specs/v34-vault-sync.md, contract:
-- thock/specs/v34-vault-sync-api.md). The server stores encrypted snapshots
-- and queues the phone's writes; it reads neither. Paths, sizes, versions and
-- hashes of the ciphertext are the only clear-text facts about a vault.

alter table plans add column vault_quota_bytes bigint not null default 0 check (vault_quota_bytes >= 0);
update plans set vault_quota_bytes = 209715200 where id in ('plus', 'dev');

create table vaults (
    id text primary key,
    user_id text not null unique references users (id),
    -- hex(sha256("thock-vault-key-check/1" || key))[0..32]: lets a mis-scanned
    -- or rotated key fail on the first call instead of the first decrypt.
    key_check text not null,
    created_at timestamptz not null,
    -- One counter per vault for file versions, one for write seqs. They
    -- only ever grow, even across a reset.
    latest_version bigint not null default 0,
    latest_seq bigint not null default 0,
    acked_through_seq bigint not null default 0,
    acked_at_version bigint not null default 0,
    bytes_used bigint not null default 0,
    -- Tombstones below this version were pruned; a cursor under it must do
    -- a full pull.
    tombstone_horizon bigint not null default 0,
    lapsed_at timestamptz
);

create table devices (
    id text primary key,
    vault_id text not null references vaults (id) on delete cascade,
    role text not null check (role in ('desk', 'phone')),
    name text not null default '',
    -- Phone only: the desk authenticates with its Plus credential.
    credential_hash text unique,
    apns_token text,
    paired_at timestamptz not null,
    last_seen_at timestamptz not null,
    unique (vault_id, role)
);

create table files (
    vault_id text not null references vaults (id) on delete cascade,
    path text not null,
    version bigint not null,
    deleted boolean not null default false,
    blob_id text,
    size_bytes bigint not null default 0,
    content_hash text,
    updated_by text not null,
    updated_at timestamptz not null,
    primary key (vault_id, path),
    unique (vault_id, version)
);

-- Uploads that were begun (signed URL handed out) but not committed yet.
create table pending_uploads (
    vault_id text not null references vaults (id) on delete cascade,
    blob_id text not null,
    path text not null,
    expected_version bigint not null,
    size_bytes bigint not null,
    content_hash text not null,
    created_at timestamptz not null,
    primary key (vault_id, blob_id)
);

create table writes (
    vault_id text not null references vaults (id) on delete cascade,
    seq bigint not null,
    client_id text not null,
    path text not null,
    base_version bigint not null,
    payload bytea not null,
    size_bytes bigint not null,
    created_at timestamptz not null,
    -- Set by the desk's ack; the payload is dropped then and the row stays
    -- a while so a retried upload still answers with the same seq.
    acked_at timestamptz,
    primary key (vault_id, seq),
    unique (vault_id, client_id)
);

create index writes_vault_unacked on writes (vault_id, seq) where acked_at is null;

create table pairings (
    vault_id text not null references vaults (id) on delete cascade,
    code_hash text not null,
    expires_at timestamptz not null,
    created_at timestamptz not null,
    used_at timestamptz,
    primary key (vault_id, code_hash)
);
