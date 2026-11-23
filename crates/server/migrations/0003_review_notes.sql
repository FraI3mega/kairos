-- Append-only: the app never edits or deletes a note.
-- Notes go with the participant's account; a deleted author leaves the note without a name.
CREATE TABLE review_notes (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    author_id BIGINT REFERENCES users ON DELETE SET NULL,
    body TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX review_notes_user ON review_notes (user_id, id DESC);
