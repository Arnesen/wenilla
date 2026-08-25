//! The app-side **group-loot-roll feed** (decision 0591) — the inward half of the roll seam around
//! [`benilla_ui::script`]'s `loot_roll` module, the sibling of [`crate::ui_loot`]'s window seam.
//!
//! When the group's loot method is group/need-before-greed and a drop is at or above the quality
//! threshold, the server does not put the item in anyone's loot window; it opens a **roll** on it
//! and every eligible looter gets a `GroupLootFrame` with Need/Greed/Pass and a one-minute bar.
//!
//! The net bridge ([`crate::net::apply`]) fills [`LootRolls`] from the wire: `SMSG_LOOT_START_ROLL`
//! → a fresh roll with a client-allocated id ([`LootRolls::start`]); `SMSG_LOOT_ROLL` → one
//! announcement line ([`LootRolls::announce`]); `SMSG_LOOT_ROLL_WON` / `SMSG_LOOT_ALL_PASSED` → the
//! resolution line plus the roll's close ([`LootRolls::won`] / [`LootRolls::all_passed`]).
//!
//! Each frame [`feed_loot_rolls`] ticks every open roll's remaining time, resolves each to a
//! Lua-facing [`LootRollEntry`], pushes the snapshot
//! ([`benilla_ui::script::UiScript::set_loot_rolls`]), fires `START_LOOT_ROLL(rollID, rollTime)` for
//! each newly opened roll and `CANCEL_LOOT_ROLL(rollID)` for each closed one, and drains the queued
//! announcement lines into the chat window once their names resolve. [`drain_loot_rolls`] pulls the
//! `RollOnLoot` votes back out as [`ClientCommand::LootRoll`].
//!
//! ## `UPDATE_LOOT_ROLL` — why a snapshot seam needs an event the real client doesn't have
//!
//! The reference's `GetLootRollItemInfo` reads live C state off the roll node, so its
//! `GroupLootFrame_OnShow` can paint once and be done. Ours reads a **pushed snapshot**, and a
//! snapshot is always at least one step behind the thing that made it change — twice over:
//!
//! 1. A roll is added to [`LootRolls::active`] and to `opened` in the same [`LootRolls::start`]
//!    call, so the *first* snapshot that contains a roll is built in the same pass that announces
//!    it. Push before the announce and the OnShow sees it; announce first and it paints a blank.
//! 2. `name`/`texture`/`quality`/`bindOnPickUp` come from the ask-once item-template cache and are
//!    `None` until the query lands, which is typically several frames after the roll opens.
//!
//! (1) is fixed by ordering — the push precedes the events here, as it does in [`crate::ui_loot`].
//! (2) cannot be, so `UPDATE_LOOT_ROLL(rollID)` fires whenever an open roll's *display identity*
//! ([`display_identity`]) changes underneath a frame, and the frame repaints. Ordering alone would
//! be a guarantee resting on system order in a file far from the Lua that depends on it; with the
//! repair in place the paint is correct either way, and ordering only decides whether there is a
//! visible blank first. Both halves ship: (1) for the frame you actually see, (2) because it is what
//! makes the documented in-flight fallback resolve instead of staying a `?` forever.
//!
//! ## Two client-side behaviours, and why
//!
//! - **`rollID` is ours.** The wire addresses a roll by `(lootedTarget, itemSlot)`; the FrameXML API
//!   addresses it by an opaque `rollID`. Nothing on the wire depends on its value, so
//!   [`LootRolls::next_id`] hands out a monotonic one per roll and the drain maps it back.
//! - **Our own vote closes our frame immediately** ([`LootRolls::vote`]) — client-predicted, not
//!   waiting for the server's resolution, the same shape as the 0515 loot-kneel latch. The server
//!   echoes our vote as an ordinary `SMSG_LOOT_ROLL` announcement and only resolves the roll once
//!   *everyone* has voted or the minute expires, so a server-driven close would leave our frame up
//!   for up to a minute after we clicked. **VERIFIED** in the 5875 binary (decision 0594, resolving
//!   0591's deferral): `RollOnLoot` at `0x61bdf0` sends the CMSG and fires `CANCEL_LOOT_ROLL`
//!   itself, in the same call — the client-side close is the real behaviour, not an approximation.
//! - **…except on a bind-on-pickup roll**, where a Need or Greed sends *nothing* and leaves the
//!   frame up: the seam raises `CONFIRM_LOOT_ROLL` instead, and only the popup's `ConfirmLootRoll`
//!   re-enters past the gate (decision 0594; the gate is in the real client's C `RollOnLoot`, so it
//!   lives in our seam too — see [`benilla_ui::script`]'s `loot_roll`). Pass is never gated.

use benilla_protocol::messages::{roll_vote, LootAllPassed, LootRoll, LootRollWon, LootStartRoll};
use bevy::prelude::*;

use benilla_ui::script::{LootRollEntry, LootRollsState, ScriptValue, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands, SelfGuid};
use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_script::UiInput;

/// Give up re-checking a pending announcement's names after this many frames — the same budget and
/// reasoning as [`crate::ui_loot`]'s receive lines (a negative-cached entry never resolves).
const LINE_MAX_TRIES: u16 = 120;

/// `item_template.bonding` = *bind on pickup* (VERIFIED vmangos `ItemPrototype.h`'s `ItemBondingType`:
/// `NO_BIND` 0, `BIND_WHEN_PICKED_UP` 1, `BIND_WHEN_EQUIPPED` 2, `BIND_WHEN_USE` 3, `QUEST_ITEM` 4).
/// Only `1` drives `GroupLootFrame_OnShow`'s gold BoP backdrop — a BoE roll shows the plain plate.
const BIND_WHEN_PICKED_UP: u32 = 1;

/// One group loot roll currently open on our screen.
struct ActiveRoll {
    /// The client-internal id the Lua side addresses this roll by (see the module docs).
    roll_id: u32,
    /// The roll's **wire** identity — what `CMSG_LOOT_ROLL` addresses it by.
    looted_target: u64,
    item_slot: u32,
    item_id: u32,
    /// `SMSG_LOOT_START_ROLL`'s `randomPropertyId` — the drop's random-suffix roll (decision
    /// 1547). The reference's own `SetLootRollItem 0x5364a0` copies exactly this into the
    /// tooltip's `+0x424` and passes no item object, so — like a loot slot — the roll is the only
    /// enchant source the roll window's hover can have.
    random_property_id: u32,
    /// Milliseconds left; ticked down by [`feed_loot_rolls`], saturating at `0`. The roll is *not*
    /// dropped at zero — the server closes it with `SMSG_LOOT_ROLL_WON`/`SMSG_LOOT_ALL_PASSED` when
    /// its own timer fires, and the frame stays up (bar empty) until that lands.
    remaining_ms: u32,
}

/// One queued chat announcement, awaiting the item/player names it needs to render.
struct PendingLine {
    line: RollLine,
    tries: u16,
}

/// Which announcement a [`PendingLine`] renders.
enum RollLine {
    /// `SMSG_LOOT_ROLL` — a vote, or a dice result (the overloaded pair; see [`LootRoll::is_dice`]).
    Announce(LootRoll),
    /// `SMSG_LOOT_ROLL_WON` — the roll resolved.
    Won(LootRollWon),
    /// `SMSG_LOOT_ALL_PASSED` — nobody wanted it.
    AllPassed(LootAllPassed),
}

/// Every group loot roll open on our screen, filled by the net bridge and read by
/// [`feed_loot_rolls`]. Cleared on disconnect.
#[derive(Resource, Default)]
pub(crate) struct LootRolls {
    active: Vec<ActiveRoll>,
    /// Monotonic client-internal id source (see the module docs). Never reused within a session, so
    /// a late packet for a closed roll can't collide with a fresh one.
    next_id: u32,
    /// Announcement lines awaiting their names.
    pending: Vec<PendingLine>,
    /// Newly opened rolls → one `START_LOOT_ROLL(rollID, rollTime)` each, drained by the feed.
    opened: Vec<(u32, u32)>,
    /// Newly closed rolls → one `CANCEL_LOOT_ROLL(rollID)` each, drained by the feed.
    cancelled: Vec<u32>,
}

impl LootRolls {
    /// A roll opened (`SMSG_LOOT_START_ROLL`): allocate its client-internal id and queue the
    /// `START_LOOT_ROLL` the `GroupLootFrame` machinery listens for.
    ///
    /// A **duplicate** start for a `(looted_target, item_slot)` we already hold is ignored rather
    /// than opening a second frame for the same item: the server re-sends every active roll on
    /// reconnect (`Group::SendLootStartRollsForPlayer`, gated by vmangos'
    /// `SEND_LOOT_ROLL_UPON_RECONNECT`), which would otherwise stack duplicate frames.
    pub(crate) fn start(&mut self, p: LootStartRoll) {
        if self
            .active
            .iter()
            .any(|r| r.looted_target == p.looted_target && r.item_slot == p.item_slot)
        {
            debug!(
                "ui_loot_roll: duplicate start for {:#x} slot {} — ignored",
                p.looted_target, p.item_slot
            );
            return;
        }
        self.next_id += 1;
        let roll_id = self.next_id;
        self.active.push(ActiveRoll {
            roll_id,
            looted_target: p.looted_target,
            item_slot: p.item_slot,
            item_id: p.item_id,
            random_property_id: p.random_property_id,
            remaining_ms: p.countdown_ms,
        });
        self.opened.push((roll_id, p.countdown_ms));
    }

    /// One roller's vote or dice result (`SMSG_LOOT_ROLL`) — queue its chat line.
    pub(crate) fn announce(&mut self, p: LootRoll) {
        self.pending.push(PendingLine {
            line: RollLine::Announce(p),
            tries: 0,
        });
    }

    /// The roll resolved (`SMSG_LOOT_ROLL_WON`): queue the line and close the frame.
    pub(crate) fn won(&mut self, p: LootRollWon) {
        self.pending.push(PendingLine {
            line: RollLine::Won(p),
            tries: 0,
        });
        self.close(p.looted_target, p.item_slot);
    }

    /// Nobody wanted it (`SMSG_LOOT_ALL_PASSED`): queue the line and close the frame. The item
    /// returns to the corpse and reappears as an ordinary lootable row.
    pub(crate) fn all_passed(&mut self, p: LootAllPassed) {
        self.pending.push(PendingLine {
            line: RollLine::AllPassed(p),
            tries: 0,
        });
        self.close(p.looted_target, p.item_slot);
    }

    /// Drop the roll on `(looted_target, item_slot)` if we still hold it, queueing its
    /// `CANCEL_LOOT_ROLL`. Idempotent — a resolution for a roll we already closed is a no-op.
    fn close(&mut self, looted_target: u64, item_slot: u32) {
        if let Some(i) = self
            .active
            .iter()
            .position(|r| r.looted_target == looted_target && r.item_slot == item_slot)
        {
            let r = self.active.remove(i);
            self.cancelled.push(r.roll_id);
        }
    }

    /// Our own vote on `roll_id`: close the frame client-side and return the roll's wire identity
    /// for the outbound `CMSG_LOOT_ROLL`. `None` if no such roll is open (a stale click).
    fn vote(&mut self, roll_id: u32) -> Option<(u64, u32)> {
        let i = self.active.iter().position(|r| r.roll_id == roll_id)?;
        let r = self.active.remove(i);
        self.cancelled.push(r.roll_id);
        Some((r.looted_target, r.item_slot))
    }

    /// Tick every open roll's bar down by `delta_ms`, saturating at zero (see
    /// [`ActiveRoll::remaining_ms`] for why a spent roll is not dropped here).
    fn tick(&mut self, delta_ms: u32) {
        for r in &mut self.active {
            r.remaining_ms = r.remaining_ms.saturating_sub(delta_ms);
        }
    }

    /// Drop everything (session teardown) — no `CANCEL_LOOT_ROLL` fan-out: the whole UI is going
    /// down with the session, and the queues would never be drained.
    pub(crate) fn clear(&mut self) {
        self.active.clear();
        self.pending.clear();
        self.opened.clear();
        self.cancelled.clear();
    }
}

pub(crate) struct UiLootRollPlugin;

impl Plugin for UiLootRollPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LootRolls>().add_systems(
            Update,
            (
                // Same ordering rule as the loot window (ui_loot): push before the input pass so a
                // freshly opened roll is on screen the same frame, drain after it so a Need/Greed/
                // Pass click goes out the same frame.
                feed_loot_rolls.before(UiInput),
                drain_loot_rolls.after(UiInput),
            ),
        );
    }
}

/// Pick and fill the announcement's chat string, given the pieces already resolved: the roller's
/// `name` (present whenever the chosen string needs one — see [`render`]), whether the line is about
/// *us* (`is_self`), and the item `link`.
///
/// The strings are QUOTED from `Interface\FrameXML\GlobalStrings.lua` (verified extract,
/// l.2614-2633). The selection follows the four `(roll_number, roll_type)` shapes vmangos emits —
/// see [`LootRoll`]'s table — and **must branch on `roll_number` first**: a Greed *vote* is
/// `(128, 2)` and a Greed *dice roll* is `(1..=100, 2)`, identical in `roll_type`.
///
/// **The dice line has NO `_SELF` split** (decision 0594, correcting 0591). `GlobalStrings.lua`
/// *does* define `LOOT_ROLL_ROLLED_NEED_SELF`/`_GREED_SELF` — but that file is MPQ **data**, and the
/// 5875 **binary** never references them: the shared formatter tail (`0x61c320`) picks
/// `ROLLED_GREED` when `rollType == 2` else `ROLLED_NEED`, with no self test anywhere. Of the 20
/// `LOOT_ROLL_*` keys in the data file the C++ references only 15; the 5 orphans are exactly those
/// two, `LOOT_ROLL_ROLLED`, `_ROLLED_SELF`, and `LOOT_ROLL_START`. So **your own numbered roll
/// prints third-person with your own name** — the trap here is that a FrameXML-only pin reaches
/// naturally for a string that is present but dead.
///
/// The *vote* lines keep their `_SELF` split (`LOOT_ROLL_NEED_SELF` &c. are among the referenced
/// 15), as does `LOOT_ROLL_YOU_WON`.
///
/// **The `NO_SPAM` variants are the `showLootSpam == 0` branch**, and since decision 1589 (B246's
/// Chat options page) that CVar has a row, so `detailed` is a real argument rather than a constant
/// `true`. 0594 §3 recorded the whole gated flow waiting for exactly this; wow-re's
/// `lootroll-chat-and-lifecycle.md` §4 is the byte census behind it:
///
/// | `showLootSpam` | the per-vote / per-dice line (`0x61c0b0`) | the WON line (`0x61b9e0`) |
/// |---|---|---|
/// | `1` (default) | emitted | `LOOT_ROLL_WON` / `_YOU_WON` — **no roll number** |
/// | `0` | suppressed entirely | `*_NO_SPAM_NEED` / `_GREED` — **carries the roll number** |
///
/// Two things about that table are easy to get backwards and both are VERIFIED: it is the *winner*
/// line that changes shape (turning detail OFF makes it say MORE, because it is now the only line
/// you get), and **`SMSG_LOOT_ALL_PASSED` is never gated on either side** — `0x61b640` contains no
/// read of the CVar at all. The `NO_SPAM` discriminator is `rollType == 1` (need), so anything that
/// is not need renders as greed.
///
/// Returns `None` for the one case the reference drops on the floor: a vote/dice line with detail
/// off.
///
/// Kept free of the resource lookups (`name`/`link` arrive resolved) so the whole table is directly
/// testable — it is the piece most likely to be got subtly wrong.
fn format_line(
    line: &RollLine,
    name: Option<&str>,
    is_self: bool,
    link: &str,
    detailed: bool,
) -> Option<String> {
    if !detailed {
        if let RollLine::Announce(_) = line {
            // `0x61c0b9`: the composer frees the node and returns.
            return None;
        }
    }
    Some(format_line_detailed(line, name, is_self, link, detailed))
}

/// [`format_line`]'s body once the suppression fork is out of the way.
fn format_line_detailed(
    line: &RollLine,
    name: Option<&str>,
    is_self: bool,
    link: &str,
    detailed: bool,
) -> String {
    match line {
        // A real dice result (roll_number in 1..=100) — checked BEFORE the vote shapes.
        RollLine::Announce(p) if p.is_dice() => {
            let n = p.roll_number;
            let w = name.unwrap_or_default();
            // LOOT_ROLL_ROLLED_NEED / _GREED (l.2624 / l.2622) — note the trailing "by %s". No
            // self variant: `is_self` is deliberately unread here (see this fn's doc).
            match p.roll_type {
                roll_vote::NEED => format!("Need Roll - {n} for {link} by {w}"),
                _ => format!("Greed Roll - {n} for {link} by {w}"),
            }
        }
        // One of the three vote announcements.
        RollLine::Announce(p) => match (is_self, p.vote()) {
            // LOOT_ROLL_NEED_SELF / _GREED_SELF / _PASSED_SELF (l.2618 / l.2616 / l.2620).
            (true, Some(roll_vote::NEED)) => format!("You have selected Need for: {link}"),
            (true, Some(roll_vote::GREED)) => format!("You have selected Greed for: {link}"),
            (true, _) => format!("You passed on: {link}"),
            // LOOT_ROLL_NEED / _GREED / _PASSED (l.2617 / l.2615 / l.2619).
            (false, Some(roll_vote::NEED)) => {
                format!("{} has selected Need for: {link}", name.unwrap_or_default())
            }
            (false, Some(roll_vote::GREED)) => {
                format!(
                    "{} has selected Greed for: {link}",
                    name.unwrap_or_default()
                )
            }
            (false, _) => format!("{} passed on: {link}", name.unwrap_or_default()),
        },
        // LOOT_ROLL_*_NO_SPAM_NEED / _GREED (l.2629/2630, l.2632/2633) — the detail-off winner
        // line, which is the ONLY line that roll produces, so it carries the number the suppressed
        // dice line would have shown. The grey is the GlobalString's own `|cff818181`.
        RollLine::Won(p) if !detailed => {
            let kind = if p.roll_type == roll_vote::NEED {
                "Need"
            } else {
                "Greed"
            };
            let n = p.roll_number;
            if is_self {
                format!("You won: {link} |cff818181({kind} - {n})|r")
            } else {
                format!(
                    "{} won: {link} |cff818181({kind} - {n})|r",
                    name.unwrap_or_default()
                )
            }
        }
        // LOOT_ROLL_YOU_WON (l.2631) / LOOT_ROLL_WON (l.2628).
        RollLine::Won(_) if is_self => format!("You won: {link}"),
        RollLine::Won(_) => format!("{} won: {link}", name.unwrap_or_default()),
        // LOOT_ROLL_ALL_PASSED (l.2614) — names nobody.
        RollLine::AllPassed(_) => format!("Everyone passed on: {link}"),
    }
}

/// Render one queued announcement, or `None` while a name it needs is still in flight. Resolves the
/// item template + the roller's name, then defers the whole string choice to [`format_line`].
fn render(
    line: &RollLine,
    self_guid: Option<u64>,
    items: &mut Items,
    names: &mut NameCache,
    commands: &NetCommands,
    rolls: crate::items::RollCatalogs,
    detailed: bool,
) -> Option<String> {
    // Every line embeds the item link, so the template must be in hand before any of them render.
    let (looted_item, roll, roller) = match line {
        RollLine::Announce(p) => (p.item_id, p.random_property_id, Some(p.roller)),
        RollLine::Won(p) => (p.item_id, p.random_property_id, Some(p.winner)),
        RollLine::AllPassed(p) => (p.item_id, p.random_property_id, None),
    };
    let t = items.template(looted_item, 0, commands)?;
    // The link's name is the ROLLED one (1547) — every announcement names the same drop the roll
    // window does, so "[Bloodrazor of the Monkey] won by …" has to agree with the frame.
    let link = crate::ui_items::item_link_full(
        looted_item,
        0,
        roll,
        0,
        &rolls.name(&t.name.clone(), roll),
        t.quality,
    );

    let is_self = roller.is_some() && roller == self_guid;

    // Which strings actually need a name (decision 0594): every third-person one, PLUS our own
    // *dice* line — the binary's ROLLED_* tail has no self variant, so a roll of ours still prints
    // "Need Roll - 57 for [Item] by <us>" and must resolve our OWN name. The self *vote* and
    // *YOU_WON* lines do have _SELF forms and need no lookup; `AllPassed` names nobody.
    // Resolving our own name is the house pattern (`npc_text::player_identity`) — one ask-once
    // NameQuery, cached thereafter.
    let needs_name = match line {
        RollLine::Announce(p) => p.is_dice() || !is_self,
        RollLine::Won(_) => !is_self,
        RollLine::AllPassed(_) => false,
    };
    let name = match roller {
        Some(g) if needs_name => Some(names.resolve(g, commands)?.to_string()),
        _ => None,
    };

    // Never `None` here: the suppression fork is the caller's (a suppressed line must be DROPPED,
    // where a `None` from this function means "retry, a name is still in flight").
    format_line(line, name.as_deref(), is_self, &link, detailed)
}

/// Surface the queued announcement lines in the chat window once their names resolve, colored
/// `LOOT` green (the roll lines ride `CHAT_MSG_LOOT` in the real client, like the receive lines).
/// Unresolved lines retry up to [`LINE_MAX_TRIES`] frames, then drop.
#[allow(clippy::too_many_arguments)] // the line resolve's full read set
fn drain_lines(
    rolls: &mut LootRolls,
    self_guid: Option<u64>,
    items: &mut Items,
    names: &mut NameCache,
    commands: &NetCommands,
    chat: &mut ChatLog,
    catalogs: crate::items::RollCatalogs,
    detailed: bool,
) {
    let pending = std::mem::take(&mut rolls.pending);
    let mut still = Vec::new();
    for mut p in pending {
        // Detail off drops the vote/dice lines outright — and *drops* them, never retries them,
        // which is why the fork is here and not inside `render`'s `None` (that one means "a name
        // is still in flight, come back next frame").
        if !detailed && matches!(p.line, RollLine::Announce(_)) {
            continue;
        }
        match render(
            &p.line, self_guid, items, names, commands, catalogs, detailed,
        ) {
            Some(text) => chat.push_event(ChatEvent::text_only(ChatEventKind::Loot, text)),
            None => {
                p.tries += 1;
                if p.tries < LINE_MAX_TRIES {
                    still.push(p);
                }
            }
        }
    }
    rolls.pending = still;
}

/// Build the Lua-facing snapshot from [`LootRolls`] — the icon straight from the item template's
/// display id through the same catalog the bags use, name/quality/bind from the ask-once template
/// cache (`None`/`false` while in flight; the frame shows its placeholder and fills in later).
fn snapshot(
    rolls: &LootRolls,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
    catalogs: crate::items::RollCatalogs,
) -> LootRollsState {
    let entries = rolls
        .active
        .iter()
        .map(|r| {
            let t = items.template(r.item_id, 0, commands);
            let texture = t
                .and_then(|t| icons?.catalog.get(t.display_info_id))
                .and_then(|d| d.icon.clone());
            LootRollEntry {
                roll_id: r.roll_id,
                name: t.map(|t| catalogs.name(&t.name, r.random_property_id)),
                texture,
                // The roll is always for the whole stack the drop rolled; the wire carries no
                // count on SMSG_LOOT_START_ROLL, so the frame shows the single-item case (the
                // reference `GroupLootFrame_OnShow` reads `count` but 1.12 group rolls are
                // per-item — a stacked drop opens one roll).
                quantity: 1,
                quality: t.map(|t| t.quality),
                bind_on_pickup: t.is_some_and(|t| t.bonding == BIND_WHEN_PICKED_UP),
                time_left_ms: r.remaining_ms,
                item_id: r.item_id,
                // The roll the hover resolves its enchant lines from — `SetLootRollItem`'s own
                // `+0x424` (1547).
                random_property_id: r.random_property_id,
                // The icon button's ctrl/shift arms read this (`GetLootRollItemLink`, decision
                // 1059). Same builder and same arguments as the announcement lines' own link a few
                // functions up ([`render`]) — one `item_link` call site per resolved roll, `None`
                // until the template answers, exactly like `name`/`quality` beside it.
                link: t.map(|t| {
                    crate::ui_items::item_link_full(
                        r.item_id,
                        0,
                        r.random_property_id,
                        0,
                        &catalogs.name(&t.name, r.random_property_id),
                        t.quality,
                    )
                }),
            }
        })
        .collect();
    LootRollsState { rolls: entries }
}

/// Everything about a roll a `GroupLootFrame` actually *paints* — deliberately **not**
/// `time_left_ms`, which ticks every frame and would make "the snapshot changed" mean nothing.
fn display_identity(e: &LootRollEntry) -> (&Option<String>, &Option<String>, Option<u32>, bool) {
    (&e.name, &e.texture, e.quality, e.bind_on_pickup)
}

/// The rolls open in *both* snapshots whose display identity changed — i.e. the ones with a frame
/// already up that is now painting the wrong thing. A roll only in `fresh` is newly opened and gets
/// its `START_LOOT_ROLL`; one only in `last` is closed and gets its `CANCEL_LOOT_ROLL`.
fn repainted(last: &LootRollsState, fresh: &LootRollsState) -> Vec<u32> {
    fresh
        .rolls
        .iter()
        .filter(|f| {
            last.rolls
                .iter()
                .find(|l| l.roll_id == f.roll_id)
                .is_some_and(|l| display_identity(l) != display_identity(f))
        })
        .map(|f| f.roll_id)
        .collect()
}

/// Tick the open rolls, push them into the VM, fire the open/close events, and drain the queued
/// announcement lines into chat.
#[allow(clippy::too_many_arguments)]
fn feed_loot_rolls(
    script: Option<NonSendMut<UiScript>>,
    mut rolls: ResMut<LootRolls>,
    mut items: ResMut<Items>,
    mut names: ResMut<NameCache>,
    icons: Option<Res<ItemDisplays>>,
    commands: Res<NetCommands>,
    self_guid: Res<SelfGuid>,
    mut chat: ResMut<ChatLog>,
    time: Res<Time>,
    mut last: Local<crate::ui_script::VmMemo<LootRollsState>>,
    // The random-suffix roll's catalogs (1547): the rolled name the frame and its chat lines show.
    props: Option<Res<crate::items::RandomProperties>>,
    enchants: Option<Res<crate::items::Enchants>>,
    // `showLootSpam` (1589) — read here, at the moment each line is composed, exactly where the
    // reference reads it (`0x61ba3a`/`0x61bafe`/`0x61c0b9`, all three inside the composers).
    loot: Res<crate::ui_loot::LootConfig>,
) {
    rolls.tick(time.delta().as_millis() as u32);

    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let catalogs = crate::items::RollCatalogs {
        props: props.as_deref(),
        enchants: enchants.as_deref(),
    };
    drain_lines(
        &mut rolls,
        self_guid.0,
        &mut items,
        &mut names,
        &commands,
        &mut chat,
        catalogs,
        loot.show_loot_spam,
    );

    // The snapshot goes in FIRST — a GroupLootFrame claimed by the START_LOOT_ROLL below reads its
    // item out of the model in its OnShow, and the roll it is about was added to `active` in the
    // same `start()` call that queued `opened`, so pushing after would hand every fresh roll an
    // empty lookup. Same order as ui_loot's window feed, for the same reason.
    let fresh = snapshot(&rolls, &mut items, icons.as_deref(), &commands, catalogs);
    let changed = repainted(last, &fresh);
    if fresh != *last {
        script.set_loot_rolls(fresh.clone());
        *last = fresh;
    }

    for (roll_id, roll_time) in std::mem::take(&mut rolls.opened) {
        debug!("ui_loot_roll: roll {roll_id} opened ({roll_time} ms)");
        script.fire_event(
            "START_LOOT_ROLL",
            vec![
                ScriptValue::Int(roll_id as i64),
                ScriptValue::Int(roll_time as i64),
            ],
        );
    }
    for roll_id in std::mem::take(&mut rolls.cancelled) {
        debug!("ui_loot_roll: roll {roll_id} cancelled");
        script.fire_event("CANCEL_LOOT_ROLL", vec![ScriptValue::Int(roll_id as i64)]);
    }
    // The late item template landing under a frame that is already up (see the module docs).
    for roll_id in changed {
        debug!("ui_loot_roll: roll {roll_id} display identity resolved");
        script.fire_event("UPDATE_LOOT_ROLL", vec![ScriptValue::Int(roll_id as i64)]);
    }
}

/// Drain the Lua votes: each `RollOnLoot(rollID, rollType)` becomes one `CMSG_LOOT_ROLL` addressed
/// by the roll's wire identity, and closes our frame client-side (see the module docs).
fn drain_loot_rolls(
    script: Option<NonSendMut<UiScript>>,
    mut rolls: ResMut<LootRolls>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    // A Need/Greed on a bind-on-pickup roll sends nothing yet: the seam diverted it here, and the
    // frame STAYS UP while the popup asks (decision 0594). `CONFIRM_LOOT_ROLL(rollID, rollType)`
    // is what `UIParent_OnEvent` turns into `StaticPopup_Show("CONFIRM_LOOT_ROLL")`.
    for (roll_id, roll_type) in script.take_loot_roll_confirms() {
        debug!("ui_loot_roll: BoP confirm for {roll_type} on roll {roll_id}");
        script.fire_event(
            "CONFIRM_LOOT_ROLL",
            vec![
                ScriptValue::Int(roll_id as i64),
                ScriptValue::Int(roll_type as i64),
            ],
        );
    }
    for (roll_id, roll_type) in script.take_loot_roll_votes() {
        match rolls.vote(roll_id) {
            Some((looted_target, item_slot)) => {
                debug!("ui_loot_roll: vote {roll_type} on roll {roll_id}");
                let _ = commands.0.send(ClientCommand::LootRoll {
                    looted_target,
                    item_slot,
                    roll_type,
                });
            }
            None => debug!("ui_loot_roll: RollOnLoot({roll_id}) — no such roll, ignored"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(target: u64, slot: u32, item_id: u32) -> LootStartRoll {
        LootStartRoll {
            looted_target: target,
            item_slot: slot,
            item_id,
            random_property_id: 0,
            countdown_ms: 60_000,
        }
    }

    fn announce(roller: u64, roll_number: u8, roll_type: u8) -> LootRoll {
        LootRoll {
            looted_target: 0xAA,
            item_slot: 0,
            roller,
            item_id: 17182,
            random_property_id: 0,
            roll_number,
            roll_type,
        }
    }

    const LINK: &str = "|cffa335ee|Hitem:17182:0:0:0|h[Sulfuras]|h|r";

    /// The whole `LOOT_ROLL_*` selection table, both persons. The Greed pair is the case worth
    /// locking down: `(128, 2)` is a *vote* and `(57, 2)` a *dice roll*, identical in `roll_type`,
    /// so a table that branched on `roll_type` alone would render the same line for both.
    #[test]
    fn line_table_covers_every_shape() {
        let other = Some("Bob");
        let me = Some("Sam");
        // (packet, name, is_self, expected)
        let cases: &[(LootRoll, Option<&str>, bool, &str)] = &[
            // ── The three vote announcements (Group.cpp:970-990) — these DO have _SELF forms ───
            (
                announce(1, 0, 0),
                other,
                false,
                "Bob has selected Need for: {L}",
            ),
            (
                announce(1, 0, 0),
                None,
                true,
                "You have selected Need for: {L}",
            ),
            (
                announce(1, 128, roll_vote::GREED),
                other,
                false,
                "Bob has selected Greed for: {L}",
            ),
            (
                announce(1, 128, roll_vote::GREED),
                None,
                true,
                "You have selected Greed for: {L}",
            ),
            (announce(1, 128, 128), other, false, "Bob passed on: {L}"),
            (announce(1, 128, 128), None, true, "You passed on: {L}"),
            // ── The dice results (Group.cpp:1163 / :1214) ─────────────────────────────────────
            // NO _SELF split (0594): the self rows below are third-person WITH OUR OWN NAME, not
            // "You roll a 57 …". The binary's ROLLED_* tail never tests for self.
            (
                announce(1, 57, roll_vote::NEED),
                other,
                false,
                "Need Roll - 57 for {L} by Bob",
            ),
            (
                announce(1, 57, roll_vote::NEED),
                me,
                true,
                "Need Roll - 57 for {L} by Sam",
            ),
            (
                announce(1, 57, roll_vote::GREED),
                other,
                false,
                "Greed Roll - 57 for {L} by Bob",
            ),
            (
                announce(1, 57, roll_vote::GREED),
                me,
                true,
                "Greed Roll - 57 for {L} by Sam",
            ),
        ];
        for (p, name, is_self, expect) in cases {
            let got = format_line(&RollLine::Announce(*p), *name, *is_self, LINK, true);
            assert_eq!(
                got,
                Some(expect.replace("{L}", LINK)),
                "roll_number {} roll_type {} name {name:?} is_self {is_self}",
                p.roll_number,
                p.roll_type
            );
        }
    }

    /// The correction 0594 landed, pinned on its own: a dice roll of OURS must never take a
    /// "You roll a …" form, because the string the 5875 binary would need for that
    /// (`LOOT_ROLL_ROLLED_NEED_SELF`) is one of the five `LOOT_ROLL_*` keys that exist in
    /// GlobalStrings.lua but which no C++ path references. Guards against a well-meaning
    /// re-reading of the data file reintroducing it.
    #[test]
    fn our_own_dice_roll_prints_third_person() {
        for roll_type in [roll_vote::NEED, roll_vote::GREED] {
            let got = format_line(
                &RollLine::Announce(announce(1, 57, roll_type)),
                Some("Sam"),
                true, // is_self
                LINK,
                true,
            )
            .expect("detail on emits the dice line");
            assert!(
                got.ends_with("by Sam"),
                "our own roll must name us third-person: {got}"
            );
            assert!(
                !got.contains("You roll"),
                "no _SELF dice form exists: {got}"
            );
        }
    }

    /// The greed vote and the greed dice roll must NOT render the same line — the regression a
    /// `match roll_type` implementation would introduce.
    #[test]
    fn greed_vote_and_greed_roll_differ() {
        let vote = format_line(
            &RollLine::Announce(announce(1, 128, roll_vote::GREED)),
            Some("Bob"),
            false,
            LINK,
            true,
        )
        .unwrap();
        let dice = format_line(
            &RollLine::Announce(announce(1, 57, roll_vote::GREED)),
            Some("Bob"),
            false,
            LINK,
            true,
        )
        .unwrap();
        assert_ne!(vote, dice);
        assert!(vote.contains("has selected Greed"), "{vote}");
        assert!(dice.contains("Greed Roll - 57"), "{dice}");
    }

    #[test]
    fn resolution_lines() {
        let won = LootRollWon {
            looted_target: 0xAA,
            item_slot: 0,
            item_id: 17182,
            random_property_id: 0,
            winner: 1,
            roll_number: 84,
            roll_type: roll_vote::NEED,
        };
        assert_eq!(
            format_line(&RollLine::Won(won), Some("Bob"), false, LINK, true),
            Some(format!("Bob won: {LINK}"))
        );
        // LOOT_ROLL_YOU_WON *does* exist and is referenced — the won line keeps its self split.
        assert_eq!(
            format_line(&RollLine::Won(won), None, true, LINK, true),
            Some(format!("You won: {LINK}"))
        );
        let passed = LootAllPassed {
            looted_target: 0xAA,
            item_slot: 0,
            item_id: 17182,
            random_property_id: 0,
        };
        // Names nobody — the `name`/`is_self` arguments are irrelevant on this arm.
        assert_eq!(
            format_line(&RollLine::AllPassed(passed), Some("Bob"), false, LINK, true),
            Some(format!("Everyone passed on: {LINK}"))
        );
    }

    /// `showLootSpam == 0` — the whole gated flow 0594 §3 recorded and 1589 finally wired, all
    /// three of its claims in one place (wow-re `lootroll-chat-and-lifecycle.md` §4).
    #[test]
    fn detail_off_suppresses_the_roll_lines_and_reshapes_the_winner() {
        // 1 · every vote and every dice line is dropped outright.
        for p in [
            announce(1, 128, roll_vote::NEED),
            announce(1, 128, roll_vote::GREED),
            announce(1, 128, roll_vote::PASS),
            announce(1, 57, roll_vote::NEED),
            announce(1, 57, roll_vote::GREED),
        ] {
            assert_eq!(
                format_line(&RollLine::Announce(p), Some("Bob"), false, LINK, false),
                None,
                "roll_number {} roll_type {}",
                p.roll_number,
                p.roll_type
            );
        }

        // 2 · the WINNER line grows the roll number it would otherwise have left to the dice
        // line, and its NEED/GREED word is decided by `rollType == 1`.
        let mut won = LootRollWon {
            looted_target: 0xAA,
            item_slot: 0,
            item_id: 17182,
            random_property_id: 0,
            winner: 1,
            roll_number: 84,
            roll_type: roll_vote::NEED,
        };
        assert_eq!(
            format_line(&RollLine::Won(won), Some("Bob"), false, LINK, false),
            Some(format!("Bob won: {LINK} |cff818181(Need - 84)|r"))
        );
        assert_eq!(
            format_line(&RollLine::Won(won), None, true, LINK, false),
            Some(format!("You won: {LINK} |cff818181(Need - 84)|r"))
        );
        won.roll_type = roll_vote::GREED;
        assert_eq!(
            format_line(&RollLine::Won(won), Some("Bob"), false, LINK, false),
            Some(format!("Bob won: {LINK} |cff818181(Greed - 84)|r"))
        );
        // "anything that is not need renders as greed" — the discriminator is `== 1`, not a
        // two-way match, so a PASS-typed win (server bookkeeping we never expect) reads Greed.
        won.roll_type = roll_vote::PASS;
        assert!(format_line(&RollLine::Won(won), None, true, LINK, false)
            .unwrap()
            .contains("(Greed - 84)"));

        // 3 · ALL_PASSED is not gated on either side — `0x61b640` reads the CVar not at all.
        let passed = LootAllPassed {
            looted_target: 0xAA,
            item_slot: 0,
            item_id: 17182,
            random_property_id: 0,
        };
        assert_eq!(
            format_line(&RollLine::AllPassed(passed), None, false, LINK, false),
            Some(format!("Everyone passed on: {LINK}"))
        );
    }

    #[test]
    fn start_allocates_ids_and_queues_the_open() {
        let mut r = LootRolls::default();
        r.start(start(0xAA, 0, 17182));
        r.start(start(0xAA, 1, 4306));
        assert_eq!(r.active.len(), 2);
        // Distinct, monotonic ids; each queued a START_LOOT_ROLL with the wire's countdown.
        assert_eq!(r.opened, vec![(1, 60_000), (2, 60_000)]);
        assert_eq!(r.active[0].roll_id, 1);
        assert_eq!(r.active[1].roll_id, 2);
    }

    /// vmangos re-sends every active roll on reconnect (`SendLootStartRollsForPlayer`) — a repeat
    /// for a `(target, slot)` we already hold must not stack a second frame on the same item.
    #[test]
    fn duplicate_start_is_ignored() {
        let mut r = LootRolls::default();
        r.start(start(0xAA, 0, 17182));
        r.start(start(0xAA, 0, 17182));
        assert_eq!(r.active.len(), 1);
        assert_eq!(r.opened, vec![(1, 60_000)]);
    }

    #[test]
    fn resolution_closes_the_matching_roll_only() {
        let mut r = LootRolls::default();
        r.start(start(0xAA, 0, 17182));
        r.start(start(0xAA, 1, 4306));
        r.opened.clear();

        r.won(LootRollWon {
            looted_target: 0xAA,
            item_slot: 0,
            item_id: 17182,
            random_property_id: 0,
            winner: 1,
            roll_number: 84,
            roll_type: roll_vote::NEED,
        });
        assert_eq!(r.cancelled, vec![1]);
        assert_eq!(r.active.len(), 1);
        assert_eq!(r.active[0].item_slot, 1);

        r.all_passed(LootAllPassed {
            looted_target: 0xAA,
            item_slot: 1,
            item_id: 4306,
            random_property_id: 0,
        });
        assert_eq!(r.cancelled, vec![1, 2]);
        assert!(r.active.is_empty());
        // Both queued their chat line.
        assert_eq!(r.pending.len(), 2);
    }

    /// A resolution for a roll we already closed (a duplicate, or one we voted on) is a no-op —
    /// no second CANCEL_LOOT_ROLL for an id no frame holds any more.
    #[test]
    fn resolution_for_a_closed_roll_is_idempotent() {
        let mut r = LootRolls::default();
        r.start(start(0xAA, 0, 17182));
        let won = LootRollWon {
            looted_target: 0xAA,
            item_slot: 0,
            item_id: 17182,
            random_property_id: 0,
            winner: 1,
            roll_number: 84,
            roll_type: roll_vote::NEED,
        };
        r.won(won);
        r.won(won);
        assert_eq!(r.cancelled, vec![1], "only the first close cancels");
    }

    #[test]
    fn our_vote_closes_the_frame_and_yields_the_wire_identity() {
        let mut r = LootRolls::default();
        r.start(start(0xAA, 3, 17182));
        assert_eq!(r.vote(1), Some((0xAA, 3)));
        assert!(r.active.is_empty(), "the frame closes client-predicted");
        assert_eq!(r.cancelled, vec![1]);
        // A stale click on an id nothing holds any more yields nothing to send.
        assert_eq!(r.vote(1), None);
        assert_eq!(r.vote(99), None);
    }

    /// The bar ticks down and saturates — a spent roll stays open (the server closes it), so a
    /// long-running roll must not underflow the countdown.
    #[test]
    fn tick_saturates_without_dropping_the_roll() {
        let mut r = LootRolls::default();
        r.start(start(0xAA, 0, 17182));
        r.tick(20_000);
        assert_eq!(r.active[0].remaining_ms, 40_000);
        r.tick(999_999);
        assert_eq!(r.active[0].remaining_ms, 0);
        assert_eq!(r.active.len(), 1, "the server closes it, not the tick");
    }

    /// Ids are never reused, so a late packet for a closed roll cannot address a fresh one.
    #[test]
    fn ids_are_never_reused() {
        let mut r = LootRolls::default();
        r.start(start(0xAA, 0, 17182));
        r.vote(1);
        r.start(start(0xBB, 0, 4306));
        assert_eq!(r.active[0].roll_id, 2);
    }

    fn entry(roll_id: u32, name: Option<&str>, time_left_ms: u32) -> LootRollEntry {
        LootRollEntry {
            roll_id,
            name: name.map(Into::into),
            texture: name.map(|_| "Interface\\Icons\\INV_Staff_12".into()),
            quantity: 1,
            quality: name.map(|_| 4),
            bind_on_pickup: name.is_some(),
            time_left_ms,
            item_id: 17182,
            // Lands with the name — one template answer fills both (decision 1059).
            link: name.map(|n| crate::ui_items::item_link(17182, n, 4)),
            random_property_id: 0,
        }
    }

    fn state(entries: &[LootRollEntry]) -> LootRollsState {
        LootRollsState {
            rolls: entries.to_vec(),
        }
    }

    /// `UPDATE_LOOT_ROLL` fires only when what a frame *paints* changed. The load-bearing half is
    /// the negative: `time_left_ms` ticks every single frame, so counting it would fire the repaint
    /// 60x a second and make the signal worthless (and the timer bar reads the model directly — it
    /// has never needed an event).
    #[test]
    fn only_a_display_change_asks_for_a_repaint() {
        let flight = state(&[entry(7, None, 60_000)]);
        let landed = state(&[entry(7, Some("Staff of Jordan"), 59_000)]);

        assert_eq!(repainted(&flight, &landed), vec![7], "the template landed");
        assert_eq!(repainted(&landed, &landed), Vec::<u32>::new(), "unchanged");

        // The timer alone, which is what changes on almost every frame.
        let ticked = state(&[entry(7, Some("Staff of Jordan"), 58_000)]);
        assert_eq!(
            repainted(&landed, &ticked),
            Vec::<u32>::new(),
            "a ticking bar is not a repaint"
        );

        // Opened and closed rolls are the START/CANCEL events' business, not this one's.
        let two = state(&[
            entry(7, Some("Staff of Jordan"), 59_000),
            entry(8, None, 60_000),
        ]);
        assert_eq!(repainted(&landed, &two), Vec::<u32>::new(), "8 is new");
        assert_eq!(repainted(&two, &landed), Vec::<u32>::new(), "8 closed");
    }
}
