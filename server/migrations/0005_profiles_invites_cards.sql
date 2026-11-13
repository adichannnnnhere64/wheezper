-- Multi-user support: one profile per person, invite codes to bind a device to a
-- profile, and reusable answer cards the assistant can ground answers in.
--
-- Backwards compatible: existing devices keep profile_id = NULL and fall back to
-- the server-wide prompt.

CREATE TABLE profiles (
    id            UUID PRIMARY KEY,
    slug          TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL,
    -- Candidate/meeting facts injected into the assistant prompt. This is the
    -- only source of personal claims the model may use.
    summary       TEXT NOT NULL DEFAULT '',
    -- Optional per-profile model override (empty = server default).
    default_model TEXT NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE devices
    ADD COLUMN profile_id UUID REFERENCES profiles(id) ON DELETE SET NULL;

CREATE INDEX devices_profile_idx ON devices (profile_id);

CREATE TABLE invites (
    id         UUID PRIMARY KEY,
    code_hash  TEXT NOT NULL UNIQUE,
    profile_id UUID NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    label      TEXT NOT NULL DEFAULT '',
    max_uses   INTEGER NOT NULL DEFAULT 1,
    uses       INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE answer_cards (
    id         UUID PRIMARY KEY,
    topic      TEXT NOT NULL,
    question   TEXT NOT NULL,
    answer     TEXT NOT NULL,
    keywords   TEXT NOT NULL DEFAULT '',
    -- NULL = shared by every profile.
    profile_id UUID REFERENCES profiles(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX answer_cards_fts_idx
    ON answer_cards USING GIN (to_tsvector('english', question || ' ' || keywords || ' ' || answer));

CREATE INDEX answer_cards_topic_idx ON answer_cards (topic);
