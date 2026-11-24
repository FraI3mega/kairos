-- A browser can ask for the same sign-in callback twice and keep only the second answer (a double click on the
-- provider's button). A finished flow stays for a minute with the user it signed in, so the repeat signs them in too.
ALTER TABLE auth_flows ADD COLUMN user_id BIGINT REFERENCES users ON DELETE CASCADE;
