//! Dev helper: seed an account with one character of every vanilla class (`CMSG_CHAR_CREATE` over
//! a live session). Idempotent by **class** — classes the account already has are skipped — so it
//! tops an account up to the full nine. Race per class is a fixed spread that covers all eight
//! races with valid 1.12 pairs; appearance fields ride as zeros (the first options).
//!
//! Both factions on one account needs `AllowTwoSide.Accounts = 1` in mangosd.conf — otherwise the
//! Horde creates (Shaman lives Horde-side) come back `CHAR_CREATE_PVP_TEAMS_VIOLATION` (0x33).
//!
//! ```text
//! cargo run -p benilla-protocol --example seed_classes -- <user> <pass> <name-prefix>
//! # e.g. … -- one pone One   → Onepaladin, Onehunter, …
//! ```

use benilla_protocol::{logon, messages, WorldSession, WORLD_PORT};

/// (class id, class name, race id, race name, gender): the full vanilla class list with an
/// all-eight-races spread of valid pairs.
const ROSTER: [(u8, &str, u8, &str, u8); 9] = [
    (1, "warrior", 1, "Human", 0),
    (2, "paladin", 3, "Dwarf", 0),
    (3, "hunter", 4, "Night Elf", 1),
    (4, "rogue", 7, "Gnome", 0),
    (5, "priest", 8, "Troll", 1),
    (7, "shaman", 2, "Orc", 0),
    (8, "mage", 5, "Undead", 1),
    (9, "warlock", 1, "Human", 1),
    (11, "druid", 6, "Tauren", 0),
];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [user, pass, prefix] = &args[1..] else {
        anyhow::bail!("usage: seed_classes <user> <pass> <name-prefix>");
    };

    let l = logon("localhost", user, pass)?;
    let addr = l
        .realms
        .first()
        .map(|r| r.address.clone())
        .unwrap_or_else(|| format!("localhost:{WORLD_PORT}"));
    let mut s = WorldSession::connect(&addr, user, l.session_key)?;

    let have = s.char_enum()?;
    println!("account {user}: {} existing character(s)", have.len());
    for (class, class_name, race, race_name, gender) in ROSTER {
        if have.iter().any(|c| c.class == class) {
            println!("  {class_name}: already present — skipped");
            continue;
        }
        let name = format!("{prefix}{class_name}");
        let req = messages::CharCreateReq {
            name,
            race,
            class,
            gender,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        let name = &req.name;
        match s.create_character(&req)? {
            messages::CHAR_CREATE_SUCCESS => println!("  {class_name}: created {name} ({race_name})"),
            messages::CHAR_CREATE_NAME_IN_USE => println!("  {class_name}: name {name} in use — skipped"),
            0x33 => anyhow::bail!(
                "{name}: CHAR_CREATE_PVP_TEAMS_VIOLATION — set AllowTwoSide.Accounts = 1 (see module doc)"
            ),
            other => anyhow::bail!("{name}: create failed, result {other:#x}"),
        }
    }

    println!("final roster:");
    for c in s.char_enum()? {
        println!(
            "  {} — class {} race {} gender {} level {}",
            c.name, c.class, c.race, c.gender, c.level
        );
    }
    Ok(())
}
