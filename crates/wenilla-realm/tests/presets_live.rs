//! Dungeon presets against a REAL realm: builds a Deadmines group and a Blackrock Depths group
//! with the headless player, checks every character in the character database, then deletes
//! both groups and checks that every account and character is gone. Gated on the environment,
//! so CI (which has no world server) skips; on the dev box, with the local realm running:
//!
//!   TEST_SOAP_URL=http://127.0.0.1:7878/ TEST_SOAP_USER=claudegm TEST_SOAP_PASS=claudegm \
//!   TEST_MARIADB_URL=mysql://mangos:mangos@127.0.0.1:3306/classicrealmd \
//!   TEST_REALMD_HOST=127.0.0.1 TEST_MANGOSD_HOST=127.0.0.1 \
//!     cargo test -p wenilla-realm --test presets_live -- --nocapture
//!
//! The MariaDB user needs SELECT on `classiccharacters`, as the service's own has.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::MySqlPool;
use wenilla_realm::presets::{self, defs};
use wenilla_realm::{db, mangos_conf, ratelimit, realmdb, secrets, soap, AppState, Config};

struct Env {
    soap_url: String,
    soap_user: String,
    soap_pass: String,
    mariadb: String,
    realmd: String,
    mangosd: String,
}

fn env() -> Option<Env> {
    let v = |k: &str| std::env::var(k).ok();
    Some(Env {
        soap_url: v("TEST_SOAP_URL")?,
        soap_user: v("TEST_SOAP_USER")?,
        soap_pass: v("TEST_SOAP_PASS")?,
        mariadb: v("TEST_MARIADB_URL")?,
        realmd: v("TEST_REALMD_HOST")?,
        mangosd: v("TEST_MANGOSD_HOST").or_else(|| v("TEST_REALMD_HOST"))?,
    })
}

async fn state(e: &Env, dir: &std::path::Path) -> Arc<AppState> {
    let mut cfg = Config::from_env().unwrap();
    cfg.state_dir = dir.to_path_buf();
    cfg.config_dir = dir.to_path_buf();
    let sqlite = db::open_sqlite(&dir.join("realm.sqlite")).await.unwrap();
    // Game account names derive from web user ids (WR000001…): start this throwaway database's
    // ids far above any a real service on this box has handed out, so no existing account is
    // taken over.
    let base: i64 = 800_000 + (rand_u16() as i64) * 10;
    sqlx::query("INSERT INTO users (id, username, display_name, role, created_at) VALUES (?, 'placeholder', 'placeholder', 'player', 0)")
        .bind(base)
        .execute(&sqlite)
        .await
        .unwrap();
    let realmdb = realmdb::connect(&e.mariadb).await.unwrap();
    Arc::new(AppState {
        setup_cache: Default::default(),
        realmdb: realmdb.clone(),
        soap: soap::Client::new(&e.soap_url, &e.soap_user, &e.soap_pass),
        conf: mangos_conf::ConfFiles::in_dir(dir),
        secrets: secrets::Keyring::load_or_create(dir).unwrap(),
        providers: Vec::new(),
        limiter: ratelimit::Limiter::default(),
        provisioner: Arc::new(presets::Headless {
            servers: presets::headless::Servers {
                realmd: e.realmd.clone(),
                mangosd: e.mangosd.clone(),
            },
            realmdb: realmdb.clone(),
        }),
        client_data_error: None,
        db: sqlite,
        cfg,
    })
}

fn rand_u16() -> u16 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u16
}

#[derive(sqlx::FromRow, Debug)]
struct CharRow {
    guid: i64,
    level: i64,
    class: i64,
    map: i64,
    x: f64,
    y: f64,
    gmlevel: i64,
}

async fn char_row(db: &MySqlPool, name: &str) -> Option<CharRow> {
    sqlx::query_as(
        "SELECT CAST(c.guid AS SIGNED) AS guid, CAST(c.level AS SIGNED) AS level, \
         CAST(c.class AS SIGNED) AS class, CAST(c.map AS SIGNED) AS map, \
         CAST(c.position_x AS DOUBLE) AS x, CAST(c.position_y AS DOUBLE) AS y, \
         CAST(a.gmlevel AS SIGNED) AS gmlevel \
         FROM classiccharacters.characters c JOIN classicrealmd.account a ON a.id = c.account \
         WHERE c.name = ?",
    )
    .bind(name)
    .fetch_optional(db)
    .await
    .unwrap()
}

/// Template entries in equipment slots (bag 0, slots 0..19).
async fn equipped(db: &MySqlPool, guid: i64) -> Vec<i64> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT CAST(item_template AS SIGNED) FROM classiccharacters.character_inventory \
         WHERE guid = ? AND bag = 0 AND slot < 19",
    )
    .bind(guid)
    .fetch_all(db)
    .await
    .unwrap();
    rows.into_iter().map(|r| r.0).collect()
}

/// Bags in bag slots 19..23.
async fn bags_worn(db: &MySqlPool, guid: i64) -> usize {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT CAST(item_template AS SIGNED) FROM classiccharacters.character_inventory \
         WHERE guid = ? AND bag = 0 AND slot BETWEEN 19 AND 22",
    )
    .bind(guid)
    .fetch_all(db)
    .await
    .unwrap();
    rows.len()
}

/// Whether the character knows `spell` or another rank of it. A talent ability's trainer ranks
/// replace its first rank (the talent itself) in the saved spell book, so a rank-3 Shield Slam
/// is the Shield Slam talent. Names come from the world DB (the dev box's `mangos` user reads it).
async fn knows(db: &MySqlPool, guid: i64, spell: u32) -> bool {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM classiccharacters.character_spell cs \
         JOIN classicmangos.spell_template s ON s.Id = cs.spell \
         WHERE cs.guid = ? AND s.SpellName = \
           (SELECT SpellName FROM classicmangos.spell_template WHERE Id = ?) LIMIT 1",
    )
    .bind(guid)
    .bind(spell)
    .fetch_optional(db)
    .await
    .unwrap();
    row.is_some()
}

async fn skills(db: &MySqlPool, guid: i64) -> Vec<i64> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT CAST(skill AS SIGNED) FROM classiccharacters.character_skills WHERE guid = ?",
    )
    .bind(guid)
    .fetch_all(db)
    .await
    .unwrap();
    rows.into_iter().map(|r| r.0).collect()
}

/// The skill line a proficiency spell grants. The server keeps proficiencies as skills (and
/// re-derives the spells from them at login), so the skill is what the gear is checked against.
fn skill_of(proficiency: u32) -> i64 {
    match proficiency {
        196 => 44,
        197 => 172,
        198 => 54,
        199 => 160,
        200 => 229,
        201 => 43,
        202 => 55,
        227 => 136,
        264 => 45,
        266 => 46,
        1180 => 173,
        2567 => 176,
        5011 => 226,
        5009 => 228,
        15590 => 473,
        9116 => 433,
        750 => 293,
        8737 => 413,
        674 => 118,
        p => panic!("no skill line known for proficiency {p}"),
    }
}

/// The outdoor `game_tele` point each preset lands on, from the world DB's rows.
/// Where a preset's `game_tele` point is, from the world DB.
async fn entrance(db: &MySqlPool, tele: &str) -> (i64, f64, f64) {
    sqlx::query_as(
        "SELECT CAST(map AS SIGNED), CAST(position_x AS DOUBLE), CAST(position_y AS DOUBLE) \
         FROM classicmangos.game_tele WHERE name = ?",
    )
    .bind(tele)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn quest_rewarded(db: &MySqlPool, guid: i64, quest: u32) -> bool {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT CAST(rewarded AS SIGNED) FROM classiccharacters.character_queststatus \
         WHERE guid = ? AND quest = ?",
    )
    .bind(guid)
    .bind(quest)
    .fetch_optional(db)
    .await
    .unwrap();
    row.is_some_and(|(r,)| r == 1)
}

async fn wait_built(st: &AppState, group: i64) -> presets::Group {
    // A raid builds a few characters at a time: allow it the better part of a quarter hour.
    let deadline = Instant::now() + Duration::from_secs(900);
    loop {
        let g = presets::by_id(&st.db, group).await.unwrap().unwrap();
        if g.status != "building" {
            return g;
        }
        assert!(
            Instant::now() < deadline,
            "group {group} still building after 15 minutes"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn presets_build_real_characters_and_delete_them() {
    let Some(e) = env() else {
        eprintln!("TEST_SOAP_URL/TEST_MARIADB_URL/TEST_REALMD_HOST not set — skipping the live preset test");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let st = state(&e, dir.path()).await;
    let only = std::env::var("TEST_PRESET").ok();

    let mut groups = Vec::new();
    for p in defs::all() {
        // TEST_PRESET=deadmines,brd builds just those.
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|id| id.trim() == p.id))
        {
            continue;
        }
        let (id, token) = presets::create(&st, p, None).await.unwrap();
        println!("{}: group {id}, link /g/{token}", p.id);
        groups.push((p, id));
    }

    let mut problems = Vec::new();
    for (p, id) in &groups {
        let g = wait_built(&st, *id).await;
        let members = presets::members(&st.db, *id).await.unwrap();
        for m in &members {
            let slot = &p.slots[m.slot as usize];
            let at = format!("{}/{}", p.id, slot.label);
            println!("{at}: {} {:?} {:?}", m.status, m.char_name, m.detail);
            if m.status != "ready" {
                problems.push(format!("{at}: {} — {:?}", m.status, m.detail));
                continue;
            }
            let name = m.char_name.as_deref().unwrap();
            let c = char_row(&st.realmdb, name).await.expect("character row");
            let (map, x, y) = entrance(&st.realmdb, &p.tele).await;
            for t in &p.turn_ins {
                if !quest_rewarded(&st.realmdb, c.guid, t.quest).await {
                    problems.push(format!("{at}: quest {} not turned in", t.quest));
                }
            }
            if c.level != i64::from(p.level) {
                problems.push(format!("{at}: level {} (want {})", c.level, p.level));
            }
            if c.class != i64::from(slot.class) {
                problems.push(format!("{at}: class {}", c.class));
            }
            let dist = ((c.x - x).powi(2) + (c.y - y).powi(2)).sqrt();
            if c.map != map || dist > 15.0 {
                problems.push(format!(
                    "{at}: at map {} ({:.1}, {:.1}), {dist:.1} yd from the entrance",
                    c.map, c.x, c.y
                ));
            }
            if c.gmlevel != 0 {
                problems.push(format!(
                    "{at}: account gmlevel {} after the build",
                    c.gmlevel
                ));
            }
            let worn = equipped(&st.realmdb, c.guid).await;
            let missing: Vec<u32> = slot
                .gear
                .iter()
                .copied()
                .filter(|item| !worn.contains(&i64::from(*item)))
                .collect();
            if !missing.is_empty() {
                problems.push(format!("{at}: not wearing {missing:?}"));
            }
            // A hunter's starting quiver fills one bag slot before the preset's bags arrive.
            let bags = bags_worn(&st.realmdb, c.guid).await;
            if bags < slot.bags.len() {
                problems.push(format!("{at}: {bags} bags worn, want {}", slot.bags.len()));
            }
            let mut unlearned = Vec::new();
            for t in &slot.talents {
                if !knows(&st.realmdb, c.guid, *t).await {
                    unlearned.push(*t);
                }
            }
            if !unlearned.is_empty() {
                problems.push(format!("{at}: talents not learned: {unlearned:?}"));
            }
            let has = skills(&st.realmdb, c.guid).await;
            let unskilled: Vec<u32> = slot
                .proficiencies
                .iter()
                .copied()
                .filter(|p| !has.contains(&skill_of(*p)))
                .collect();
            if !unskilled.is_empty() {
                problems.push(format!(
                    "{at}: proficiencies without their skill: {unskilled:?}"
                ));
            }
        }
        println!("{}: group status {}", p.id, g.status);
    }

    // TEST_KEEP=1 leaves the groups standing (their links printed above) to look at them in the
    // game or the database; delete them from the link page or `account delete WR…`.
    if std::env::var("TEST_KEEP").is_ok_and(|v| v == "1") {
        assert!(problems.is_empty(), "problems:\n{}", problems.join("\n"));
        return;
    }
    // Delete everything, whatever the checks said, then prove it is gone.
    let mut names = Vec::new();
    for (_, id) in &groups {
        for m in presets::members(&st.db, *id).await.unwrap() {
            names.extend(m.char_name);
        }
        let g = presets::by_id(&st.db, *id).await.unwrap().unwrap();
        presets::delete(&st, &g).await.unwrap();
        assert!(presets::by_id(&st.db, *id).await.unwrap().is_none());
    }
    // `account delete` removes the characters; give the world server a moment.
    tokio::time::sleep(Duration::from_secs(3)).await;
    for n in &names {
        if char_row(&st.realmdb, n).await.is_some() {
            problems.push(format!("{n} still exists after the group was deleted"));
        }
    }
    let left: Vec<(String,)> =
        sqlx::query_as("SELECT username FROM users WHERE username LIKE 'preset-%'")
            .fetch_all(&st.db)
            .await
            .unwrap();
    assert!(left.is_empty(), "preset users left behind: {left:?}");

    assert!(problems.is_empty(), "problems:\n{}", problems.join("\n"));
}

/// A dev tool, not a check: say GM commands as an existing account's character and print what the
/// server answered, then what a player would find. Skipped unless pointed at one:
///
///   TEST_SAY_ACCOUNT=WP… TEST_SAY_PASSWORD=… TEST_SAY='.learn 23922;.gm' TEST_REALMD_HOST=127.0.0.1 \
///     cargo test -p wenilla-realm --test presets_live say_as -- --nocapture
///
/// The account needs GM for dot-commands (`account set gmlevel <acct> 3 -1` over SOAP).
#[test]
fn say_as() {
    let v = |k: &str| std::env::var(k).ok();
    let (Some(account), Some(password), Some(realmd)) = (
        v("TEST_SAY_ACCOUNT"),
        v("TEST_SAY_PASSWORD"),
        v("TEST_REALMD_HOST"),
    ) else {
        eprintln!("TEST_SAY_ACCOUNT/TEST_SAY_PASSWORD/TEST_REALMD_HOST not set — skipping");
        return;
    };
    let servers = presets::headless::Servers {
        mangosd: v("TEST_MANGOSD_HOST").unwrap_or_else(|| realmd.clone()),
        realmd,
    };
    let lines: Vec<String> = v("TEST_SAY")
        .unwrap_or_default()
        .split(';')
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();
    if !lines.is_empty() {
        for l in presets::headless::say(&servers, &account, &password, &lines).unwrap() {
            println!("server: {l}");
        }
    }
    let i = presets::headless::inspect(&servers, &account, &password).unwrap();
    let mut spells: Vec<u32> = i.spells.into_iter().collect();
    spells.sort();
    println!(
        "level {}, {} free talent points, worn {:?}\nspells {spells:?}",
        i.level, i.free_talent_points, i.worn
    );
}

/// A dev tool, not a check: build one preset slot on a fresh account exactly as a group build
/// does, but stop before the check login, and print the account — to look at a character in the
/// state the builder leaves it. Needs the TEST_SOAP_* and TEST_MARIADB_URL variables as well:
///
///   TEST_BUILD=ubrs:2 TEST_SOAP_URL=… TEST_SOAP_USER=… TEST_SOAP_PASS=… TEST_REALMD_HOST=127.0.0.1 \
///     cargo test -p wenilla-realm --test presets_live build_one -- --nocapture
#[tokio::test(flavor = "multi_thread")]
async fn build_one() {
    let (Some(e), Some(which)) = (env(), std::env::var("TEST_BUILD").ok()) else {
        eprintln!("TEST_BUILD and the live variables not set — skipping");
        return;
    };
    let (id, slot) = which.split_once(':').expect("TEST_BUILD=<preset>:<slot>");
    let p = defs::get(id).expect("preset");
    let slot = p.slots[slot.parse::<usize>().unwrap()].clone();
    let soap = soap::Client::new(&e.soap_url, &e.soap_user, &e.soap_pass);
    let account = format!("WPT{}", rand_u16());
    let password = "TESTPW12".to_string();
    soap.exec(&format!("account create {account} {password}"))
        .await
        .unwrap();
    soap.exec(&format!("account set gmlevel {account} 3 -1"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let servers = presets::headless::Servers {
        realmd: e.realmd.clone(),
        mangosd: e.mangosd.clone(),
    };
    let names = presets::names(slot.race, 8);
    let (a, pw) = (account.clone(), password.clone());
    let built = tokio::task::spawn_blocking(move || {
        presets::headless::build(
            &servers,
            &presets::headless::Job {
                account: &a,
                password: &pw,
                slot: &slot,
                level: p.level,
                tele: &p.tele,
                money: p.money,
                names: &names,
                turn_ins: &p.turn_ins,
            },
            &mut |m| println!("build: {m}"),
        )
    })
    .await
    .unwrap()
    .unwrap();
    soap.exec(&format!("account set gmlevel {account} 0 -1"))
        .await
        .unwrap();
    println!("built {} on {account} / {password}", built.name);
}
