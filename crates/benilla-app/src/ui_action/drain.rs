//! **Outward** — the two action-bar drains, and the law that decides what a click *does*.
//!
//! - **Use** ([`drain_action_uses`]): a queued `UseAction(n)` becomes wire. A SPELL action goes
//!   through the one cast-send path ([`super::cast_send::send_spell_cast`]); the auto-attack action
//!   (6603) sends `CMSG_ATTACKSWING` at the selection, or acquires the nearest enemy when there is
//!   none; an ITEM action names an *entry*, not a position, so it must first find a copy and then
//!   decide equip-vs-use — [`item_action_route`], the byte-verified two-stage law of decision 0666.
//!   A MACRO action runs its body's lines through the chat-input door (`crate::ui_macro::run`,
//!   decision 0983) — the `0x4f1460` fork of the reference's own `UseAction`.
//! - **Set** ([`drain_action_sets`]): a queued `PickupAction`/`PlaceAction` mutation becomes one
//!   `CMSG_SET_ACTION_BUTTON` per entry (0218 §4: the bar is client-authoritative, there is no
//!   answer packet to lock against, and a drag-swap is two independent sends — never atomic).
//!
//! Both run `.after(UiInput)` so a click's intent goes out the same frame it was made. The two
//! queues are disjoint per gesture, so their relative order does not matter.

use bevy::prelude::*;

use benilla_protocol::messages::{
    ActionButton, ACTION_KIND_ITEM, ACTION_KIND_MACRO, ACTION_KIND_SPELL,
};
use benilla_ui::script::UiScript;

use crate::net::{ClientCommand, NetCommands};

use super::cast_send::{CastCommit, CastLadder};
use super::{attack_mounted_refusal, cast_target, PlayerActions, UiErrorKeys, SPELL_ATTACK};

/// What clicking an ITEM action does, and to which copy — [`item_action_route`]'s verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ItemRoute {
    /// Use this copy — the wire `(bag_index, slot)` plus the instance guid the shared use fork
    /// needs (`ui_items::item_use_command`, decision 0664).
    Use((u8, u8, u64)),
    /// Equip this copy — the same triple.
    Equip((u8, u8, u64)),
    /// No copy anywhere the walk reaches — the click does nothing.
    Nowhere,
}

/// The reference's **two-stage** equip-vs-use decision for an ITEM action, byte-verified (wow-re
/// `action-item-slot.md` §8.1, `0x4e5fdd`–`0x4e5ff7`; decision 0666, which supersedes 0216 §7's
/// guessed one):
///
/// ```text
/// InventoryType == 0            → USE          (a consumable is never equipped)
/// InventoryType != 0, worn      → USE IN PLACE (a copy is in equipment slots 0..18)
/// InventoryType != 0, not worn  → EQUIP        (the full walk finds the copy to equip)
/// ```
///
/// The second stage is the whole point. A ONE-stage `equippable → equip` fork can never *use* an
/// equipped trinket — it re-equips it forever — and before 0666 the walk did not look at the
/// equipment slots at all, so an equipped item's button was simply inert (reproduced live
/// 2026-07-26: `item action 1 (entry 25) not in any bag — skipped`).
///
/// `find` is the inventory walk ([`crate::ui_items::find_item`]) with the entry already bound, so
/// this stays a pure function of the template and the walk's answers — the law is testable
/// without a world.
pub(super) fn item_action_route(
    template: &benilla_protocol::ItemInfo,
    find: impl Fn(crate::ui_items::ItemSearch) -> Option<(u8, u8, u64)>,
) -> ItemRoute {
    let anywhere = |live_charges_only| crate::ui_items::ItemSearch {
        equipment_only: false,
        live_charges_only,
    };
    if template.inventory_type == 0 {
        // The use leg's mode-`0x20` charge filter (`0x4e603a`): only when the TEMPLATE says this
        // item carries finite charges does the search skip spent copies.
        return match find(anywhere(template.has_finite_charges())) {
            Some(pos) => ItemRoute::Use(pos),
            None => ItemRoute::Nowhere,
        };
    }
    if let Some(pos) = find(crate::ui_items::ItemSearch {
        equipment_only: true,
        live_charges_only: false,
    }) {
        return ItemRoute::Use(pos);
    }
    match find(anywhere(false)) {
        Some(pos) => ItemRoute::Equip(pos),
        None => ItemRoute::Nowhere,
    }
}

pub(super) fn drain_action_uses(
    script: Option<NonSendMut<UiScript>>,
    actions: Res<PlayerActions>,
    targeting: cast_target::CastTargeting,
    mut acquire: MessageWriter<crate::target::AttackNearestRequest>,
    // The by-key local error line — the only sink here that is not the ladder's own
    // (`ladder.cast_errors` is the reason-coded one, `ladder.ground` the targeting mode).
    mut ui_errors: ResMut<UiErrorKeys>,
    mut ladder: CastLadder,
) {
    let selection = &targeting.selection;
    let Some(mut script) = script else {
        return;
    };
    for action in script.take_action_uses() {
        let slot = match u8::try_from(action.saturating_sub(1)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        match actions.buttons.get(&slot) {
            Some(b) if b.kind == ACTION_KIND_SPELL && b.action == SPELL_ATTACK => {
                // The mounted attack block ([`attack_mounted_refusal`]): the ref's validator
                // refuses BEFORE the with-target swing and before the nearest-enemy scan
                // (`0x613039` precedes `0x6130b5`), so both arms gate here.
                if attack_mounted_refusal(targeting.self_store.iter().next(), &mut ui_errors) {
                    continue;
                }
                match selection.guid {
                    Some(guid) => {
                        debug!("ui_action: attack swing at {guid:#x}");
                        // Auto-draw: initiating melee requests melee sheath state through the
                        // anim layer's ONE setter (decision 0080) — a SNAP, no ceremony, no
                        // sound: the attack path passes `(newState=1, bInstant=1, bFireEvent=1)`
                        // at `0x5ecd80` (wow-re `sheath-policy.md`). The setter's idempotency
                        // is the client's own "no-op if already melee".
                        if let Ok((e, _)) = ladder.self_player.single() {
                            ladder.sheath.write(crate::creature_anim::SheathRequest {
                                entity: e,
                                state: 1,
                                ceremony: false,
                            });
                        }
                        // Melee attack-start cancels a running auto-repeat UNCONDITIONALLY —
                        // the client's `0x5ecd8c` (wow-re `nocked-ammo-cancel.md` §Q-B-5).
                        let self_e = ladder.self_player.single().ok().map(|(e, _)| e);
                        crate::creature_anim::cancel_auto_repeat_local(
                            self_e,
                            &mut ladder.auto_repeat,
                            &mut ladder.ecs,
                            &ladder.commands,
                        );
                        let _ = ladder.commands.0.send(ClientCommand::AttackSwing { guid });
                    }
                    // No target: the client's attack resolver runs the nearest-enemy core and
                    // swings at the winner (`0x612df0` @ `6130b5`) — `target::scan` answers.
                    None => {
                        debug!("ui_action: attack with no target — acquiring nearest");
                        acquire.write(crate::target::AttackNearestRequest);
                    }
                }
            }
            Some(b) if b.kind == ACTION_KIND_SPELL => {
                // UseAction's toggle-cancel (`0x4e5ee0`: `GetTargetingSpellId 0x6e48e0` +
                // `StopTargeting 0x6e4900`, decision 0792): re-pressing the spell whose
                // targeting cursor is up cancels the targeting instead of re-arming it —
                // press-again-to-cancel, before TryCast ever runs. (A spellbook re-press stays
                // the ref's abort-and-re-enter — it never passes through UseAction.)
                if ladder.ground.spell() == Some(b.action) {
                    debug!(
                        "ui_action: cast {} re-pressed — targeting toggles off",
                        b.action
                    );
                    ladder.ground.clear();
                    continue;
                }
                // The active-action toggle (`0x4e55f0` → the `0x4e60c1` cancel; wow-re
                // `shapeshift-plaincast-toggle.md`): a live ActiveIconID spell re-pressed on
                // its button cancels its own aura — Ghost Wolf, the druid forms, Stealth. The
                // form-match toggle is deliberately NOT here (the ref's `UseAction` has no such
                // leg — the `CastSpell` dispatcher alone carries it; keep the asymmetry).
                if let Some(d) = ladder.spells.as_ref().and_then(|s| s.catalog.get(b.action)) {
                    if let Some(store) = targeting.self_store.iter().next() {
                        if super::toggle::active_action_toggle(b.action, d, store) {
                            debug!("ui_action: cast {} re-pressed — aura cancels", b.action);
                            let _ = ladder
                                .commands
                                .0
                                .send(crate::net::ClientCommand::CancelAura { spell_id: b.action });
                            continue;
                        }
                    }
                }
                debug!("ui_action: cast {} (target {:?})", b.action, selection.guid);
                ladder.send(b.action, &targeting.context(), CastCommit::Spell);
            }
            // An item action names an item ENTRY, not a position, so the click has to find a copy
            // — [`item_action_route`] is that law. A miss (the copy left the bags between the
            // click and this drain, or a stale action from a previous session) is a
            // debug-log-and-skip, NOT the red error line: nothing was attempted against the
            // server, so "Item is not ready" would be a lie.
            Some(b) if b.kind == ACTION_KIND_ITEM => {
                let Some(store) = targeting.self_store.iter().next() else {
                    continue;
                };
                let template = ladder
                    .items
                    .template(b.action, 0, &ladder.commands)
                    .cloned();
                // The reference reads the template first and bails on a null record; ours is all
                // but always cached by click time (the icon resolve needed it).
                let Some(template) = template else {
                    debug!(
                        "ui_action: item action {action} (entry {}) has no template yet — skipped",
                        b.action
                    );
                    continue;
                };
                let route = item_action_route(&template, |s| {
                    crate::ui_items::find_item(&store.0, &ladder.items, b.action, s)
                });
                let ((bag_index, slot0, guid), equip) = match route {
                    ItemRoute::Use(pos) => (pos, false),
                    ItemRoute::Equip(pos) => (pos, true),
                    ItemRoute::Nowhere => {
                        debug!(
                            "ui_action: item action {action} (entry {}) is nowhere in the inventory — skipped",
                            b.action
                        );
                        continue;
                    }
                };
                if equip {
                    // Deliberately WITHOUT the bag click's quest guard: the bar's own engine tests
                    // only `[rec+0x2c]` (inventoryType) before the equip route (`0x4e5fdd`), where
                    // `Script::UseContainerItem` also tests `StartQuest` (`0x4fa3c4`) — so an
                    // equippable quest-starter on the bar equips, exactly as the reference does
                    // (decision 0664).
                    debug!("ui_action: item action {action} auto-equip (wire {bag_index}/{slot0})");
                    let _ = ladder.commands.0.send(ClientCommand::AutoEquipItem {
                        bag_index,
                        slot: slot0,
                    });
                } else {
                    // …then the shared use fork (`CGItem::Use` — the bar's engine calls the very
                    // same function at `0x4e607b`), so a quest-starter on the bar offers its quest
                    // instead of a `CMSG_USE_ITEM` the server can only refuse (decision 0664). The
                    // wire's third byte is the spell BLOCK ordinal, not a flag (decision 0666).
                    // The fork runs the WHOLE cast ladder — an item use IS a cast through the same
                    // `TryCast` (decisions 0908/0914; [`crate::ui_items::send_item_use`] is the
                    // law), so the cooldown/GCD/in-flight/mounted/moving/form rungs and the local
                    // "Item is not ready yet." live there now, for the bag and doll clicks too.
                    let spell_index = template.use_spell_index().unwrap_or(0);
                    debug!(
                        "ui_action: item action {action} use (wire {bag_index}/{slot0}, spell #{spell_index})"
                    );
                    crate::ui_items::send_item_use(
                        crate::ui_items::ItemUse {
                            guid: Some(guid),
                            start_quest: template.start_quest,
                            bag_index,
                            slot: slot0,
                            entry: b.action,
                            spell_index,
                            use_spell: template.use_spell.map(|u| u.spell_id),
                            on_object: None,
                        },
                        &targeting.context(),
                        &mut ladder,
                    );
                }
            }
            // The MACRO arm (`0x4e5ee0`'s `and ecx,0xbfffffff; call 0x4f1460` fork, wow-re
            // `action-item-slot.md` §8): run the macro's body. Every line goes onto the chat-input
            // queue — the door a typed line comes through — so `/cast`, `/target`, `/script`, the
            // chat types and the 225 emotes all work in a macro by construction
            // (`crate::ui_macro::run`'s module doc).
            Some(b) if b.kind == ACTION_KIND_MACRO => {
                if !crate::ui_macro::run_macro(&mut script, b.action) {
                    debug!(
                        "ui_action: macro action {action} (macro {}) is empty",
                        b.action
                    );
                }
            }
            Some(b) => {
                debug!(
                    "ui_action: action {action} kind {:#04x} has no use path",
                    b.kind
                );
            }
            None => debug!("ui_action: UseAction({action}) on an empty slot"),
        }
    }
}

/// Drain the `(lua action id, packed)` pairs the cursor seam's `PickupAction`/`PlaceAction`
/// queued (decision 0216 §7) — the engine's own local mutation already agrees with what lands
/// here (it wrote the same value into its optimistic `model.actions` mirror before queuing this).
/// Each entry: write `PlayerActions.buttons` (`packed == 0` removes the slot, else inserts),
/// mark `dirty` so [`super::feed::feed_actions`] re-resolves + re-pushes + fires
/// `ACTIONBAR_SLOT_CHANGED` (the existing diff machinery — no bespoke event here), and send ONE
/// `CMSG_SET_ACTION_BUTTON` (0218 §4: client-authoritative, no answer packet, a drag-swap is two
/// independent sends).
pub(super) fn drain_action_sets(
    script: Option<NonSendMut<UiScript>>,
    mut actions: ResMut<PlayerActions>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for (lua_id, packed) in script.take_action_sets() {
        let Ok(slot) = u8::try_from(lua_id.saturating_sub(1)) else {
            debug!("ui_action: set_action_button lua id {lua_id} out of range — ignored");
            continue;
        };
        if packed == 0 {
            actions.buttons.remove(&slot);
        } else {
            actions.buttons.insert(
                slot,
                ActionButton {
                    slot,
                    action: packed & 0x00FF_FFFF,
                    kind: (packed >> 24) as u8,
                },
            );
        }
        actions.dirty = true;
        debug!(
            "ui_action: set_action_button lua {lua_id} (wire slot {slot}) packed {packed:#010x}"
        );
        let _ = commands.0.send(ClientCommand::SetActionButton {
            button: slot,
            packed,
        });
    }
}
