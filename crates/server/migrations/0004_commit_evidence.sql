-- `ts` is when the device saw the commit appear; the git dates are whatever the committer set.
ALTER TABLE commits
    ADD COLUMN device_id BIGINT REFERENCES devices ON DELETE SET NULL,
    ADD COLUMN authored_at BIGINT,
    ADD COLUMN committed_at BIGINT,
    ADD COLUMN via TEXT,
    ADD COLUMN changes JSONB,
    ADD COLUMN received_at TIMESTAMPTZ;
ALTER TABLE commits ALTER COLUMN received_at SET DEFAULT now();
