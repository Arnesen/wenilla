//! `--quest-item`: the quest-STARTER item wire (decision 0664) — the fork a bag right-click makes
//! when the clicked item's template carries a non-zero `StartQuest`. The client does NOT send
//! `CMSG_USE_ITEM` for such an item (the server refuses that with `EQUIP_ERR_ITEM_NOT_FOUND`, the
//! red "The item was not found." line): it sends `CMSG_QUESTGIVER_QUERY_QUEST` addressed to the
//! **item's own guid**, and accepts against that same guid.
//!
//! This probe proves the whole item-as-questgiver round trip against the live server: add the item,
//! query its quest from the item guid, require `SMSG_QUESTGIVER_QUEST_DETAILS`, accept from the same
//! guid, and require the quest to land in `PLAYER_QUEST_LOG` with the starter kept-or-consumed as
//! the quest's own `ReqItemId`/`SrcItemId` dictate (vmangos `Player::AddQuest`).

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use benilla_protocol::{decode, SessionEvent, WorldSession};

use crate::probes::{Ctx, Probe, FIELD_PLAYER_QUEST_LOG_1_1};

/// A per-event handler for this probe's drain pump: it inspects each decoded [`SessionEvent`] and
/// returns `Some(msg)` to stop the drain early (the match landed) or `None` to keep pumping — the
/// `--quest` probe's own shape.
type QuestEventHandler = Box<dyn FnMut(&SessionEvent) -> Option<String>>;

/// The probe target: "Northshire Gift Voucher" (entry 14646), which starts quest 5805 "Welcome!" —
/// picked because it is takeable by the probe body as it stands (`MinLevel` 1, no prerequisite
/// quest, no race/class gate, item `RequiredLevel` 0 — VERIFIED live against
/// `mangos.item_template` ⋈ `quest_template`), so no rigging is needed. Non-equippable
/// (`inventory_type` 0) and spell-less, like all but five of the 215 quest-starters in 1.12.
const ITEM_ENTRY: u32 = 14646;
const QUEST_ID: u32 = 5805;

pub(crate) struct QuestItem;

impl Probe for QuestItem {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        // A clean slate so the accept is a real fresh accept across re-runs, then hand ourselves
        // the starter item (`verify` subtracts it again at the end).
        cx.session.send_chat(&format!(".quest remove {QUEST_ID}"))?;
        cx.session.send_chat(&format!(".additem {ITEM_ENTRY}"))?;
        println!("sent GM: .quest remove {QUEST_ID}; .additem {ITEM_ENTRY}");
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let world = &mut *cx.world;
        let session = &mut *cx.session;
        let self_guid = world.self_guid;

        // Pump packets up to `secs`, folding self-descriptor deltas into `self_fields` and handing
        // each decoded event to `f`; stop early when `f` returns Some. (The `--quest` probe's own
        // drain, which is a closure over its locals and so can't simply be shared.)
        let drain = |session: &mut WorldSession,
                     sf: &mut Option<benilla_protocol::messages::ObjectFields>,
                     secs: u64,
                     mut f: QuestEventHandler|
         -> Option<String> {
            let until = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < until {
                let Ok(msg) = session.recv() else { continue };
                for ev in decode(msg) {
                    if let SessionEvent::ObjectValues { guid: g, fields } = &ev {
                        if *g == self_guid {
                            if let Some(sf) = sf.as_mut() {
                                sf.merge(fields.clone());
                            }
                        }
                    }
                    if let Some(done) = f(&ev) {
                        return Some(done);
                    }
                }
            }
            None
        };

        // 1) Find the starter item in the backpack — the guid IS the questgiver on this wire.
        let sf = world
            .self_fields
            .as_ref()
            .context("no self descriptor — can't walk the backpack")?;
        let item_guid = (0..16)
            .filter_map(|i| sf.player_pack_slot(i).filter(|g| *g != 0))
            .find(|g| world.item_entries.get(g).copied() == Some(ITEM_ENTRY))
            .with_context(|| {
                format!(
                    "--quest-item: item {ITEM_ENTRY} isn't in the backpack — did `.additem` land? \
                     (needs a GM account; try a longer --seconds)"
                )
            })?;
        println!("\nquest-starter item {ITEM_ENTRY}: guid {item_guid:#x} (the giver on this wire)");

        // 2) The fork's own packet: QUERY_QUEST addressed to the ITEM guid → DETAILS.
        println!("CMSG_QUESTGIVER_QUERY_QUEST({QUEST_ID}) on the item guid");
        session.questgiver_query_quest(item_guid, QUEST_ID)?;
        let details = drain(
            session,
            &mut world.self_fields,
            5,
            Box::new(|ev| match ev {
                SessionEvent::QuestDetail(d) if d.quest_id == QUEST_ID => Some(format!(
                    "SMSG_QUESTGIVER_QUEST_DETAILS: \"{}\" — giver {:#x}",
                    d.title, d.npc
                )),
                _ => None,
            }),
        )
        .context(
            "--quest-item: no SMSG_QUESTGIVER_QUEST_DETAILS within 5s — the item guid was not \
             accepted as a questgiver",
        )?;
        println!("✅ details: {details}");

        // 3) Accept against the same item guid: the quest must land in the log, and the starter
        // must survive — vmangos `Player::AddQuest`'s "remove start item if not need" destroys a
        // `TYPEID_ITEM` giver ONLY when the quest neither requires it (`ReqItemId`) nor names it
        // `SrcItemId`, and this one is both (`quest_template` 5805: `SrcItemId = ReqItemId1 =
        // 14646` — the voucher IS the turn-in). Asserting the retention pins the branch as firmly
        // as a destroy would, and keeps the probe's item deterministic across re-runs.
        println!("CMSG_QUESTGIVER_ACCEPT_QUEST({QUEST_ID}) on the item guid");
        session.questgiver_accept_quest(item_guid, QUEST_ID)?;
        let mut destroyed = false;
        let mut in_log = false;
        for _ in 0..6 {
            if drain(
                session,
                &mut world.self_fields,
                1,
                Box::new(move |ev| match ev {
                    SessionEvent::ObjectDestroyed(g) if *g == item_guid => Some(String::new()),
                    _ => None,
                }),
            )
            .is_some()
            {
                destroyed = true;
            }
            in_log = world.self_fields.as_ref().is_some_and(|sf| {
                (0..20).any(|i| {
                    sf.raw_fields().any(|(idx, val)| {
                        idx == FIELD_PLAYER_QUEST_LOG_1_1 + 3 * i && val == QUEST_ID
                    })
                })
            });
            if in_log {
                break;
            }
        }
        anyhow::ensure!(
            in_log,
            "--quest-item: quest {QUEST_ID} never landed in a PLAYER_QUEST_LOG field — the \
             item-sourced accept did not take"
        );
        anyhow::ensure!(
            !destroyed,
            "--quest-item: the starter item {item_guid:#x} was destroyed, but quest {QUEST_ID} \
             requires it (ReqItemId1) — vmangos' AddQuest keeps a required starter"
        );
        println!(
            "✅ accept: quest {QUEST_ID} is in PLAYER_QUEST_LOG; the starter item survived (it is \
             the quest's own required turn-in)"
        );

        // 4) The director's second click (decision 0669): the starter is still in the bag while
        // the quest is in the log, so clicking it again re-sends the same QUERY. vmangos'
        // `HandleQuestgiverQueryQuestOpcode` has NO status gate — it answers with the DETAILS
        // again, which is why the panel legitimately re-opens — and the accept behind it is what
        // fails: `CanTakeQuest` → `SatisfyQuestStatus` → `SendCanTakeQuestResponse`
        // (`SMSG_QUESTGIVER_QUEST_INVALID`) with `INVALIDREASON_QUEST_ALREADY_ON` = 13 = 0x0d.
        // That 13 is the code the client maps to `ERR_QUEST_ALREADY_ON` — pin BOTH halves here.
        println!("\nsecond click while ON the quest — QUERY then ACCEPT again");
        session.questgiver_query_quest(item_guid, QUEST_ID)?;
        let reopened = drain(
            session,
            &mut world.self_fields,
            5,
            Box::new(|ev| match ev {
                SessionEvent::QuestDetail(d) if d.quest_id == QUEST_ID => {
                    Some(format!("\"{}\"", d.title))
                }
                _ => None,
            }),
        )
        .context(
            "--quest-item: the re-query got no DETAILS — this server DOES gate the query by \
             quest status, so the panel would not re-open (decision 0669 expects vmangos' \
             ungated handler)",
        )?;
        println!("✅ the panel legitimately re-opens: DETAILS {reopened}");

        session.questgiver_accept_quest(item_guid, QUEST_ID)?;
        let refusal = drain(
            session,
            &mut world.self_fields,
            5,
            Box::new(|ev| match ev {
                SessionEvent::QuestGiverInvalid { reason } => Some(reason.to_string()),
                _ => None,
            }),
        )
        .context(
            "--quest-item: the second accept drew no SMSG_QUESTGIVER_QUEST_INVALID within 5s",
        )?;
        anyhow::ensure!(
            refusal == "13",
            "--quest-item: expected QUEST_INVALID reason 13 (ALREADY_ON, the director's 0x0d), \
             got {refusal}"
        );
        println!("✅ refusal: QUEST_INVALID reason 13 → ERR_QUEST_ALREADY_ON (the ref's 0x5dbca0)");

        // Leave the probe character as found: drop the quest and the copy staging added (this
        // starter survives the accept, so without the subtract every run would litter one).
        session.send_chat(&format!(".quest remove {QUEST_ID}"))?;
        session.send_chat(&format!(".additem {ITEM_ENTRY} -1"))?;
        drain(session, &mut world.self_fields, 2, Box::new(|_| None));
        println!("cleanup: .quest remove {QUEST_ID}; .additem {ITEM_ENTRY} -1");
        Ok(())
    }
}
