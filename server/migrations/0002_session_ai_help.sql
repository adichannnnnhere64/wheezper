-- Per-session interview AI help (auto-answer the far side's questions).
ALTER TABLE sessions ADD COLUMN ai_help BOOLEAN NOT NULL DEFAULT FALSE;
