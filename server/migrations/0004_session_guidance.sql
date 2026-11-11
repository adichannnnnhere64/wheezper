-- Per-session brief for the AI: what the meeting/interview is about, plus the
-- answer guidance the user asked for ("one sentence", "correct my grammar").
-- Both are prompt text, never transcript: they do not touch transcript_entries.
ALTER TABLE sessions ADD COLUMN ai_context TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN ai_instructions TEXT NOT NULL DEFAULT '';
