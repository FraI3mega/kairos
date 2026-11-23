-- Event time (ts, start_ts, first_seen...) is unix seconds as float8, like the event log.
-- Server time (created_at, expires_at...) is timestamptz.

CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    hca_sub TEXT NOT NULL UNIQUE,
    username TEXT NOT NULL UNIQUE CHECK (username ~ '^[a-z0-9][a-z0-9_-]{1,31}$' AND username !~ '^[0-9]+$'),
    display_name TEXT CHECK (char_length(display_name) <= 64),
    avatar_url TEXT,
    email TEXT,
    slack_id TEXT,
    verification_status TEXT,
    ysws_eligible BOOLEAN,
    role TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('user', 'reviewer', 'admin')),
    trust TEXT NOT NULL DEFAULT 'unscored' CHECK (trust IN ('unscored', 'trusted', 'suspicious', 'banned')),
    is_public BOOLEAN NOT NULL DEFAULT TRUE,
    show_ai_tools BOOLEAN NOT NULL DEFAULT TRUE,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at TIMESTAMPTZ
);
CREATE INDEX users_slack_id ON users (slack_id) WHERE slack_id IS NOT NULL;
CREATE INDEX users_role ON users (role) WHERE role <> 'user';

CREATE TABLE web_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX web_sessions_user ON web_sessions (user_id);
CREATE INDEX web_sessions_expires ON web_sessions (expires_at);

-- The browser holds the state; the server keeps the nonce and the PKCE verifier.
CREATE TABLE auth_flows (
    state_hash TEXT PRIMARY KEY,
    nonce TEXT NOT NULL,
    verifier TEXT NOT NULL,
    next TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX auth_flows_expires ON auth_flows (expires_at);

CREATE TABLE programs (
    id BIGSERIAL PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9][a-z0-9-]{1,39}$'),
    name TEXT NOT NULL,
    created_by BIGINT REFERENCES users ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE api_keys (
    id BIGSERIAL PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('editor', 'program')),
    user_id BIGINT REFERENCES users ON DELETE CASCADE,
    program_id BIGINT REFERENCES programs ON DELETE CASCADE,
    name TEXT,
    token_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    CHECK ((kind = 'editor' AND user_id IS NOT NULL AND program_id IS NULL)
        OR (kind = 'program' AND program_id IS NOT NULL))
);
CREATE UNIQUE INDEX api_keys_one_editor ON api_keys (user_id) WHERE kind = 'editor' AND revoked_at IS NULL;
CREATE INDEX api_keys_program ON api_keys (program_id) WHERE program_id IS NOT NULL;

CREATE TABLE devices (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    os TEXT NOT NULL,
    client_version TEXT,
    backends JSONB,
    anchor TEXT,
    head TEXT,
    event_count BIGINT NOT NULL DEFAULT 0,
    first_event_ts DOUBLE PRECISION,
    last_event_ts DOUBLE PRECISION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX devices_user ON devices (user_id);

CREATE TABLE device_codes (
    user_code TEXT PRIMARY KEY,
    poll_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    client_version TEXT,
    user_id BIGINT REFERENCES users ON DELETE CASCADE,
    approved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX device_codes_expires ON device_codes (expires_at);

-- The raw record. body is the event exactly as the device hashed it (prev and hash included).
CREATE TABLE events (
    device_id BIGINT NOT NULL REFERENCES devices ON DELETE CASCADE,
    seq BIGINT NOT NULL,
    ts DOUBLE PRECISION NOT NULL,
    kind TEXT NOT NULL,
    project TEXT,
    body JSONB NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (device_id, seq)
);
CREATE INDEX events_device_ts ON events (device_id, ts);

-- project NULL is Other. Spans never exceed six hours, so a range query can bound its scan on start_ts.
CREATE TABLE spans (
    device_id BIGINT NOT NULL REFERENCES devices ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    start_ts DOUBLE PRECISION NOT NULL,
    end_ts DOUBLE PRECISION NOT NULL,
    project TEXT,
    class TEXT NOT NULL,
    rule TEXT NOT NULL,
    CHECK (end_ts > start_ts AND end_ts - start_ts <= 21600)
);
CREATE INDEX spans_device_start ON spans (device_id, start_ts);
CREATE INDEX spans_user_start ON spans (user_id, start_ts);

-- Never credited. Kept beside spans so user_days can be rebuilt without re-reading other devices' events.
CREATE TABLE agent_spans (
    device_id BIGINT NOT NULL REFERENCES devices ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    start_ts DOUBLE PRECISION NOT NULL,
    end_ts DOUBLE PRECISION NOT NULL,
    project TEXT,
    CHECK (end_ts > start_ts AND end_ts - start_ts <= 21600)
);
CREATE INDEX agent_spans_device_start ON agent_spans (device_id, start_ts);
CREATE INDEX agent_spans_user_start ON agent_spans (user_id, start_ts);

-- project '' is Other; class is a credit class or 'agent'. Each second of real time counts once per day across devices.
CREATE TABLE user_days (
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    day DATE NOT NULL,
    project TEXT NOT NULL,
    class TEXT NOT NULL,
    seconds DOUBLE PRECISION NOT NULL CHECK (seconds >= 0),
    PRIMARY KEY (user_id, day, project, class)
);
CREATE INDEX user_days_day ON user_days (day);

CREATE TABLE projects (
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    id TEXT NOT NULL,
    repo_url TEXT,
    hidden BOOLEAN NOT NULL DEFAULT FALSE,
    first_seen DOUBLE PRECISION NOT NULL,
    last_seen DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (user_id, id)
);

CREATE TABLE commits (
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    project TEXT NOT NULL,
    sha TEXT NOT NULL CHECK (sha ~ '^[0-9a-f]{7,64}$'),
    ts DOUBLE PRECISION NOT NULL,
    files INT NOT NULL,
    added INT NOT NULL,
    removed INT NOT NULL,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (user_id, project, sha)
);
CREATE INDEX commits_user_ts ON commits (user_id, ts);

CREATE TABLE heartbeats (
    user_id BIGINT NOT NULL REFERENCES users ON DELETE CASCADE,
    time DOUBLE PRECISION NOT NULL,
    entity_hash TEXT NOT NULL,
    project TEXT,
    language TEXT,
    editor TEXT,
    branch TEXT,
    category TEXT,
    is_write BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (user_id, time, entity_hash)
);

-- Kept when users are deleted (actor and target become NULL), so detail never holds personal data beyond what the action is about.
CREATE TABLE audit_log (
    id BIGSERIAL PRIMARY KEY,
    at TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor_id BIGINT REFERENCES users ON DELETE SET NULL,
    action TEXT NOT NULL,
    target_user_id BIGINT REFERENCES users ON DELETE SET NULL,
    detail JSONB NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_log_at ON audit_log (at DESC);
CREATE INDEX audit_log_target ON audit_log (target_user_id, at DESC) WHERE target_user_id IS NOT NULL;
