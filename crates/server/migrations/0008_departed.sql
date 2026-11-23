-- What a banned or reviewed participant's account held when they deleted it, so signing in again with the same Hack Club
-- identity gets it back. Only written when the trust state or the notes are not the default.
CREATE TABLE departed (
    hca_sub TEXT PRIMARY KEY,
    trust TEXT NOT NULL CHECK (trust IN ('unscored', 'trusted', 'suspicious', 'banned')),
    notes JSONB NOT NULL DEFAULT '[]',
    deleted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
