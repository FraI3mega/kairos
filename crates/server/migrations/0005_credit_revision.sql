-- A server rebuilds everyone below its own credit revision when it starts.
ALTER TABLE users ADD COLUMN credit_revision INT NOT NULL DEFAULT 0;
