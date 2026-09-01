-- An admin-issued password is a bootstrap credential: it exists to get someone to their first
-- login, and it travels out of band — pasted into a Discord message, read out, mailed. Without
-- an expiry it stays valid in that scrollback forever, and whoever reads it first can claim the
-- account. `must_change` was never a real gate either (a holder could just navigate past the
-- redirect), so the pasted password was a permanent key to a playing account.
--
-- NULL means "never expires", which is what a password the user chose themselves gets. Existing
-- rows keep NULL deliberately: this must not retroactively lock out someone who was invited last
-- week and has not logged in yet. New bootstrap credentials get now + REALM_BOOTSTRAP_TTL_HOURS.
ALTER TABLE local_credentials ADD COLUMN expires_at INTEGER;
