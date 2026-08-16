//! The app-side **gossip feed** (decision 0081 phase 3) — the inward half of the gossip seam around
//! [`benilla_ui::script`]'s `gossip` module, the exact twin of [`crate::ui_items`]'s container seam.
//!
//! The net bridge fills [`GossipState`] from the wire (`SMSG_GOSSIP_MESSAGE` → options + an ask-once
//! `CMSG_NPC_TEXT_QUERY` for the greeting; `SMSG_NPC_TEXT_UPDATE` → the greeting; `SMSG_GOSSIP_COMPLETE`
//! → cleared). Each frame [`feed_gossip`] turns that state into a [`GossipMenu`] snapshot, pushes it
//! into the VM ([`benilla_ui::script::UiScript::set_gossip`]), and fires `GOSSIP_SHOW` on open (or a
//! content change — the async greeting landing) / `GOSSIP_CLOSED` on clear. [`drain_gossip`] pulls the
//! Lua intents back out: `SelectGossipOption` → [`ClientCommand::GossipSelectOption`] (mapped through
//! the wire option `index`; coded options are guarded, never sent — decision 0081), and `CloseGossip`
//! → a local clear (vanilla's client-side close sends no packet; verified against the 1.12 opcode set
//! — there is no `CMSG_GOSSIP_CLOSE`).

use std::collections::HashMap;

use benilla_protocol::messages::{select_greeting, GossipOption, NpcTextBlock};
use bevy::prelude::*;

use benilla_ui::script::{GossipMenu, GossipOptionView, GossipQuestRow, ScriptValue, UiScript};

use crate::names::NameCache;
use crate::net::{ClientCommand, Guid, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_quest::{row_is_active, row_is_one_click};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};

/// The open gossip menu, filled by the net bridge ([`crate::net`]) and read by [`feed_gossip`]. The
/// ask-once `records` cache keyed by `text_id` (mirroring [`crate::items::Items`]'s template cache)
/// means a revisit to the same NPC never re-queries the text. Cleared on `SMSG_GOSSIP_COMPLETE`,
/// a client-side close, and disconnect; the record cache survives (NPC text is static per id).
///
/// The cache holds the **record**, not a greeting. Which of its 8 blocks greets you is drawn per
/// menu-open from the NPC's gender and a die roll (`benilla_protocol::messages::select_greeting`),
/// exactly as the reference re-draws it in its gossip handler — so the same `text_id` can greet you
/// differently on the next visit, and two NPCs sharing a `text_id` can differ by gender. Caching a
/// resolved string, as this did, froze the first draw for the whole session.
#[derive(Resource, Default)]
pub(crate) struct GossipState {
    /// The NPC whose menu is open; `None` = no menu open.
    pub(crate) npc: Option<u64>,
    /// The open menu's `NpcText` id (drives the text query / cache).
    pub(crate) text_id: u32,
    /// The greeting drawn for THIS menu-open, or `None` while its query is in flight (or when the
    /// record names no line for this NPC — the reference's "Missing gossip text!" path).
    pub(crate) greeting: Option<String>,
    /// The selectable option rows (wire `GossipOption`: `index` echoed on select, `icon`, `coded`,
    /// `message`).
    pub(crate) options: Vec<GossipOption>,
    /// The quest rows riding the same packet — `(quest_id, dialog-status icon, title)`. The gossip
    /// window lists them above the options; a click sends `CMSG_QUESTGIVER_QUERY_QUEST` /
    /// `_COMPLETE_QUEST` (decision 0088).
    pub(crate) quests: Vec<(u32, u32, String)>,
    /// Ask-once NPC-text record cache keyed by `text_id` — the 8 undrawn blocks.
    records: HashMap<u32, Vec<NpcTextBlock>>,
}

impl GossipState {
    /// The cached record for `text_id`, if `CMSG_NPC_TEXT_QUERY` was already answered for it.
    pub(crate) fn cached_record(&self, text_id: u32) -> Option<&[NpcTextBlock]> {
        self.records.get(&text_id).map(Vec::as_slice)
    }

    /// Record an NPC-text answer (`SMSG_NPC_TEXT_UPDATE`) into the ask-once cache.
    pub(crate) fn remember_record(&mut self, text_id: u32, blocks: Vec<NpcTextBlock>) {
        self.records.insert(text_id, blocks);
    }

    /// Draw this menu-open's greeting out of `text_id`'s cached record for an NPC of `npc_gender`
    /// — `None` if the record hasn't arrived yet, or names no line for that gender.
    pub(crate) fn draw_greeting(&self, text_id: u32, npc_gender: u8) -> Option<String> {
        let blocks = self.cached_record(text_id)?;
        select_greeting(blocks, npc_gender, greeting_roll()).map(str::to_string)
    }
}

/// The reference's uniform float in `[1.0, 2.0)` for the greeting draw: it stuffs PRNG bits straight
/// into a mantissa (`(rand & 0x7fffff) | 0x3f800000`), which is what makes the range `[1, 2)` rather
/// than the `[0, 1)` you would expect. We build the same shape from our own source — the *stream*
/// can't match (it is the client's own PRNG, seeded per run) but the distribution is what the law
/// needs.
fn greeting_roll() -> f32 {
    f32::from_bits((rand::random::<u32>() & 0x7f_ffff) | 0x3f80_0000)
}

impl GossipState {
    /// Close the open menu (`SMSG_GOSSIP_COMPLETE` / a client-side close). Keeps the greeting cache.
    pub(crate) fn clear(&mut self) {
        self.npc = None;
        self.text_id = 0;
        self.greeting = None;
        self.options.clear();
        self.quests.clear();
    }

    /// Disconnect: drop the open menu (the greeting cache is static, like item templates — kept),
    /// mirroring [`crate::items::Items::clear_session`].
    pub(crate) fn clear_session(&mut self) {
        self.clear();
    }
}

/// The gossip window's feed + drain (decision 0081), cloned from [`crate::ui_items::UiItemsPlugin`].
pub(crate) struct UiGossipPlugin;

impl Plugin for UiGossipPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GossipState>().add_systems(
            Update,
            (
                // Range-close before the feed so the clear turns into GOSSIP_CLOSED the same frame;
                // push before the input pass so an open/close is on screen the same frame; drain
                // after it so a click's intent goes out the same frame (mirrors ui_items).
                close_npc_session_out_of_range::<GossipState>.before(feed_gossip),
                feed_gossip.before(UiInput),
                drain_gossip.after(UiInput),
            ),
        );
    }
}

/// The gossip menu is an NPC session: the standardized range guard ([`crate::ui_session`])
/// client-side-closes it — the exact `CloseGossip` clear, no packet — when the player walks out of
/// the NPC's service range or the NPC despawns.
impl NpcSession for GossipState {
    fn npc(&self) -> Option<u64> {
        self.npc
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// The lowercase Era `GetGossipOptions()` icon *type* for a wire `GOSSIP_ICON` byte — the value
/// the XML resolves to a `Interface\GossipFrame\<Type>GossipIcon` texture.
///
/// **The byte is a bare index into [`GOSSIP_ICON_TYPES`], with no bounds check anywhere on the
/// client's path** — VERIFIED at the bytes (wow-re `system/ui/scratch/gossip-icon-and-binder-flow.md`,
/// decision 1335): `GetGossipOptions 0x4e28d0` reads the stored byte and does
/// `mov edx,[eax*4 + 0x84b7ac]` straight into a 14-entry pointer table, then `lua_pushstring`s it.
///
/// The old map read the byte through **vmangos's `GossipDef.h` enum *names*** (`INTERACT_1`,
/// `MONEY_BAG`, `TALK`, …), which name a **later** client's icon art. It was wrong for six of the
/// eleven values; 5 — the icon every `GOSSIP_OPTION_INNKEEPER` row in the world DB sends — had no
/// entry at all and fell through to the chat bubble, which is the icon half of B249 (decision 1331).
///
/// Two of the reference's out-of-range behaviours we deliberately do **not** reproduce, because
/// both are its missing guard rather than its design (wow-re's note says so in as many words):
/// index **14** is a NULL in the table, so the reference pushes `nil` as the option's type and the
/// row draws whatever the XML's own fallback gives it — the chat bubble, which is what we return
/// directly. Index **≥15** walks off the end into the gossip Lua *binding* table, pushing API-name
/// literals and `strlen`'d `.text` addresses as type strings, and eventually faults. We clamp.
fn gossip_icon_type(icon: u8) -> &'static str {
    GOSSIP_ICON_TYPES
        .get(icon as usize)
        .copied()
        .unwrap_or("gossip")
}

/// The client's icon-name table verbatim — `0x84b7ac`, 14 entries, indexed by the wire
/// `GOSSIP_ICON` byte (see [`gossip_icon_type`]). Indices 11-13 are genuine `gossip` aliases in the
/// binary, not padding.
///
/// Ten of the eleven distinct names have a `Interface\GossipFrame\<Type>GossipIcon.blp` behind
/// them on the 5875 chain; **`auctioneer` does not** — the table names art the client never shipped
/// (auction houses came with 1.9, the icon later). [`gossip_icon_types_name_the_shipped_art`] pins
/// both halves of that, and the XML deliberately has no `auctioneer` key: a missing texture draws
/// nothing, our fallback draws the bubble, and the bubble is the better wrong answer.
const GOSSIP_ICON_TYPES: [&str; 14] = [
    "gossip",
    "vendor",
    "taxi",
    "trainer",
    "healer",
    "binder",
    "banker",
    "petition",
    "tabard",
    "battlemaster",
    "auctioneer",
    "gossip",
    "gossip",
    "gossip",
];

/// Build the Lua-facing snapshot from [`GossipState`] — `None` when no menu is open.
fn snapshot(state: &GossipState) -> Option<GossipMenu> {
    state.npc?;
    Some(GossipMenu {
        greeting: state.greeting.clone(),
        // Quest rows riding the gossip packet (decision 0088): split active-vs-available by the same
        // predicate the quest window's greeting panel uses — the WIRE ICON. The reference runs the
        // identical `{3,4}` test here, just lazily (`0x4e2430`/`0x4e2580`, behind
        // `GetGossipAvailableQuests`/`GetGossipActiveQuests`) rather than at parse time. Decision 0758.
        quests: state
            .quests
            .iter()
            .map(|(_id, icon, title)| GossipQuestRow {
                title: title.clone(),
                active: row_is_active(*icon),
            })
            .collect(),
        options: state
            .options
            .iter()
            .map(|o| GossipOptionView {
                label: o.message.clone(),
                icon_type: gossip_icon_type(o.icon).into(),
                coded: o.coded,
            })
            .collect(),
    })
}

/// Push the current menu into the VM and fire the open/close events on a transition (or a content
/// change — the greeting arriving a frame after the menu). Diffed against a `Local` memory, exactly
/// like the container feed's per-bag diff.
#[allow(clippy::too_many_arguments)]
fn feed_gossip(
    script: Option<NonSendMut<UiScript>>,
    state: Res<GossipState>,
    self_q: Query<(&ObjectStore, &Guid), With<SelfPlayer>>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    states: Res<crate::world_state::WorldStates>,
    mut last: Local<crate::ui_script::VmMemo<Option<GossipMenu>>>,
    mut last_name: Local<crate::ui_script::VmMemo<Option<String>>>,
    mut last_npc: Local<crate::ui_script::VmMemo<Option<u64>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let last_name = last_name.get(&script);
    let last_npc = last_npc.get(&script);
    let mut fresh = snapshot(&state);
    // Expand the greeting's chat-text macros ($N/$B/$G/$<n>w) client-side, as the real client does.
    if let Some(greeting) = fresh.as_mut().and_then(|m| m.greeting.as_mut()) {
        let player = crate::npc_text::player_identity(&self_q, &mut names, &commands);
        *greeting = crate::npc_text::substitute(
            greeting,
            &crate::npc_text::MacroContext {
                subject: player.as_ref(),
                states: &states,
            },
        );
    }
    // The gossip NPC's name resolves through the NameCache (ask-once — the merchant feed's pattern),
    // delivered as arg1 of GOSSIP_SHOW; `None`/empty while the query is in flight, and the diff below
    // tracks it so a name-only landing still re-fires GOSSIP_SHOW to repaint the title. It rides an
    // event arg rather than a `GossipMenu` field so no benilla-ui engine change is needed for the title.
    let npc_name = state
        .npc
        .and_then(|g| names.resolve(g, &commands).map(str::to_string));
    let name_changed = *last_name != npc_name;
    // A different NPC while the menu is already open is a real close+open (decision 0096 /
    // [`crate::ui_session::npc_switched`]); a cross-window switch is handled by OnHide → CloseX on
    // panel displacement (decision 0095).
    let switched = npc_switched(*last_npc, state.npc);
    if fresh == *last && !name_changed && !switched {
        return;
    }
    script.set_gossip(fresh.clone());
    let name_arg = || vec![ScriptValue::Str(npc_name.clone().unwrap_or_default())];
    if switched {
        // Close the old NPC's menu, open the new: the frame hides then shows, playing both kits.
        // GOSSIP_CLOSED routes through OnHide → CloseGossip (decision 0095), which queues a close
        // intent — consume it so the drain does not clear the menu we just re-opened.
        script.fire_event("GOSSIP_CLOSED", vec![]);
        script.fire_event("GOSSIP_SHOW", name_arg());
        let _ = script.take_gossip_close();
    } else {
        match (&*last, &fresh) {
            // Opened, or the greeting/options/name changed while open → (re)paint via GOSSIP_SHOW.
            (_, Some(_)) => script.fire_event("GOSSIP_SHOW", name_arg()),
            // Closed.
            (Some(_), None) => script.fire_event("GOSSIP_CLOSED", vec![]),
            (None, None) => {}
        }
    }
    *last = fresh;
    *last_name = npc_name;
    *last_npc = state.npc;
}

/// Drain the Lua intents: a selected option → `CMSG_GOSSIP_SELECT_OPTION` (mapped to the wire option
/// `index`; a coded option is guarded and never sent — decision 0081); a close → a local clear (no
/// packet, vanilla).
fn drain_gossip(
    script: Option<NonSendMut<UiScript>>,
    mut state: ResMut<GossipState>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for pos in script.take_gossip_selects() {
        let Some(npc) = state.npc else { continue };
        // Lua position is 1-based; resolve it to the option's wire `index`.
        match pos
            .checked_sub(1)
            .and_then(|i| state.options.get(i as usize))
        {
            Some(opt) if !opt.coded => {
                debug!("ui_gossip: select option {} (index {})", pos, opt.index);
                let _ = commands.0.send(ClientCommand::GossipSelectOption {
                    guid: npc,
                    option: opt.index,
                });
            }
            Some(_) => debug!("ui_gossip: ignoring coded option {pos} (v1 greys it)"),
            None => debug!("ui_gossip: SelectGossipOption({pos}) out of range — ignored"),
        }
    }
    // Quest-row clicks (decision 0088): map the 1-based row to its quest id and send
    // QUERY_QUEST (available → look at/accept) or COMPLETE_QUEST (active → turn-in progress).
    for pos in script.take_gossip_quest_selects() {
        let Some(npc) = state.npc else { continue };
        match pos
            .checked_sub(1)
            .and_then(|i| state.quests.get(i as usize))
        {
            Some((quest_id, icon, _title)) => {
                let (quest, icon) = (*quest_id, *icon);
                let active = row_is_active(icon);
                // Same opcode law as the greeting panel: an active row is always a turn-in; an
                // available row turns into one only when its one-click flag is set.
                let cmd = if active || row_is_one_click(icon) {
                    ClientCommand::QuestgiverComplete { npc, quest }
                } else {
                    ClientCommand::QuestgiverQuery { npc, quest }
                };
                debug!("ui_gossip: quest row {pos} (quest {quest}, icon {icon}, active {active})");
                let _ = commands.0.send(cmd);
            }
            None => debug!("ui_gossip: SelectGossipQuest({pos}) out of range — ignored"),
        }
    }
    if script.take_gossip_close() {
        debug!("ui_gossip: client-side close (no packet)");
        state.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one type string [`GOSSIP_ICON_TYPES`] names that the 5875 install ships no art for.
    /// Test-only: nothing in the client needs to know, because the XML simply has no key for it.
    const GOSSIP_ICON_TYPE_UNSHIPPED: &str = "auctioneer";

    #[test]
    fn snapshot_is_none_until_a_menu_opens() {
        let mut state = GossipState::default();
        assert!(snapshot(&state).is_none());
        state.npc = Some(0x42);
        state.greeting = Some("Hello".into());
        state.options = vec![GossipOption {
            index: 3,
            icon: 1,
            coded: false,
            message: "Browse".into(),
        }];
        let menu = snapshot(&state).expect("open");
        assert_eq!(menu.greeting.as_deref(), Some("Hello"));
        assert_eq!(menu.options.len(), 1);
        assert_eq!(menu.options[0].icon_type, "vendor");
    }

    #[test]
    fn ask_once_text_cache_survives_clear() {
        let mut state = GossipState::default();
        state.remember_record(
            50,
            vec![NpcTextBlock {
                probability: 0.0,
                male: "Greetings $N".into(),
                female: String::new(),
            }],
        );
        state.clear();
        // The open menu is gone but the cached RECORD is still servable (a revisit won't re-query
        // — it re-draws from these blocks).
        assert!(state.cached_record(50).is_some());
        assert_eq!(
            state.draw_greeting(50, 0).as_deref(),
            Some("Greetings $N"),
            "the all-zero record draws block 0 on every roll"
        );
        assert!(state.npc.is_none());
    }

    /// The record is cached, the GREETING is not: a female NPC and a male NPC sharing one `text_id`
    /// must read their own columns. Caching a resolved string (as this did) froze whichever one
    /// asked first for the rest of the session.
    #[test]
    fn one_cached_record_greets_each_gender_from_its_own_column() {
        let mut state = GossipState::default();
        state.remember_record(
            77,
            vec![NpcTextBlock {
                probability: 0.0,
                male: "Well met, friend.".into(),
                female: "Well met, sister.".into(),
            }],
        );
        assert_eq!(
            state.draw_greeting(77, 0).as_deref(),
            Some("Well met, friend.")
        );
        assert_eq!(
            state.draw_greeting(77, 1).as_deref(),
            Some("Well met, sister.")
        );
        assert_eq!(state.draw_greeting(78, 0), None, "record not yet arrived");
    }

    /// The icon byte is an INDEX into the client's own table (`0x84b7ac`, VERIFIED — decision
    /// 1335), and these are the seats that were wrong before 1331. Each is pinned to the
    /// `option_id` the world DB pairs it with, because that pairing is what makes the table
    /// legible: spirit healers send 4, innkeepers 5, bankers 6, petitioners 7, tabard designers 8,
    /// battlemasters 9.
    #[test]
    fn the_icon_byte_indexes_the_clients_own_table() {
        assert_eq!(gossip_icon_type(0), "gossip");
        assert_eq!(gossip_icon_type(1), "vendor"); // GOSSIP_OPTION_VENDOR / _ARMORER
        assert_eq!(gossip_icon_type(2), "taxi"); // _TAXIVENDOR
        assert_eq!(gossip_icon_type(3), "trainer"); // _TRAINER
        assert_eq!(gossip_icon_type(4), "healer"); // _SPIRITHEALER / _SPIRITGUIDE — was "binder"
        assert_eq!(gossip_icon_type(5), "binder"); // _INNKEEPER — B249: was the chat bubble
        assert_eq!(gossip_icon_type(6), "banker"); // _BANKER — was "tabard"
        assert_eq!(gossip_icon_type(7), "petition"); // _PETITIONER — was the chat bubble
        assert_eq!(gossip_icon_type(8), "tabard"); // _TABARDDESIGNER — was the chat bubble
        assert_eq!(gossip_icon_type(9), "battlemaster"); // _BATTLEFIELD
        assert_eq!(gossip_icon_type(10), "auctioneer"); // _AUCTIONEER — was the chat bubble
                                                        // 11-13 are genuine `gossip` aliases in the binary, not padding.
        assert_eq!(gossip_icon_type(11), "gossip");
        assert_eq!(gossip_icon_type(13), "gossip");
        // 14 is a NULL the reference pushes as `nil`, and 15+ walks off its table into the Lua
        // binding array. Both are its missing guard, not its design — we clamp (see the fn's doc).
        assert_eq!(gossip_icon_type(14), "gossip");
        assert_eq!(gossip_icon_type(255), "gossip");
    }

    /// The table must name the art the install actually ships — the check that could not have
    /// passed while the mapping was wrong-by-naming, because the reference builds the path by
    /// concatenation (`GossipFrame.lua:123`:
    /// `SetTexture("Interface\\GossipFrame\\" .. arg[i+1] .. "GossipIcon")`), so a type with no BLP
    /// behind it is a blank row.
    ///
    /// **`auctioneer` is the one exception, and it is the client's, not ours**: index 10 names a
    /// texture 5875 never shipped, so the reference itself draws nothing for it. Asserting the
    /// absence pins that as a fact rather than leaving it as a hole someone later "fixes" by
    /// inventing a path. Skips without client data.
    #[test]
    fn gossip_icon_types_name_the_shipped_art() {
        let data = benilla_formats::wow_data_or_skip!();
        let chain = benilla_formats::open_chain(&data).expect("open chain");
        let blp = |ty: &str| format!("Interface\\GossipFrame\\{ty}GossipIcon.blp");
        for ty in GOSSIP_ICON_TYPES {
            if ty == GOSSIP_ICON_TYPE_UNSHIPPED {
                continue;
            }
            assert!(chain.contains(&blp(ty)), "{} is not on the chain", blp(ty));
        }
        assert!(
            !chain.contains(&blp(GOSSIP_ICON_TYPE_UNSHIPPED)),
            "5875 has grown a {GOSSIP_ICON_TYPE_UNSHIPPED} gossip icon — give the XML its key"
        );
    }
}
