-- Keep in-flight requests alive briefly while exactly one response installs a new cookie.
ALTER TABLE sessions ADD COLUMN previous_token_hash BLOB;
ALTER TABLE sessions ADD COLUMN previous_valid_until INTEGER;
CREATE UNIQUE INDEX sessions_previous_token ON sessions(previous_token_hash);
