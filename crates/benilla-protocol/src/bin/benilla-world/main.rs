//! `benilla-world` — Phase 4 CLI: log in to realmd, connect to the world server (`mangosd`), enter the
//! world as a character, and stream/tally the object updates the server pushes.
//!
//! Example: `cargo run -p benilla-protocol --bin benilla-world -- one pone localhost`
//!
//! If the account has no character yet, one is created (`--create <name>`, Human Warrior) so
//! `CMSG_PLAYER_LOGIN` has something to log in. The realmd host is a positional arg; the world server
//! address defaults to whatever the realm list advertises (override with `--world`).

mod probes;
mod world;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::messages::{
    CharCreateReq, CHAR_CREATE_NAME_IN_USE, CHAR_CREATE_SUCCESS, CLASS_WARRIOR, GENDER_MALE,
    RACE_HUMAN,
};
use benilla_protocol::{decode, EntityKind, WorldSession, WORLD_PORT};
use clap::Parser;

use probes::{
    Attack, Aura, Charge, Ctx, Death, EquipPackSlot, GiverStatus, Loot, Probe, QueryNames, Quest,
    QuestItem, QuestLog, Speed, Spells, Spirit, SwapPackSlots, UsePackSlot, Vendor, WorldState,
};
use world::{DeathArc, Tracked, World};

/// Connect to a WoW 1.12.1 world server, log in a character, and stream object updates.
#[derive(Parser)]
#[command(name = "benilla-world", version, about)]
struct Cli {
    /// Account name.
    username: String,
    /// Account password.
    password: String,
    /// Auth (realmd) server host.
    #[arg(default_value = "localhost")]
    host: String,
    /// World server address (`host:port`). Defaults to the realm list's advertised address.
    #[arg(long)]
    world: Option<String>,
    /// Character name to create if the account has none.
    #[arg(long, default_value = "One")]
    create: String,
    /// How many seconds to stream packets after entering the world.
    #[arg(long, default_value_t = 10)]
    seconds: u64,
    /// After streaming, walk this many yards forward, then log out and re-enum to confirm the server
    /// persisted the new position. Omit to just observe (read-only).
    #[arg(long)]
    walk: Option<f32>,
    /// Live-verify the name-query pair: ask our own name (`CMSG_NAME_QUERY`) and the first streamed
    /// creature's template name (`CMSG_CREATURE_QUERY`, entry from its guid), and require both answers
    /// to arrive and parse.
    #[arg(long)]
    query_names: bool,
    /// Live-verify the spell/action wire: require `SMSG_INITIAL_SPELLS` + `SMSG_ACTION_BUTTONS` to
    /// arrive and parse at login, then send one `CMSG_CAST_SPELL` (Battle Shout 6673 if known, else
    /// the first known spell) and require an `SMSG_CAST_RESULT` verdict (ok *or* a failure reason —
    /// either proves the round trip).
    #[arg(long)]
    spells: bool,
    /// Live-verify the melee-swing wire (decision 0073): GM-teleport (`.go xyz`) onto a Northshire
    /// Kobold Vermin spawn, `CMSG_ATTACKSWING` the nearest streamed creature, and require ≥1
    /// `SMSG_ATTACKERSTATEUPDATE` to arrive and decode (attacker/victim/hitInfo/damage). Needs a GM
    /// account (the deploy's probes are gmlevel 6).
    #[arg(long)]
    attack: bool,
    /// Live-verify `CMSG_USE_ITEM`: use the item in this 1-based backpack slot (the wire's bag
    /// 255 with player-array slot 23…) and require the server to react — a stack-count values
    /// delta or a destroy on that item's guid (consumed), or an explicit cast-result refusal.
    #[arg(long)]
    use_pack_slot: Option<u8>,
    /// Send a `/say` line right after entering the world — the GM dot-command channel
    /// (`.additem 3732`, `.repairitems`, …) for setting up probe scenarios. Repeatable; the lines go
    /// out in order (e.g. `--say ".modify money 5000000" --say ".go creature 1"`).
    #[arg(long)]
    say: Vec<String>,
    /// Live-verify `CMSG_AUTOEQUIP_ITEM`: equip the item in this 1-based backpack slot and
    /// require the server to react — a self values delta landing that item's guid in an
    /// equipment INV slot, or a decoded `SMSG_INVENTORY_CHANGE_FAILURE` refusal.
    #[arg(long)]
    equip_pack_slot: Option<u8>,
    /// Live-verify `CMSG_SWAP_INV_ITEM` (the backpack pick/place/swap wire): swap the two 1-based
    /// backpack slots `A:B` (the wire's bag 255, player-array slots 23+A-1 / 23+B-1), await the
    /// self values delta, and assert the two slots' item guids exchanged — then swap back and assert
    /// the original layout is restored (leaves the character exactly as found). Slot A must be
    /// occupied; B may be empty (an empty destination is a move on this wire).
    #[arg(long, value_parser = parse_slot_pair)]
    swap_pack_slots: Option<(u8, u8)>,
    /// Live-verify the vendor wire (decision 0081 phase 4): auto-find the nearest streamed
    /// vendor NPC (`UNIT_NPC_FLAG_VENDOR`), `CMSG_LIST_INVENTORY` it and require an
    /// `SMSG_LIST_INVENTORY` to arrive and parse (N rows), then `CMSG_BUY_ITEM` the cheapest row and
    /// require a reaction — the purchased item arriving (`ItemCreate`/values), the vendor stock
    /// updating (`SMSG_BUY_ITEM`), *or* a decoded `SMSG_BUY_FAILED` (all prove the round trip). Also
    /// reads our `PLAYER_FIELD_COINAGE` (the money accessor) at login and confirms any coinage delta
    /// lands on that field. No GM needed — it uses whatever vendor streams in range.
    #[arg(long)]
    vendor: bool,
    /// Live-verify the solo-loot wire (decision 0084 §1): select a target (nearest streamed
    /// creature within range of the `--attack` teleport spot, or `--loot-guid` if given),
    /// GM-kill it (`.damage 10000`), wait for `UNIT_DYNFLAG_LOOTABLE`, then `CMSG_LOOT` it,
    /// require `SMSG_LOOT_RESPONSE` to arrive and parse, `CMSG_AUTOSTORE_LOOT_ITEM` every row,
    /// `CMSG_LOOT_MONEY` if it carried gold, and `CMSG_LOOT_RELEASE` — requiring
    /// `SMSG_LOOT_RELEASE_RESPONSE` to close it. Prints every loot-related packet decoded. Needs a
    /// GM account (the deploy's probes are gmlevel 6).
    #[arg(long)]
    loot: bool,
    /// Skip the nearest-creature search for `--loot` and loot this guid directly (decimal or
    /// `0x`-prefixed hex).
    #[arg(long, value_parser = parse_guid)]
    loot_guid: Option<u64>,
    /// Live-verify the questgiver wire (decision 0088): GM-teleport onto Marshal McBride (Northshire,
    /// entry 197), `CMSG_GOSSIP_HELLO` him, `CMSG_QUESTGIVER_QUERY_QUEST` the target quest and require
    /// `SMSG_QUESTGIVER_QUEST_DETAILS` to parse, `CMSG_QUESTGIVER_ACCEPT_QUEST` it and confirm the
    /// quest id lands in the player descriptor's `PLAYER_QUEST_LOG` fields, GM-complete it
    /// (`.quest complete`), then `CMSG_QUESTGIVER_COMPLETE_QUEST` → `_REQUEST_REWARD` → `_CHOOSE_REWARD`
    /// requiring `SMSG_QUESTGIVER_QUEST_COMPLETE` (XP/money) + a `PLAYER_FIELD_COINAGE` delta. Uses
    /// quest 7 "Kobold Camp Cleanup" (McBride gives + takes it; XP 170, money 25c). Needs a GM
    /// account (the deploy's probes are gmlevel 6).
    #[arg(long)]
    quest: bool,
    /// Live-verify the quest-LOG wire (decision 0109 — the questgiver wire's deferred second
    /// slice): GM-teleport onto Marshal McBride (entry 197 — gives *and* takes quest 7 "Kobold
    /// Camp Cleanup", VERIFIED live against `mangos.creature_questrelation` /
    /// `creature_involvedrelation`, both rows pointing at 197; objective is 10× creature entry 6,
    /// `quest_template.ReqCreatureOrGOId1/Count1`), accept quest 7, `CMSG_QUEST_QUERY` it and
    /// require `SMSG_QUEST_QUERY_RESPONSE` to parse into the title plus a real
    /// (`required_count > 0`) objective — the fat template parser's live golden, distinct from the
    /// giver-panel `SMSG_QUESTGIVER_QUEST_DETAILS` [`Self::quest`] already exercises. Then poll the
    /// player descriptor for the `PLAYER_QUEST_LOG` slot the accept landed in, GM-complete it
    /// (`.quest complete 7`) and require the slot's count-state field to gain the `COMPLETE` state
    /// byte, then `CMSG_QUESTLOG_REMOVE_QUEST` that slot and require its id field to clear to `0`
    /// (no ack SMSG on this wire — the field clear *is* the confirmation). Needs a GM account (the
    /// deploy's probes are gmlevel 6).
    #[arg(long)]
    questlog: bool,
    /// Live-verify the quest-STARTER item wire (decision 0664): `.additem` the Northshire Gift
    /// Voucher (entry 14646, starts quest 5805 "Welcome!"), find it in the backpack, and run the
    /// fork a bag right-click makes for an item whose template carries a non-zero `StartQuest` —
    /// `CMSG_QUESTGIVER_QUERY_QUEST` addressed to the **item's own guid** (never `CMSG_USE_ITEM`,
    /// which the server refuses with `EQUIP_ERR_ITEM_NOT_FOUND`, the red "The item was not found."
    /// line) — requiring `SMSG_QUESTGIVER_QUEST_DETAILS`, then `CMSG_QUESTGIVER_ACCEPT_QUEST` on
    /// the same guid, requiring BOTH the quest id landing in `PLAYER_QUEST_LOG` and the starter
    /// item being destroyed. Cleans up after itself (`.quest remove`). Needs a GM account (the
    /// deploy's probes are gmlevel 6).
    #[arg(long)]
    quest_item: bool,
    /// Live-verify the force-speed-change wire: GM `.modify speed 1.5` (self-targeted), require
    /// `SMSG_FORCE_RUN_SPEED_CHANGE` to arrive and parse (flat speed 10.5 = 1.5 × the 7.0 base),
    /// ack it (`CMSG_FORCE_RUN_SPEED_CHANGE_ACK` echoing counter + exact speed with our live pose),
    /// then `.modify speed 1` and require a SECOND change (counter incremented, speed 7.0) on a
    /// still-live stream. A malformed ack body throws in the server's parser and drops the session
    /// (the `--charge` precedent), so survival through both round trips is the wire proof. Needs a
    /// GM account (the deploy's probes are gmlevel 6).
    #[arg(long)]
    speed: bool,

    /// Live-verify the aura wire (decision 0255 phase 1): GM-apply Mark of the Wild (1126, a
    /// cancelable buff) and Shadow Word: Pain (589, a periodic-damage DoT, unambiguously negative)
    /// to ourselves with explicit durations via `.aura`, then require — from the *live* descriptor
    /// — that both land in `UNIT_FIELD_AURA` (proving the field index), in the correct half (buffs
    /// 0–31, debuffs 32–47), with the `AURAFLAGS` cancelable nibble bit set only on the buff, the
    /// `AURALEVELS` byte equal to our own level, and a stack of 1 (the `count - 1` wire bias). Also
    /// requires an `SMSG_UPDATE_AURA_DURATION` for each aura's own slot carrying the duration we
    /// asked for, and asserts it arrived **before** the descriptor delta that names the slot (the
    /// ordering the aura model depends on). Leaves the character as found (`.unaura` both). Needs a
    /// GM account (the deploy's probes are gmlevel 6).
    #[arg(long)]
    aura: bool,

    /// Live-verify the questgiver STATUS wire (the overhead `!`/`?` markers' data):
    /// teleport onto Marshal McBride, `CMSG_QUESTGIVER_STATUS_QUERY` his guid, and require an
    /// `SMSG_QUESTGIVER_STATUS` answer for him — printing the dialog status BEFORE and AFTER a
    /// `.quest remove 7` (unaccepted quest 7 should read AVAILABLE=5). Needs a GM account.
    #[arg(long)]
    giverstatus: bool,

    /// Live-verify the world-state table's two wires (`SMSG_INIT_WORLD_STATES` /
    /// `SMSG_UPDATE_WORLD_STATE` — what the NPC-text `$<n>w`/`$<n>e` tokens read): teleport to
    /// Elwynn, hop to Stormwind to force a zone-change init, and `.debug send worldstate` one
    /// synthetic pair to require back. Prints every state received. Needs a **SEC_DEVELOPER**
    /// account: `.debug send worldstate` is gmlevel 5, under the 6 the slot-keyed probe
    /// accounts carry (decision 0450), so the update leg needs a temporary grant.
    #[arg(long)]
    worldstate: bool,

    /// Live-verify the Charge wire (warrior Charge rank 1, spell 100): GM `.learn 100`, teleport to
    /// open ground near the Northshire kobold camp ([`CHARGE_TP`]), pick a creature at charge range
    /// (8–25 yd), `CMSG_SET_SELECTION` + `CMSG_CAST_SPELL` 100 at it — then require an
    /// `SMSG_MONSTER_MOVE` addressed to **our own guid**, proving Charge drives the caster through
    /// the same server spline machinery as any creature (not a teleport / not a knockback). Prints
    /// the self spline in full (facing kind, duration, waypoints, flying bit) so the ride + the
    /// `CMSG_MOVE_SPLINE_DONE` ack it obliges can be built from the real numbers. Needs a GM warrior
    /// (any slot-keyed probe account, gmlevel 6).
    #[arg(long)]
    charge: bool,

    /// Live-verify the death-arc slice-1 wire (decision 0308): GM-kill self (`.die`), require the
    /// death signals (health force-flushed to 0, the death root), release
    /// (`CMSG_REPOP_REQUEST`), require the full ghost transition — unroot, the water-walk grant,
    /// `SMSG_CORPSE_RECLAIM_DELAY`, `PLAYER_FLAGS_GHOST`, the corpse object streaming in, the
    /// graveyard teleport — query the corpse (`MSG_CORPSE_QUERY`), then GM-revive (`.revive`) to
    /// leave the character alive. Recommend `--seconds 45`: the `.die`→repop→revive round trip
    /// itself completes in ~10s, but the reclaim delay is only ever observed as a packet value
    /// (never awaited), and the slack keeps a slow local server from truncating the ghost phase.
    /// Needs a GM account (the deploy's probes are gmlevel 6).
    #[arg(long)]
    death: bool,

    /// Live-verify the spirit-healer res + the 25% durability loss's wire (director-reported:
    /// "durability still 100% after spirit-healer rez"): GM-repair to a full baseline, die and
    /// release (the --death staging), teleport onto the graveyard's Spirit Healer, send
    /// `CMSG_SPIRIT_HEALER_ACTIVATE`, and require BOTH the res (the ghost flag clears) and the
    /// post-activate `ITEM_FIELD_DURABILITY` values-deltas the loss must push — the exact packets
    /// the app's tooltip line feeds from. Recommend `--seconds 45`. Needs a GM account; leaves the
    /// character alive and re-repaired.
    #[arg(long)]
    spirit: bool,
}

/// Parse a `--loot-guid` value: decimal, or `0x`-prefixed hex (as `benilla-world`'s own `guid
/// {:#x}` printouts read out).
fn parse_guid(s: &str) -> Result<u64, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u64>().map_err(|e| e.to_string())
    }
}

/// Parse a `--swap-pack-slots` value: two 1-based backpack slots as `A:B` (or `A,B`).
fn parse_slot_pair(s: &str) -> Result<(u8, u8), String> {
    let (a, b) = s
        .split_once([':', ','])
        .ok_or_else(|| format!("expected two slots like '1:2', got '{s}'"))?;
    let a: u8 = a.trim().parse().map_err(|e| format!("slot A: {e}"))?;
    let b: u8 = b.trim().parse().map_err(|e| format!("slot B: {e}"))?;
    if a == 0 || b == 0 || a == b {
        return Err("slots are 1-based and must differ".into());
    }
    Ok((a, b))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. realmd logon → session key + realm list (Phase 3).
    let logon = benilla_protocol::logon(&cli.host, &cli.username, &cli.password)?;
    println!("authenticated as '{}'", cli.username);

    // 2. Resolve the world address: explicit override, else the first advertised realm, else fall
    //    back to the auth host on the default world port.
    let world_addr = match (&cli.world, logon.realms.first()) {
        (Some(addr), _) => addr.clone(),
        (None, Some(realm)) => {
            println!("realm '{}' @ {}", realm.name, realm.address);
            realm.address.clone()
        }
        (None, None) => format!("{}:{}", cli.host, WORLD_PORT),
    };

    // 3. World handshake (CMSG_AUTH_SESSION + header obfuscation).
    let mut session = WorldSession::connect(&world_addr, &cli.username, logon.session_key)
        .with_context(|| format!("world handshake with {world_addr}"))?;
    println!("world handshake OK ({world_addr}) — header obfuscation active");

    // 4. Character list; create one if empty.
    let mut characters = session.char_enum()?;
    println!("{} character(s) on the account", characters.len());
    if characters.is_empty() {
        println!("creating character '{}' (Human Warrior)…", cli.create);
        let req = CharCreateReq {
            name: cli.create.clone(),
            race: RACE_HUMAN,
            class: CLASS_WARRIOR,
            gender: GENDER_MALE,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        match session.create_character(&req)? {
            CHAR_CREATE_SUCCESS | CHAR_CREATE_NAME_IN_USE => {}
            other => bail!("character creation failed: result {other:#x}"),
        }
        characters = session.char_enum()?;
        if characters.is_empty() {
            bail!("still no characters after creation");
        }
    }
    let character = &characters[0];
    println!(
        "logging in '{}' (guid {}, level {}, map {})",
        character.name, character.guid, character.level, character.map,
    );

    // 5. Enter the world, then claim movement control (vmangos ignores MSG_MOVE_* until we're the
    //    confirmed mover — the real client sends CMSG_SET_ACTIVE_MOVER on login).
    session.player_login(character.guid)?;
    session.set_active_mover(character.guid)?;

    // 6. Stream packets, tallying opcodes and folding the decoded `SessionEvent`s into a local entity
    //    map so we can read nearby entities' positions out of the stream.
    session.set_read_timeout(Some(Duration::from_secs(2)))?;
    let deadline = Instant::now() + Duration::from_secs(cli.seconds);

    // The shared world state every probe reads (identity, entity tracker, item/vendor stores, the
    // session-keeping acks); the DeathArc scenario machinery is staged when --death/--spirit is set.
    let mut world = World::new(character);
    if cli.death || cli.spirit {
        world.death_arc = Some(DeathArc::default());
    }

    // The probe registry: one entry per flag, in today's stream-loop/verify block order (so poll,
    // on_event, verify, and output order are preserved). Adding a probe is one push here.
    let mut probes: Vec<Box<dyn Probe>> = Vec::new();
    if cli.attack {
        probes.push(Box::new(Attack::default()));
    }
    if cli.charge {
        probes.push(Box::new(Charge::default()));
    }
    if cli.query_names {
        probes.push(Box::new(QueryNames::default()));
    }
    if cli.spells {
        probes.push(Box::new(Spells::default()));
    }
    if let Some(n) = cli.use_pack_slot {
        probes.push(Box::new(UsePackSlot { n }));
    }
    if let Some(n) = cli.equip_pack_slot {
        probes.push(Box::new(EquipPackSlot { n }));
    }
    if let Some((a, b)) = cli.swap_pack_slots {
        probes.push(Box::new(SwapPackSlots { a, b }));
    }
    if cli.vendor {
        probes.push(Box::new(Vendor));
    }
    if cli.quest {
        probes.push(Box::new(Quest));
    }
    if cli.aura {
        probes.push(Box::new(Aura));
    }
    if cli.giverstatus {
        probes.push(Box::new(GiverStatus));
    }
    if cli.quest_item {
        probes.push(Box::new(QuestItem));
    }
    if cli.questlog {
        probes.push(Box::new(QuestLog));
    }
    if cli.loot {
        probes.push(Box::new(Loot {
            loot_guid: cli.loot_guid,
        }));
    }
    if cli.speed {
        probes.push(Box::new(Speed::default()));
    }
    if cli.death {
        probes.push(Box::new(Death::default()));
    }
    if cli.spirit {
        probes.push(Box::new(Spirit::default()));
    }
    if cli.worldstate {
        probes.push(Box::new(WorldState::default()));
    }

    // Pre-stream staging: the DeathArc first (its `.revive` + teleport lead --spirit's `.repairitems`),
    // then every probe's stage in registry order, then the `--say` lines (after all staging, as today).
    world.stage(&mut session)?;
    for probe in probes.iter_mut() {
        let mut cx = Ctx {
            session: &mut session,
            world: &mut world,
        };
        probe.stage(&mut cx)?;
    }
    for line in &cli.say {
        session.send_chat(line)?;
        println!("sent chat: {line}");
    }

    println!("streaming world packets for {}s…", cli.seconds);
    while Instant::now() < deadline {
        world.poll(&mut session)?;
        for probe in probes.iter_mut() {
            let mut cx = Ctx {
                session: &mut session,
                world: &mut world,
            };
            probe.poll(&mut cx)?;
        }
        match session.recv() {
            Ok(msg) => {
                world.tally_packet(&msg);
                for ev in decode(msg) {
                    world.on_event(&ev, &mut session)?;
                    for probe in probes.iter_mut() {
                        let mut cx = Ctx {
                            session: &mut session,
                            world: &mut world,
                        };
                        probe.on_event(&ev, &mut cx)?;
                    }
                }
            }
            // Timeout / quiet stream: keep waiting until the deadline.
            Err(_) => continue,
        }
    }

    let total = world.total;
    println!("\n--- received {total} packet(s) ---");
    for (opcode, count) in &world.tally {
        println!("  {count:>4}  {opcode}");
    }

    // Report the entities we decoded, with raw WoW coordinates.
    let self_guid = world.self_guid;
    println!("\n--- tracked {} entit(ies) ---", world.tracked.len());
    for kind in [
        EntityKind::Player,
        EntityKind::Unit,
        EntityKind::GameObject,
        EntityKind::Other,
    ] {
        let mut group: Vec<(&u64, &Tracked)> = world
            .tracked
            .iter()
            .filter(|(_, t)| t.kind == kind)
            .collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by_key(|(guid, _)| **guid);
        println!("{:?} ({}):", kind, group.len());
        for (guid, t) in group.iter().take(8) {
            let me = if **guid == self_guid { " (self)" } else { "" };
            println!(
                "  guid {:<10} pos ({:>9.2}, {:>9.2}, {:>9.2}) o={:>5.2}{}",
                guid, t.position[0], t.position[1], t.position[2], t.orientation, me,
            );
        }
        if group.len() > 8 {
            println!("  … and {} more", group.len() - 8);
        }
    }

    let units = world
        .tracked
        .values()
        .filter(|t| t.kind == EntityKind::Unit)
        .count();
    let have_self = world.tracked.contains_key(&self_guid);
    if units == 0 && !have_self {
        bail!("decoded no positioned entities");
    }
    println!(
        "\n✅ decode: tracked {} entit(ies) with positions (self {}, {} unit/NPC).",
        world.tracked.len(),
        if have_self { "found" } else { "missing" },
        units,
    );

    // Post-stream verification: each probe's assertions + follow-up round trips, in registry order.
    for probe in probes.iter_mut() {
        let mut cx = Ctx {
            session: &mut session,
            world: &mut world,
        };
        probe.verify(&mut cx)?;
    }

    // 7. Optional: prove we can drive our own movement server-side. Walk forward, log out (which
    //    saves the character), then re-enum and compare the persisted position. Terminal — it ends
    //    the session, so it runs last.
    if let Some(yards) = cli.walk {
        let self_entity = world.tracked.get(&self_guid);
        let start = self_entity.map(|t| t.position).unwrap_or(world.spawn_pos);
        let orientation = self_entity.map(|t| t.orientation).unwrap_or(0.0);
        println!(
            "\nwalking {yards:.1} yd forward from ({:.2}, {:.2}, {:.2})…",
            start[0], start[1], start[2]
        );
        let end = walk_forward(&mut session, start, orientation, yards)?;

        // Drain a few seconds of server packets to see any reaction (teleport-back / correction).
        let drain_until = Instant::now() + Duration::from_secs(3);
        let mut reactions: BTreeMap<String, u32> = BTreeMap::new();
        while Instant::now() < drain_until {
            if let Ok(msg) = session.recv() {
                let name = msg.name();
                if name.contains("MOVE") || name.contains("TELEPORT") || name.contains("FORCE") {
                    *reactions.entry(name).or_default() += 1;
                }
            }
        }
        if reactions.is_empty() {
            println!("(no movement reaction packets from server)");
        } else {
            println!("server movement reactions: {reactions:?}");
        }

        // Log out (persists the character) and read the saved position back.
        session.logout(Duration::from_secs(25))?;
        let after = session.char_enum()?;
        let saved = after
            .iter()
            .find(|c| c.guid == self_guid)
            .map(|c| c.position)
            .context("character vanished after logout")?;

        let moved = ((saved.x - start[0]).powi(2)
            + (saved.y - start[1]).powi(2)
            + (saved.z - start[2]).powi(2))
        .sqrt();
        println!(
            "sent to   ({:.2}, {:.2}, {:.2})\nsaved as  ({:.2}, {:.2}, {:.2})  → moved {moved:.2} yd",
            end[0], end[1], end[2], saved.x, saved.y, saved.z,
        );
        if moved < 1.0 {
            bail!("server did not persist the move (snapped back?) — moved only {moved:.2} yd");
        }
        println!("\n✅ movement: server persisted our movement ({moved:.2} yd).");
    }

    Ok(())
}

/// Walk the active player `yards` forward along `orientation`, sending a realistic
/// start→heartbeat→stop sequence at run speed, and return the destination. WoW orientation 0 faces
/// +X; forward is `(cos o, sin o, 0)`. Coordinates are raw WoW yards.
fn walk_forward(
    session: &mut WorldSession,
    start: [f32; 3],
    orientation: f32,
    yards: f32,
) -> Result<[f32; 3]> {
    const RUN_SPEED: f32 = 7.0; // yd/s, vanilla default run speed
    const STEPS: u32 = 8;

    let (dx, dy) = (orientation.cos(), orientation.sin());
    let step_dist = yards / STEPS as f32;
    let dt = Duration::from_secs_f32((step_dist / RUN_SPEED).max(0.05));

    let pos_at = |i: u32| {
        [
            start[0] + dx * step_dist * i as f32,
            start[1] + dy * step_dist * i as f32,
            start[2],
        ]
    };

    session.start_forward(start, orientation)?;
    for i in 1..STEPS {
        std::thread::sleep(dt);
        session.heartbeat(pos_at(i), orientation)?;
    }
    std::thread::sleep(dt);
    let end = pos_at(STEPS);
    session.stop(end, orientation)?;
    Ok(end)
}
