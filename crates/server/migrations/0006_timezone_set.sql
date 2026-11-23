-- Until it is picked, /me sets the timezone from the browser once; afterwards a different zone only shows a hint.
ALTER TABLE users ADD COLUMN timezone_set BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE users SET timezone_set = TRUE WHERE timezone <> 'UTC';
