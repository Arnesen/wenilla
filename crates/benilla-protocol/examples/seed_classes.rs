//! Dev helper: put an account into the standard **full class set** — one character of every vanilla
//! class, optionally levelled, geared, specced and parked in its own capital. Headless: a bare
//! `WorldSession`, no Bevy app and no window, so one process dresses a whole nine-body account.
//!
//! ```text
//! cargo run -p benilla-protocol --example seed_classes -- <user> <pass> <Prefix> [flags]
//!   --wipe            delete EVERY existing character on the account first (see the warning below)
//!   --level <n>       `.character level n` + `.learn all_myclass` (all class spells + talents)
//!   --tier <t>        `.character premade gear <role>-<t>` + `.character premade spec <spec>`;
//!                     `<t>` is `phase6-bis` (Naxx BiS), `preraid-bis`, or `r14` (rank-14 PvP
//!                     kit, custom templates 901–909 — decision 0825), role is per class below
//!   --home            `.tele` each body to its own race's capital
//!   --spread <a|b>    which race spread to use (default `a`)
//!   --reload-templates  make the server re-read the premade tables before dressing (see below)
//!   --host <h>        default `localhost`
//!
//! # the director's two accounts, rebuilt as geared 60s:
//! … -- one pone One --wipe --level 60 --tier phase6-bis --home --spread a
//! … -- two ptwo Two --wipe --level 60 --tier phase6-bis --home --spread b
//! ```
//!
//! Without `--wipe` it is idempotent **by class** — classes the account already has are skipped — so
//! it tops an account up to the full nine. Creating characters needs no GM level; the state flags do,
//! and that is where this silently half-applies if the account is short: **`.character premade
//! gear|spec` needs gmlevel 4 and `--level`'s two commands need 5**, and that level lives in
//! `realmd.account_access`, *not* `account.gmlevel` (see method.md, "The local vmangos server").
//! Every server reply is echoed as `server says — …`, so a refusal is visible rather than silent.
//!
//! **`--wipe` deletes characters irreversibly, hand-made ones included.** It is deliberately not the
//! default, and it names each body and its level as it goes. Both factions on one account needs
//! `AllowTwoSide.Accounts = 1` in mangosd.conf — otherwise the Horde creates (shaman is Horde-only)
//! come back `CHAR_CREATE_PVP_TEAMS_VIOLATION` (0x33).
//!
//! Spell counts do **not** match across two accounts' same-class bodies, and that is not a
//! half-applied `.learn all_myclass`: an Alliance mage ends on 68 spells and a Horde one on 74,
//! because the command grants every spell the class *can* learn and the Alliance city
//! teleports/portals carry no gate that stops a Horde mage — so the Horde body collects all twelve
//! while the Alliance body gets only its own six. Each still has its own faction's full set.
//!
//! **Creating is idempotent; dressing is NOT.** `.character premade gear` *adds* a fresh copy of the
//! set every time it runs — it does not diff against what is worn. Dress an already-dressed body and
//! the second set lands in its bags, and any slot whose first-pass item is still equipped keeps the
//! duplicate in the backpack instead: observed as a warrior left with an empty finger2 and ten spare
//! epics, because both rings wanted finger1. So re-dress only behind `--wipe`; to fix a body that was
//! dressed twice, wipe and rebuild it rather than unpicking the bags.
//!
//! **`--reload-templates`:** the world server caches the premade tables at startup and has no
//! reload command, so a template edited in the DB is invisible to `.character premade gear` until
//! the next restart. `.character premade savegear <name>` re-reads them all as a side effect
//! (`CharacterCommands.cpp` → `LoadPlayerPremadeTemplates`; decision 0818's in-band trick), so this
//! flag sends `.character premade savegear zz-seed-reload` as the first command of the first body
//! dressed. The junk template it saves (the naked just-created body) is inert — no role a bot would
//! pick, self-naming, safe to leave or delete from `player_premade_item_template` later.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::{logon, messages, ServerPacket, WorldSession, WORLD_PORT};

/// `CHAT_MSG_SYSTEM` on the inbound `SMSG_MESSAGECHAT` type byte — every dot-command's
/// success/refusal text arrives as one of these.
const CHAT_MSG_SYSTEM: u8 = 0x0a;

/// Per class: the premade templates that make a level-60 body of that class make sense. `role` joins
/// the `--tier` (`dps` + `phase6-bis` → `dps-phase6-bis`, a `player_premade_item_template.name`);
/// `spec` is a verbatim `player_premade_spell_template.name` (specs carry no tier suffix — there is
/// one per role at 60). Every name here was read off this deploy's world DB, and the roles spread
/// plate/mail/cloth and tank/heal/dps across the nine rather than making nine damage dealers.
const CLASSES: [(u8, &str, &str, &str); 9] = [
    // class, name, gear role, spec template
    (1, "warrior", "dps", "fury-dw-pve"),
    (2, "paladin", "tank", "protection-pve"),
    (3, "hunter", "dps", "mm-sv-pve"),
    (4, "rogue", "dps", "combat-swords-pve"),
    (5, "priest", "heal", "holy-pve"),
    (7, "shaman", "heal", "resto-pve"),
    (8, "mage", "dps", "arcane-power-frost-pve"),
    (9, "warlock", "dps", "ds-ruin-pve"),
    (11, "druid", "tank", "feral-bear-pve"),
];

/// `(race, gender)` per class, indexed to match [`CLASSES`]. Two spreads exist so two accounts can
/// hold **eighteen distinct** race/class/gender bodies instead of the same nine twice; each spread on
/// its own still covers all eight races, and every pair is a legal 1.12 combination (paladin is
/// Human/Dwarf only, shaman Orc/Tauren/Troll, druid Night Elf/Tauren).
const SPREAD_A: [(u8, u8); 9] = [
    (1, 0), // human warrior
    (3, 0), // dwarf paladin
    (4, 1), // night elf hunter
    (7, 0), // gnome rogue
    (8, 1), // troll priest
    (2, 0), // orc shaman
    (5, 1), // undead mage
    (1, 1), // human warlock
    (6, 0), // tauren druid
];

/// The second spread — no race/class pair shared with [`SPREAD_A`].
const SPREAD_B: [(u8, u8); 9] = [
    (3, 0), // dwarf warrior
    (1, 0), // human paladin
    (6, 0), // tauren hunter
    (2, 0), // orc rogue
    (5, 1), // undead priest
    (8, 1), // troll shaman
    (7, 1), // gnome mage
    (5, 0), // undead warlock
    (4, 1), // night elf druid
];

/// Race id → (display name, the `game_tele` name of its capital). Gnomes ride with the dwarves:
/// Gnomeregan is a dungeon, and `game_tele` has no gnome capital row.
fn race_info(id: u8) -> Option<(&'static str, &'static str)> {
    Some(match id {
        1 => ("Human", "Stormwind"),
        2 => ("Orc", "Orgrimmar"),
        3 => ("Dwarf", "Ironforge"),
        4 => ("Night Elf", "Darnassus"),
        5 => ("Undead", "Undercity"),
        6 => ("Tauren", "ThunderBluff"),
        7 => ("Gnome", "Ironforge"),
        8 => ("Troll", "Orgrimmar"),
        _ => return None,
    })
}

struct Opts {
    user: String,
    pass: String,
    prefix: String,
    host: String,
    wipe: bool,
    level: Option<u8>,
    tier: Option<String>,
    home: bool,
    reload: bool,
    spread: [(u8, u8); 9],
}

const USAGE: &str = "usage: seed_classes <user> <pass> <Prefix> [--wipe] [--level <n>] \
                     [--tier <phase6-bis|preraid-bis|r14>] [--home] [--spread <a|b>] \
                     [--reload-templates] [--host <h>]";

fn parse_opts() -> Result<Opts> {
    let mut a = std::env::args().skip(1);
    let (user, pass, prefix) = (
        a.next().context(USAGE)?,
        a.next().context(USAGE)?,
        a.next().context(USAGE)?,
    );
    let mut o = Opts {
        user,
        pass,
        prefix,
        host: "localhost".into(),
        wipe: false,
        level: None,
        tier: None,
        home: false,
        reload: false,
        spread: SPREAD_A,
    };
    while let Some(flag) = a.next() {
        match flag.as_str() {
            "--wipe" => o.wipe = true,
            "--home" => o.home = true,
            "--reload-templates" => o.reload = true,
            "--level" => o.level = Some(a.next().context("--level needs a number")?.parse()?),
            "--tier" => o.tier = Some(a.next().context("--tier needs a name")?),
            "--host" => o.host = a.next().context("--host needs a hostname")?,
            "--spread" => {
                o.spread = match a.next().context("--spread needs a|b")?.as_str() {
                    "a" | "A" => SPREAD_A,
                    "b" | "B" => SPREAD_B,
                    other => bail!("unknown spread {other:?} — expected a or b"),
                }
            }
            other => bail!("unknown flag {other:?}\n{USAGE}"),
        }
    }
    Ok(o)
}

fn connect(o: &Opts) -> Result<WorldSession> {
    let l = logon(&o.host, &o.user, &o.pass)?;
    let addr = l
        .realms
        .first()
        .map(|r| r.address.clone())
        .unwrap_or_else(|| format!("{}:{WORLD_PORT}", o.host));
    WorldSession::connect(&addr, &o.user, l.session_key)
}

/// Read inbound packets for `window`, echoing every system-chat line — the server's own
/// success/refusal text for a dot-command, and the only thing separating a command that worked from
/// one that was silently refused. A read-timeout tick is the expected quiet case rather than an
/// error (the convention [`WorldSession::logout`]'s own poll loop uses), so draining runs to the
/// deadline either way.
fn drain(s: &mut WorldSession, window: Duration) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        if let Ok(ServerPacket::MessageChat(m)) = s.recv() {
            if m.chat_type == CHAT_MSG_SYSTEM {
                println!("      server says — {}", m.text);
            }
        }
    }
}

fn main() -> Result<()> {
    let o = parse_opts()?;
    let mut s = connect(&o)?;

    // ── 1 · wipe ──
    // Blocking reads for the whole character-select phase: char_enum/create/delete each loop on
    // `recv` until their own reply lands, so a read timeout here surfaces as a hard error.
    s.set_read_timeout(None)?;
    let have = s.char_enum()?;
    println!("account {}: {} existing character(s)", o.user, have.len());
    if o.wipe {
        for c in &have {
            match s.delete_character(c.guid)? {
                messages::CHAR_DELETE_SUCCESS => {
                    println!("  wiped {} (guid {}, level {})", c.name, c.guid, c.level);
                }
                other => bail!("deleting {} failed: {other:#x}", c.name),
            }
        }
    }

    // ── 2 · create the nine ──
    let have = s.char_enum()?;
    for (i, (class, class_name, _, _)) in CLASSES.iter().enumerate() {
        let (race, gender) = o.spread[i];
        let (race_name, _) = race_info(race).context("spread holds a known race")?;
        if have.iter().any(|c| c.class == *class) {
            println!("  {class_name}: already present — skipped");
            continue;
        }
        let name = format!("{}{class_name}", o.prefix);
        let req = messages::CharCreateReq {
            name: name.clone(),
            race,
            class: *class,
            gender,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        match s.create_character(&req)? {
            messages::CHAR_CREATE_SUCCESS => {
                let sex = if gender == 1 { "female" } else { "male" };
                println!("  {class_name}: created {name} ({sex} {race_name})");
            }
            messages::CHAR_CREATE_NAME_IN_USE => {
                println!("  {class_name}: name {name} already in use — skipped");
            }
            0x33 => bail!(
                "{name}: CHAR_CREATE_PVP_TEAMS_VIOLATION — set AllowTwoSide.Accounts = 1 \
                 (see module doc)"
            ),
            other => bail!("{name}: create failed, result {other:#x}"),
        }
    }

    // ── 3 · dress each body ──
    // The re-enum is load-bearing, not a formality: `player_login` takes each character's chat
    // tongue from the roster it last saw, and vmangos DROPS chat — dot-commands included — spoken in
    // a language the character doesn't know. Skip it and every Horde body's commands vanish
    // silently (decision 0392).
    let roster = s.char_enum()?;
    let mut dressed = 0usize;
    if o.level.is_some() || o.tier.is_some() || o.home {
        for (i, (class, class_name, role, spec)) in CLASSES.iter().enumerate() {
            let Some(c) = roster.iter().find(|c| c.class == *class) else {
                println!("  {class_name}: not on the account — nothing to dress");
                continue;
            };
            let (_, capital) = race_info(o.spread[i].0).context("spread holds a known race")?;
            let mut steps = Vec::new();
            if o.reload && dressed == 0 {
                // Must run before the first `.character premade gear` — the server's template
                // cache predates any DB edit this seeding is meant to pick up (see module doc).
                steps.push(".character premade savegear zz-seed-reload".into());
            }
            if let Some(l) = o.level {
                // Level before gear (a premade template only levels *up*), spells after the level
                // they belong to, and the premade spec after `all_myclass` so a real talent tree
                // wins over its every-talent state — the order the rig established (decision 0651).
                steps.push(format!(".character level {l}"));
                steps.push(".learn all_myclass".into());
            }
            if let Some(t) = &o.tier {
                steps.push(format!(".character premade gear {role}-{t}"));
                steps.push(format!(".character premade spec {spec}"));
            }
            if o.home {
                steps.push(format!(".tele {capital}"));
            }

            println!("  {}: entering world…", c.name);
            s.player_login(c.guid)?;
            s.set_active_mover(c.guid)?;
            s.complete_cinematic()?; // a freshly created body is shown the intro cinematic
            s.set_read_timeout(Some(Duration::from_millis(250)))?;
            drain(&mut s, Duration::from_secs(2)); // a command into a half-built session is dropped
            for step in &steps {
                println!("    {step}");
                s.send_chat(step)?;
                drain(&mut s, Duration::from_millis(1200));
            }
            s.logout(Duration::from_secs(25))
                .with_context(|| format!("logging {} back out to character select", c.name))?;
            s.set_read_timeout(None)?;
            dressed += 1;
        }
    }

    // ── 4 · report ──
    println!("final roster on {}:", o.user);
    for c in s.char_enum()? {
        let (race_name, _) = race_info(c.race).unwrap_or(("?", "?"));
        println!(
            "  {:<12} level {:<3} {} {}",
            c.name,
            c.level,
            if c.gender == 1 { "female" } else { "male" },
            race_name,
        );
    }
    println!("dressed {dressed} character(s)");
    Ok(())
}
