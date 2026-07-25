//! The app-side **questgiver feed** (decision 0088) — the inward half of the quest seam around
//! [`benilla_ui::script`]'s `quest` module, the twin of [`crate::ui_gossip`]/[`crate::ui_merchant`].
//!
//! The net bridge fills [`QuestGiver`] from the wire: `SMSG_QUESTGIVER_QUEST_LIST` → the greeting
//! panel, `_QUEST_DETAILS` → the accept panel, `_REQUEST_ITEMS` → the progress panel,
//! `_OFFER_REWARD` → the reward panel, `_QUEST_COMPLETE` → the turn-in result (closes the window),
//! and `SMSG_QUESTGIVER_STATUS` → the per-guid dialog-status store (world markers are a later
//! slice — decision 0088). Each frame [`feed_quest`] resolves the open view into a
//! [`QuestState`] snapshot (item names via the ask-once template cache, icons straight from the
//! wire display id — the merchant's pattern), pushes it ([`UiScript::set_quest`]), and fires the
//! matching FrameXML event (`QUEST_GREETING`/`QUEST_DETAIL`/`QUEST_PROGRESS`/`QUEST_COMPLETE` on a
//! panel change, `QUEST_ITEM_UPDATE` on an in-place content change, `QUEST_FINISHED` on clear).
//! [`drain_quest`] pulls the Lua intents back out and maps each to a `CMSG_QUESTGIVER_*` addressed
//! to `(npc, questId)` from the open view.

use std::collections::HashMap;

use benilla_protocol::messages::{
    QuestDetails, QuestGiverList, QuestOfferReward, QuestRequestItems, QuestRewardItem,
};
use bevy::prelude::*;

use benilla_ui::script::{
    QuestAction, QuestItemView, QuestPanel, QuestState, ScriptValue, UiScript,
};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, Guid, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_quest_log::QuestLog;
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};

/// The open questgiver view — exactly the wire packet that opened the current panel. The feed turns
/// it into the Lua snapshot; the drain reads the npc/quest ids off it for the outbound CMSG.
pub(crate) enum QuestView {
    Greeting(QuestGiverList),
    Detail(QuestDetails),
    Progress(QuestRequestItems),
    Reward(QuestOfferReward),
}

/// The open questgiver window, filled by the net bridge ([`crate::net`]) and read by [`feed_quest`].
/// Cleared on the turn-in result, a client-side close, and disconnect. The `statuses` map is the
/// DIALOG_STATUS store-now/render-later surface (decision 0088) — it survives the window closing
/// (a per-guid fact, like the gossip greeting cache).
#[derive(Resource, Default)]
pub(crate) struct QuestGiver {
    /// Set by the net apply on `SMSG_QUESTGIVER_QUEST_COMPLETE`; drained by the feed
    /// ([`Self::take_completed_fanfare`]).
    pub(crate) completed_fanfare: bool,
    /// The questgiver whose window is open; `None` = no window open.
    pub(crate) npc: Option<u64>,
    /// The open panel's wire view.
    pub(crate) view: Option<QuestView>,
    /// Per-guid dialog status (`SMSG_QUESTGIVER_STATUS`) — the `!`/`?` marker's value, stored for a
    /// later world-marker slice.
    statuses: HashMap<u64, u32>,
}

impl QuestGiver {
    /// Open (or replace) the window with a fresh wire view for `npc`.
    pub(crate) fn open(&mut self, npc: u64, view: QuestView) {
        self.npc = Some(npc);
        self.view = Some(view);
    }

    /// Whether a quest window is currently open (a predicate for callers + the module tests).
    #[allow(dead_code)]
    pub(crate) fn is_open(&self) -> bool {
        self.view.is_some()
    }

    /// Close the open window (turn-in result / client-side close). Keeps the status store.
    /// One-shot: the turn-in result landed (`SMSG_QUESTGIVER_QUEST_COMPLETE`) — the giver feed
    /// drains it into the QUESTCOMPLETED kit (the fanfare the client's C++ fires on this packet,
    /// not any Lua handler).
    pub(crate) fn take_completed_fanfare(&mut self) -> bool {
        std::mem::take(&mut self.completed_fanfare)
    }

    pub(crate) fn clear(&mut self) {
        self.npc = None;
        self.view = None;
    }

    /// Record a dialog status for an NPC guid (store-now; the world marker renders in a later slice).
    pub(crate) fn set_status(&mut self, npc: u64, status: u32) {
        self.statuses.insert(npc, status);
    }

    /// Every stored dialog status, per guid — the overhead-marker renderer's read
    /// ([`crate::quest_markers`]).
    pub(crate) fn statuses(&self) -> &HashMap<u64, u32> {
        &self.statuses
    }

    /// The stored dialog status for `npc`, if any. The store-now half of DIALOG_STATUS
    /// (decision 0088): no consumer yet — the `!`/`?` world marker is a later nameplate slice — so
    /// this accessor is deliberately unused for now.
    #[allow(dead_code)]
    pub(crate) fn status(&self, npc: u64) -> Option<u32> {
        self.statuses.get(&npc).copied()
    }

    /// Disconnect: drop the open window (mirrors the gossip/merchant session clears).
    pub(crate) fn clear_session(&mut self) {
        self.clear();
        self.statuses.clear();
    }
}

pub(crate) struct UiQuestPlugin;

impl Plugin for UiQuestPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<QuestGiver>().add_systems(
            Update,
            (
                // Range-close before the feed so the clear turns into the panel-close the same
                // frame; push before the input pass so an open/close is on screen the same frame;
                // drain after it so a click's intent goes out the same frame (mirrors
                // ui_gossip/merchant).
                close_npc_session_out_of_range::<QuestGiver>.before(feed_quest),
                feed_quest.before(UiInput),
                drain_quest.after(UiInput),
            ),
        );
    }
}

/// The questgiver panel is an NPC session: the standardized range guard ([`crate::ui_session`])
/// client-side-closes it — the same no-packet clear as its close button — when the player walks out
/// of the giver's service range or the giver despawns. The per-guid `statuses` store survives, like
/// every other close.
impl NpcSession for QuestGiver {
    fn npc(&self) -> Option<u64> {
        self.npc
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// The greeting panel's active-vs-available split — decision 0088's deferred item, now resolved by
/// the quest-log slice ([`crate::ui_quest_log`]): a row is ACTIVE iff its quest id currently occupies
/// a `PLAYER_QUEST_LOG` descriptor slot, read live off [`QuestLog`] — never derived from the wire
/// `QUEST_LIST` icon (the old icon-derived guess misclassified an auto-complete AVAILABLE quest
/// carrying a REWARD_REP icon). One helper so the snapshot and the drain's re-walk can't diverge.
pub(crate) fn is_active_quest(quest_id: u32, quest_log: &QuestLog) -> bool {
    quest_log.contains(quest_id)
}

/// Resolve one wire reward/required triple into a Lua-facing [`QuestItemView`]: icon from the wire
/// display id (immediate), name + quality from the ask-once item-template cache (`None`/white while
/// in flight — the row shows a placeholder and fills in, exactly like a bag slot / vendor row).
fn resolve_item(
    it: &QuestRewardItem,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> QuestItemView {
    let template = items.template(it.item_id, 0, commands);
    let name = template.map(|t| t.name.clone());
    let quality = template.map(|t| t.quality).unwrap_or(1);
    let texture = icons
        .and_then(|i| i.catalog.get(it.display_id))
        .and_then(|d| d.icon.clone());
    QuestItemView {
        name,
        texture,
        count: it.count,
        quality,
        item_id: it.item_id,
        usable: true, // v1: soft gray only, server authoritative (decision 0088)
    }
}

fn resolve_items(
    src: &[QuestRewardItem],
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Vec<QuestItemView> {
    src.iter()
        .map(|it| resolve_item(it, items, icons, commands))
        .collect()
}

/// Build the Lua-facing snapshot from the open view — `None` when no window is open. Every
/// server-authored text runs the shared chat-macro substitution (`$N`/`$B`/`$G` —
/// [`crate::npc_text`]): the wire delivers quest text un-expanded, the client substitutes.
fn snapshot(
    giver: &QuestGiver,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
    quest_log: &QuestLog,
    macros: &crate::npc_text::MacroContext,
) -> Option<QuestState> {
    let sub = |t: &str| crate::npc_text::substitute(t, macros);
    Some(match giver.view.as_ref()? {
        QuestView::Greeting(l) => {
            let mut active_titles = Vec::new();
            let mut available_titles = Vec::new();
            for q in &l.quests {
                if is_active_quest(q.quest_id, quest_log) {
                    active_titles.push(q.title.clone());
                } else {
                    available_titles.push(q.title.clone());
                }
            }
            QuestState {
                panel: QuestPanel::Greeting,
                greeting: sub(&l.greeting),
                active_titles,
                available_titles,
                ..Default::default()
            }
        }
        QuestView::Detail(d) => QuestState {
            panel: QuestPanel::Detail,
            title: sub(&d.title),
            body: sub(&d.details),
            objectives: sub(&d.objectives),
            choices: resolve_items(&d.choices, items, icons, commands),
            rewards: resolve_items(&d.rewards, items, icons, commands),
            reward_money: d.money.max(0) as u32,
            ..Default::default()
        },
        QuestView::Progress(p) => QuestState {
            panel: QuestPanel::Progress,
            title: sub(&p.title),
            body: sub(&p.request_text),
            required: resolve_items(&p.required_items, items, icons, commands),
            required_money: p.required_money,
            completable: p.is_complete,
            ..Default::default()
        },
        QuestView::Reward(o) => QuestState {
            panel: QuestPanel::Reward,
            title: sub(&o.title),
            body: sub(&o.offer_text),
            choices: resolve_items(&o.choices, items, icons, commands),
            rewards: resolve_items(&o.rewards, items, icons, commands),
            reward_money: o.money.max(0) as u32,
            ..Default::default()
        },
    })
}

/// The FrameXML event a panel opens with (the ref `QuestFrame.lua` names).
fn panel_event(panel: QuestPanel) -> &'static str {
    match panel {
        QuestPanel::Greeting => "QUEST_GREETING",
        QuestPanel::Detail => "QUEST_DETAIL",
        QuestPanel::Progress => "QUEST_PROGRESS",
        QuestPanel::Reward => "QUEST_COMPLETE",
    }
}

/// Push the current quest view into the VM and fire the FrameXML events on a transition (panel
/// change → the panel's open event; same panel, content changed → `QUEST_ITEM_UPDATE`; closed →
/// `QUEST_FINISHED`). Diffed against a `Local`, exactly like the gossip/merchant feeds. The NPC
/// name rides as arg1 (resolved through the NameCache, ask-once — the merchant's pattern).
#[allow(clippy::too_many_arguments)]
fn feed_quest(
    script: Option<NonSendMut<UiScript>>,
    mut giver: ResMut<QuestGiver>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    commands: Res<NetCommands>,
    mut names: ResMut<NameCache>,
    quest_log: Res<QuestLog>,
    states: Res<crate::world_state::WorldStates>,
    self_q: Query<(&ObjectStore, &Guid), With<SelfPlayer>>,
    mut last: Local<Option<QuestState>>,
    mut last_name: Local<Option<String>>,
    mut last_npc: Local<Option<u64>>,
) {
    let Some(mut script) = script else {
        return;
    };
    // The turn-in fanfare (QUESTCOMPLETED → iQuestComplete.wav): the client's C++ plays it on the
    // QUEST_COMPLETE packet itself — no Lua handler owns it, so the feed queues it directly.
    if giver.take_completed_fanfare() {
        script.queue_sound_kit("QUESTCOMPLETED");
    }
    let player = crate::npc_text::player_identity(&self_q, &mut names, &commands);
    let fresh = snapshot(
        &giver,
        &mut items,
        icons.as_deref(),
        &commands,
        &quest_log,
        &crate::npc_text::MacroContext {
            subject: player.as_ref(),
            states: &states,
        },
    );
    let npc_name = giver
        .npc
        .and_then(|g| names.resolve(g, &commands).map(str::to_string));
    let name_changed = *last_name != npc_name;
    // A different giver while a panel is already open is a real close+open (decision 0096 /
    // [`crate::ui_session::npc_switched`]); a cross-window switch is handled by OnHide → CloseX on
    // panel displacement (decision 0095).
    let switched = npc_switched(*last_npc, giver.npc);
    if fresh == *last && !name_changed && !switched {
        return;
    }
    script.set_quest(fresh.clone());
    let name_arg = || vec![ScriptValue::Str(npc_name.clone().unwrap_or_default())];
    match (&*last, &fresh) {
        (_, Some(f)) if switched => {
            // A different giver → close the old panel, open the new (both kits play). QUEST_FINISHED
            // routes through OnHide → CloseQuest (decision 0095), which queues a `Close` action —
            // drain the pending actions so it does NOT clear the giver we just re-opened. Safe: a
            // switch is net-driven, so no user action is queued this frame to lose.
            script.fire_event("QUEST_FINISHED", vec![]);
            script.fire_event(panel_event(f.panel), name_arg());
            let _ = script.take_quest_actions();
        }
        (_, Some(f)) => {
            // Same panel + already open → an in-place content refresh (a name landed); a new panel
            // (or a fresh open) → the panel's open event.
            let same_panel = last.as_ref().is_some_and(|l| l.panel == f.panel);
            let event = if same_panel {
                "QUEST_ITEM_UPDATE"
            } else {
                panel_event(f.panel)
            };
            script.fire_event(event, name_arg());
        }
        (Some(_), None) => script.fire_event("QUEST_FINISHED", vec![]),
        (None, None) => {}
    }
    *last = fresh;
    *last_name = npc_name;
    *last_npc = giver.npc;
}

/// Drain the Lua intents: the greeting-row selects (map to the row's quest id → QUERY_QUEST for an
/// available quest, COMPLETE_QUEST for an active one) and the button actions (Accept/Continue/Reward
/// → the matching CMSG; Close → a local clear, no packet).
fn drain_quest(
    script: Option<NonSendMut<UiScript>>,
    mut giver: ResMut<QuestGiver>,
    commands: Res<NetCommands>,
    quest_log: Res<QuestLog>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Some(npc) = giver.npc else {
        // Still drain the VM so intents don't queue against a closed window.
        script.take_quest_selects();
        script.take_quest_actions();
        return;
    };

    // Greeting-row selects: resolve the 1-based row to its quest id off the open greeting view.
    for sel in script.take_quest_selects() {
        let Some(QuestView::Greeting(list)) = giver.view.as_ref() else {
            continue;
        };
        // Re-walk the same active/available split the snapshot used (`is_active_quest`, backed by
        // the live quest log — not the wire icon), keeping quest ids this time.
        let (mut active, mut available): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        for q in &list.quests {
            if is_active_quest(q.quest_id, &quest_log) {
                active.push(q.quest_id);
            } else {
                available.push(q.quest_id);
            }
        }
        let pool = if sel.active { &active } else { &available };
        let Some(&quest) = sel.index.checked_sub(1).and_then(|i| pool.get(i as usize)) else {
            debug!("ui_quest: greeting select {sel:?} out of range — ignored");
            continue;
        };
        // Active quest = a turn-in (COMPLETE_QUEST → progress); available = a new quest to look at
        // (QUERY_QUEST → details).
        let cmd = if sel.active {
            ClientCommand::QuestgiverComplete { npc, quest }
        } else {
            ClientCommand::QuestgiverQuery { npc, quest }
        };
        let _ = commands.0.send(cmd);
    }

    // Button actions, addressed to the open view's quest id.
    let view_quest = giver.view.as_ref().and_then(|v| match v {
        QuestView::Detail(d) => Some(d.quest_id),
        QuestView::Progress(p) => Some(p.quest_id),
        QuestView::Reward(o) => Some(o.quest_id),
        QuestView::Greeting(_) => None,
    });
    for action in script.take_quest_actions() {
        match action {
            QuestAction::Close => {
                debug!("ui_quest: client-side close (no packet)");
                giver.clear();
            }
            QuestAction::Accept => {
                if let Some(quest) = view_quest {
                    let _ = commands
                        .0
                        .send(ClientCommand::QuestgiverAccept { npc, quest });
                }
            }
            QuestAction::Continue => {
                if let Some(quest) = view_quest {
                    let _ = commands
                        .0
                        .send(ClientCommand::QuestgiverRequestReward { npc, quest });
                }
            }
            QuestAction::Reward(choice) => {
                if let Some(quest) = view_quest {
                    let _ = commands.0.send(ClientCommand::QuestgiverChooseReward {
                        npc,
                        quest,
                        choice,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::dialog_status;

    fn triple(id: u32) -> QuestRewardItem {
        QuestRewardItem {
            item_id: id,
            count: 1,
            display_id: 100 + id,
        }
    }

    #[test]
    fn greeting_split_by_quest_log() {
        // The real semantics (decision 0088's deferred item, resolved): membership in our quest
        // log, not the wire icon. A quest in the log is ACTIVE regardless of which dialog-status
        // icon the greeting carried for it (the icon-derived guess this replaces misclassified an
        // auto-complete AVAILABLE quest carrying a REWARD_REP icon).
        let mut quest_log = QuestLog::default();
        quest_log.set_active_quests([100]);
        assert!(is_active_quest(100, &quest_log));
        assert!(!is_active_quest(200, &quest_log));
    }

    #[test]
    fn detail_snapshot_carries_text_and_rows() {
        let mut giver = QuestGiver::default();
        giver.open(
            0x42,
            QuestView::Detail(QuestDetails {
                npc: 0x42,
                quest_id: 100,
                title: "A Threat Within".into(),
                details: "Kill kobolds, $N.".into(),
                objectives: "Slay 10.".into(),
                auto_finish: 1,
                choices: vec![triple(2000)],
                rewards: vec![triple(3000)],
                money: 1234,
                reward_spell: 0,
            }),
        );
        assert!(giver.is_open());
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let quest_log = QuestLog::default();
        let player = crate::npc_text::Subject {
            name: "Tri".into(),
            race: 1,
            class: 1,
            gender: 0,
        };
        let snap = snapshot(
            &giver,
            &mut items,
            None,
            &commands,
            &quest_log,
            &crate::npc_text::MacroContext {
                subject: Some(&player),
                states: &crate::world_state::WorldStates::default(),
            },
        )
        .expect("open");
        assert_eq!(snap.panel, QuestPanel::Detail);
        assert_eq!(snap.title, "A Threat Within");
        // The wire text's $N substituted through the shared expander (crate::npc_text).
        assert_eq!(snap.body, "Kill kobolds, Tri.");
        assert_eq!(snap.objectives, "Slay 10.");
        assert_eq!(snap.choices.len(), 1);
        assert_eq!(snap.rewards.len(), 1);
        assert_eq!(snap.reward_money, 1234);
        // Name in flight (no template answer) → nil; quality defaults to white.
        assert!(snap.rewards[0].name.is_none());
        assert_eq!(snap.rewards[0].quality, 1);
    }

    #[test]
    fn progress_snapshot_carries_completability() {
        let mut giver = QuestGiver::default();
        giver.open(
            0x42,
            QuestView::Progress(QuestRequestItems {
                npc: 0x42,
                quest_id: 100,
                title: "A Threat Within".into(),
                request_text: "Bring me the tusks.".into(),
                emote: 0,
                close_on_cancel: 1,
                required_money: 500,
                required_items: vec![triple(2001)],
                is_complete: true,
            }),
        );
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let quest_log = QuestLog::default();
        let snap = snapshot(
            &giver,
            &mut items,
            None,
            &commands,
            &quest_log,
            &crate::npc_text::MacroContext {
                subject: None,
                states: &crate::world_state::WorldStates::default(),
            },
        )
        .unwrap();
        assert_eq!(snap.panel, QuestPanel::Progress);
        assert_eq!(snap.required.len(), 1);
        assert_eq!(snap.required_money, 500);
        assert!(snap.completable);
    }

    #[test]
    fn status_store_survives_close() {
        let mut giver = QuestGiver::default();
        giver.set_status(0x99, dialog_status::AVAILABLE);
        giver.open(
            0x99,
            QuestView::Greeting(QuestGiverList {
                npc: 0x99,
                greeting: "Hi".into(),
                emote_delay: 0,
                emote: 0,
                quests: vec![],
            }),
        );
        giver.clear();
        assert!(!giver.is_open());
        assert_eq!(giver.status(0x99), Some(dialog_status::AVAILABLE));
    }
}
