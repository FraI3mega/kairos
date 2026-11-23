-- Facts keep the id they were recorded with; project_aliases is the resolved map every rollup and read goes through.
CREATE TABLE project_links (
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('device', 'merge', 'split')),
    at DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (user_id, from_id, to_id, source),
    CHECK (from_id <> to_id)
);
CREATE TABLE project_aliases (
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    alias TEXT NOT NULL,
    canonical TEXT NOT NULL,
    PRIMARY KEY (user_id, alias),
    CHECK (alias <> canonical)
);
