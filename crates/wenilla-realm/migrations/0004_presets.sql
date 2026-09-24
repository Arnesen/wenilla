-- Dungeon presets: a group of ready-made characters behind one secret link. Each slot is an
-- ordinary player user (no local credential: nobody signs in as it, the link mints its sessions)
-- with the usual hidden game account.
CREATE TABLE preset_groups (
  id INTEGER PRIMARY KEY,
  preset TEXT NOT NULL,
  -- SHA-256 of the link's token, for the lookup; the token itself encrypted, so the admin panel
  -- can show the link again.
  token_hash BLOB NOT NULL UNIQUE,
  token_enc BLOB NOT NULL,
  token_nonce BLOB NOT NULL,
  -- building | ready | partial | failed
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  created_by INTEGER REFERENCES users(id) ON DELETE SET NULL
);

CREATE TABLE preset_members (
  group_id INTEGER NOT NULL REFERENCES preset_groups(id) ON DELETE CASCADE,
  slot INTEGER NOT NULL,
  user_id INTEGER NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  char_name TEXT,
  -- pending | building | ready | failed
  status TEXT NOT NULL,
  -- Why it failed, or what did not equip.
  detail TEXT,
  PRIMARY KEY (group_id, slot)
);
