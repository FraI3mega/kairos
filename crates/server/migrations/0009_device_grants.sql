-- One-time codes for signing a computer in through its own 127.0.0.1 (kairos login opens the browser and waits there).
-- Only the hash of the code is kept, and only the program holding the verifier behind challenge can use it.
CREATE TABLE device_grants (
    code_hash TEXT PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    challenge TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX device_grants_expires ON device_grants (expires_at);
