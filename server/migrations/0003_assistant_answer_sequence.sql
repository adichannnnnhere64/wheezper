-- Anchor an answer to the transcript row it answers, so the phone can render it
-- under that row again after a restart (answers used to live only in app memory).
-- NULL = an answer asked through the free-form sheet, which answers no row.
ALTER TABLE assistant_messages ADD COLUMN answered_sequence BIGINT;

CREATE INDEX assistant_messages_answered_idx
    ON assistant_messages (session_id, answered_sequence, created_at DESC);
