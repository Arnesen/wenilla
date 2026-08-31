//! `realmdb` queries against a REAL cmangos database — the decode contract the mock-DB app
//! harness (tests/app.rs, dead URL + lazy pool) can never exercise. Gated on
//! `TEST_MARIADB_URL` so CI (which has no world DB) skips; on the dev box:
//!
//!   TEST_MARIADB_URL=mysql://mangos:mangos@127.0.0.1:3306/classicrealmd \
//!     cargo test -p wenilla-realm --test realmdb_live
//!
//! Born from a live bug (2026-09-01): the admin dashboard showed "no characters yet" for every
//! player. Every integer column cmangos ships is UNSIGNED (`tinyint(3) unsigned`,
//! `int(11) unsigned`), the row structs decoded into `i64`, and sqlx refuses a signedness
//! mismatch — so `characters()` errored on every account with a character, and the caller's
//! `unwrap_or_default()` dressed the error as an empty roster.

fn url() -> Option<String> {
    std::env::var("TEST_MARIADB_URL").ok()
}

#[tokio::test]
async fn characters_decode_from_a_real_cmangos_schema() {
    let Some(url) = url() else {
        eprintln!("TEST_MARIADB_URL not set — skipping the live realmdb test");
        return;
    };
    let pool = wenilla_realm::realmdb::connect(&url).await.unwrap();
    // WOWCHAT is this project's standing clientless test account; any account with at least
    // one character proves the decode. The assertion is on Ok-ness first — the bug presented
    // as Err("mismatched types"), not as a wrong count.
    let chars = wenilla_realm::realmdb::characters(&pool, "WOWCHAT")
        .await
        .expect("characters() must decode a real cmangos row");
    assert!(
        !chars.is_empty(),
        "WOWCHAT has characters in this database; an empty answer means the query lost them"
    );
    wenilla_realm::realmdb::online(&pool)
        .await
        .expect("online() must decode a real cmangos row");
    wenilla_realm::realmdb::online_count(&pool)
        .await
        .expect("online_count() must decode");
    wenilla_realm::realmdb::active_bans(&pool)
        .await
        .expect("active_bans() must decode");
}
