CREATE TABLE IF NOT EXISTS pools (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    private BOOLEAN NOT NULL,
    policy JSONB NOT NULL,
    quota JSONB NOT NULL,
    distinct_sites BOOLEAN NOT NULL,
    admin BIGINT
);
CREATE TABLE IF NOT EXISTS invites (
    token TEXT PRIMARY KEY,
    pool BIGINT NOT NULL REFERENCES pools(id),
    inviter BIGINT,
    used_by BIGINT
);
CREATE TABLE IF NOT EXISTS accounts (
    id BIGSERIAL PRIMARY KEY,
    pool BIGINT NOT NULL REFERENCES pools(id),
    public_key TEXT NOT NULL UNIQUE,
    inviter BIGINT,
    state TEXT NOT NULL DEFAULT 'active',
    over_since DOUBLE PRECISION,
    read_only_since DOUBLE PRECISION,
    banked_credit DOUBLE PRECISION NOT NULL DEFAULT 0,
    bucket_tokens DOUBLE PRECISION NOT NULL DEFAULT 0,
    bucket_at DOUBLE PRECISION NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS nodes (
    id BIGSERIAL PRIMARY KEY,
    pool BIGINT NOT NULL REFERENCES pools(id),
    account BIGINT NOT NULL REFERENCES accounts(id),
    public_key TEXT NOT NULL,
    addr TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    probe_hash TEXT NOT NULL,
    probe_bytes BIGINT NOT NULL,
    site TEXT NOT NULL,
    asn TEXT NOT NULL,
    pledged BIGINT NOT NULL,
    target_pledged BIGINT,
    state TEXT NOT NULL DEFAULT 'active',
    last_seen DOUBLE PRECISION NOT NULL,
    audit_ok BIGINT NOT NULL DEFAULT 0,
    audit_fail BIGINT NOT NULL DEFAULT 0,
    measured_bps DOUBLE PRECISION,
    probe_requested_at DOUBLE PRECISION,
    UNIQUE (pool, public_key)
);
CREATE TABLE IF NOT EXISTS probe_samples (
    id BIGSERIAL PRIMARY KEY,
    target BIGINT NOT NULL REFERENCES nodes(id),
    prober BIGINT NOT NULL REFERENCES nodes(id),
    bps DOUBLE PRECISION NOT NULL,
    at DOUBLE PRECISION NOT NULL
);
CREATE TABLE IF NOT EXISTS placement_refusals (
    host BIGINT NOT NULL REFERENCES nodes(id),
    reporter BIGINT NOT NULL REFERENCES accounts(id),
    at DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (host, reporter)
);
CREATE TABLE IF NOT EXISTS placement_groups (
    account BIGINT NOT NULL REFERENCES accounts(id),
    k INT NOT NULL,
    n INT NOT NULL,
    hosts BIGINT[] NOT NULL,
    used INT NOT NULL,
    PRIMARY KEY (account, k, n)
);
CREATE TABLE IF NOT EXISTS segments (
    id TEXT PRIMARY KEY,
    pool BIGINT NOT NULL REFERENCES pools(id),
    owner BIGINT NOT NULL REFERENCES accounts(id),
    k INT NOT NULL,
    n INT NOT NULL,
    len BIGINT NOT NULL,
    object_tpe TEXT NOT NULL,
    object_id TEXT NOT NULL,
    piece_len BIGINT NOT NULL,
    state TEXT NOT NULL,
    created_at DOUBLE PRECISION NOT NULL
);
CREATE INDEX IF NOT EXISTS segments_owner ON segments(owner);
CREATE TABLE IF NOT EXISTS pieces (
    segment TEXT NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
    piece INT NOT NULL,
    host BIGINT NOT NULL REFERENCES nodes(id),
    hash TEXT NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY (segment, piece)
);
CREATE INDEX IF NOT EXISTS pieces_host ON pieces(host);
CREATE TABLE IF NOT EXISTS repairs (
    id BIGSERIAL PRIMARY KEY,
    segment TEXT NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
    piece INT NOT NULL,
    target BIGINT NOT NULL REFERENCES nodes(id),
    started_at DOUBLE PRECISION NOT NULL,
    delivered BOOLEAN NOT NULL DEFAULT FALSE,
    UNIQUE (segment, piece)
);
CREATE TABLE IF NOT EXISTS pending_deletes (
    host BIGINT NOT NULL REFERENCES nodes(id),
    segment TEXT NOT NULL,
    piece INT NOT NULL,
    PRIMARY KEY (host, segment, piece)
);
CREATE TABLE IF NOT EXISTS pending_probes (
    id BIGSERIAL PRIMARY KEY,
    prober BIGINT NOT NULL REFERENCES nodes(id),
    target BIGINT NOT NULL REFERENCES nodes(id)
);
CREATE TABLE IF NOT EXISTS read_grants (
    nonce TEXT PRIMARY KEY,
    reader BIGINT NOT NULL REFERENCES accounts(id),
    host BIGINT NOT NULL REFERENCES nodes(id),
    max_bytes BIGINT NOT NULL,
    credited BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS objects (
    account BIGINT NOT NULL REFERENCES accounts(id),
    tpe TEXT NOT NULL,
    id TEXT NOT NULL,
    len BIGINT NOT NULL,
    segment TEXT NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
    PRIMARY KEY (account, tpe, id)
);
CREATE TABLE IF NOT EXISTS ledger (
    id BIGSERIAL PRIMARY KEY,
    at DOUBLE PRECISION NOT NULL,
    pool BIGINT NOT NULL,
    account BIGINT NOT NULL,
    kind TEXT NOT NULL,
    bytes BIGINT NOT NULL,
    ref TEXT
);
-- Added after the first release, so existing databases gain it too.
ALTER TABLE segments ADD COLUMN IF NOT EXISTS local_groups INT NOT NULL DEFAULT 0;
