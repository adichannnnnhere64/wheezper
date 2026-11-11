CREATE TABLE sessions (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'active', 'completed')),
    started_at TIMESTAMPTZ,
    ended_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- sha256(session token): the raw token is only ever held by the client
    token_hash TEXT NOT NULL UNIQUE
);

CREATE INDEX sessions_created_at_idx ON sessions (created_at DESC);
CREATE INDEX sessions_status_idx ON sessions (status);

CREATE TABLE devices (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX devices_expires_at_idx ON devices (expires_at);

CREATE TABLE transcript_entries (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    -- idempotency key from the publishing client; retries collapse onto it
    event_id UUID NOT NULL UNIQUE,
    speaker VARCHAR(64) NOT NULL DEFAULT '',
    content TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sequence_number BIGINT NOT NULL,
    UNIQUE (session_id, sequence_number)
);

CREATE INDEX transcript_entries_session_seq_idx
    ON transcript_entries (session_id, sequence_number);
CREATE INDEX transcript_entries_fts_idx
    ON transcript_entries USING GIN (to_tsvector('english', content));

CREATE TABLE assistant_messages (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    question TEXT NOT NULL,
    answer TEXT NOT NULL,
    provider TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX assistant_messages_session_idx
    ON assistant_messages (session_id, created_at);
