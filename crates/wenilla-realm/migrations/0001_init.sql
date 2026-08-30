-- wenilla-realm service state. The game itself lives in MariaDB (classicrealmd/
-- classiccharacters); this file is only the web side: who may log in, sessions, the hidden game
-- credentials, and the audit trail.
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  username TEXT NOT NULL UNIQUE COLLATE NOCASE,
  display_name TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('admin', 'player')),
  created_at INTEGER NOT NULL,
  disabled INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE local_credentials (
  user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  password_hash TEXT NOT NULL,
  must_change INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

-- Reserved for M2 (Discord and other providers): one row per external identity.
CREATE TABLE identities (
  id INTEGER PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL,
  subject TEXT NOT NULL,
  display_name TEXT,
  email TEXT,
  created_at INTEGER NOT NULL,
  last_login INTEGER,
  UNIQUE (provider, subject)
);

CREATE TABLE sessions (
  id INTEGER PRIMARY KEY,
  token_hash BLOB NOT NULL UNIQUE,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  csrf_token TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  rotated_at INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  ip TEXT,
  user_agent TEXT
);
CREATE INDEX sessions_user ON sessions(user_id);

CREATE TABLE game_accounts (
  user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  game_username TEXT NOT NULL UNIQUE,
  password_enc BLOB NOT NULL,
  nonce BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  rotated_at INTEGER
);

CREATE TABLE bans (
  id INTEGER PRIMARY KEY,
  game_username TEXT NOT NULL,
  by_user_id INTEGER,
  reason TEXT NOT NULL,
  duration_secs INTEGER,
  created_at INTEGER NOT NULL,
  lifted_at INTEGER
);

CREATE TABLE login_attempts (
  id INTEGER PRIMARY KEY,
  ip TEXT,
  username TEXT,
  ok INTEGER NOT NULL,
  at INTEGER NOT NULL
);
CREATE INDEX login_attempts_at ON login_attempts(at);

CREATE TABLE audit (
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  actor_user_id INTEGER,
  ip TEXT,
  action TEXT NOT NULL,
  target TEXT,
  detail TEXT
);
CREATE INDEX audit_at ON audit(at);
