-- project '' is Other. Only counts: no text, and rebuilt together with user_days.
CREATE TABLE user_tool_days (
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    day DATE NOT NULL,
    project TEXT NOT NULL DEFAULT '',
    harness TEXT NOT NULL CHECK (harness ~ '^[a-z0-9][a-z0-9._-]{0,31}$'),
    prompts INT NOT NULL CHECK (prompts > 0),
    PRIMARY KEY (user_id, day, project, harness)
);
