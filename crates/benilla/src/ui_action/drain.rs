//! **Outward** — the two action-bar drains, and the law that decides what a click *does*.
//!
//! - **Use** ([`drain_action_uses`]): a queued `UseAction(n)` becomes wire. A SPELL action goes
//!   through the one cast-send path ([`super::cast_send::send_spell_cast`]); the auto-attack action
//!   (6603) sends `CMSG_ATTACKSWING` at the selection, or acquires the nearest enemy when there is
//!   none; an ITEM action names an *entry*, not a position, so it must first find a copy and then
//!   decide equip-vs-use — [`item_action_route`], the byte-verified two-stage law of decision 0666.
//!   Macro actions are a stated gap (no macro window yet).
//! - **Set** ([`drain_action_sets`]): a queued `PickupAction`/`PlaceAction` mutation becomes one
//!   `CMSG_SET_ACTION_BUTTON` per entry (0218 §4: the bar is client-authoritative, there is no
//!   answer packet to lock against, and a drag-swap is two independent sends — never atomic).
//!
//! Both run `.after(UiInput)` so a click's intent goes out the same frame it was made. The two
//! queues are disjoint per gesture, so their relative order does not matter.

use std::time::Instant;

use bevy::prelude::*;

use benilla_protocol::messages::{ActionButton, ACTION_KIND_ITEM, ACTION_KIND_SPELL};
use benilla_ui::script::UiScript;

use crate::items::Items;
use crate::net::{ClientCommand, NetCommands, SelfPlayer};

use super::cast_send::send_spell_cast;
use super::{
    attack_mounted_refusal, cast_target, AutoRepeatActive, CastErrors, PlayerActions, Spells,
    UiErrorKeys, SPELL_ATTACK,
};

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

#[allow(clippy::too_many_arguments)] // a Bevy system's full input set
pub(super) fn drain_action_uses(
    script: Option<NonSendMut<UiScript>>,
    actions: Res<PlayerActions>,
    targeting: cast_target::CastTargeting,
    commands: Res<NetCommands>,
    self_player: Query<(Entity, Has<crate::creature_anim::Engaged>), With<SelfPlayer>>,
    mut items: ResMut<Items>,
    mut sheath: MessageWriter<crate::creature_anim::SheathRequest>,
    mut acquire: MessageWriter<crate::target::AttackNearestRequest>,
    spells: Option<Res<Spells>>,
    mut pending: ResMut<crate::ui_cast::PendingCast>,
    mut queued_melee: ResMut<crate::ui_cast::QueuedMeleeSpell>,
    mut cooldowns: ResMut<crate::cooldowns::Cooldowns>,
    // The error sinks, one tuple param (Bevy's 16-param ceiling): the reason-coded cast line
    // + the by-key local line.
    mut errors: (ResMut<CastErrors>, ResMut<UiErrorKeys>),
    mut auto_repeat: ResMut<AutoRepeatActive>,
    mut trade_skill_opens: ResMut<crate::ui_tradeskill::TradeSkillOpens>,
    mut ecs: Commands,
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
                if attack_mounted_refusal(targeting.self_store.iter().next(), &mut errors.1) {
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
                        if let Ok((e, _)) = self_player.single() {
                            sheath.write(crate::creature_anim::SheathRequest {
                                entity: e,
                                state: 1,
                                ceremony: false,
                            });
                        }
                        // Melee attack-start cancels a running auto-repeat UNCONDITIONALLY —
                        // the client's `0x5ecd8c` (wow-re `nocked-ammo-cancel.md` §Q-B-5).
                        let self_e = self_player.single().ok().map(|(e, _)| e);
                        crate::creature_anim::cancel_auto_repeat_local(
                            self_e,
                            &mut auto_repeat,
                            &mut ecs,
                            &commands,
                        );
                        let _ = commands.0.send(ClientCommand::AttackSwing { guid });
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
                debug!("ui_action: cast {} (target {:?})", b.action, selection.guid);
                send_spell_cast(
                    b.action,
                    &targeting.context(),
                    &commands,
                    &self_player,
                    spells.as_deref(),
                    &items,
                    &mut sheath,
                    &mut ecs,
                    &mut pending,
                    &mut queued_melee,
                    &mut cooldowns,
                    &mut errors.0,
                    &mut auto_repeat,
                    &mut trade_skill_opens,
                );
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
                let template = items.template(b.action, 0, &commands).cloned();
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
                    crate::ui_items::find_item(&store.0, &items, b.action, s)
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
                    let _ = commands.0.send(ClientCommand::AutoEquipItem {
                        bag_index,
                        slot: slot0,
                    });
                } else {
                    // The item twin of the cast path's local not-ready refusal
                    // (`IsItemOnCooldown 0x6e2fc0`: the on-use spell against the cooldown list)
                    // — reason 0x28 is the client's "Item is not ready yet.".
                    let on_cooldown = template.use_spell.filter(|u| {
                        cooldowns.is_on_cooldown(
                            u.spell_id,
                            spells.as_ref().and_then(|s| s.catalog.get(u.spell_id)),
                            Instant::now(),
                        )
                    });
                    if let Some(u) = on_cooldown {
                        debug!("ui_action: item action {action} refused locally — on cooldown");
                        errors.0 .0.push((u.spell_id, 0x28));
                        continue;
                    }
                    // …then the shared use fork (`CGItem::Use` — the bar's engine calls the very
                    // same function at `0x4e607b`), so a quest-starter on the bar offers its quest
                    // instead of a `CMSG_USE_ITEM` the server can only refuse (decision 0664). The
                    // wire's third byte is the spell BLOCK ordinal, not a flag (decision 0666).
                    let spell_index = template.use_spell_index().unwrap_or(0);
                    debug!(
                        "ui_action: item action {action} use (wire {bag_index}/{slot0}, spell #{spell_index})"
                    );
                    let _ = commands.0.send(crate::ui_items::item_use_command(
                        Some(guid),
                        template.start_quest,
                        bag_index,
                        slot0,
                        spell_index,
                    ));
                }
            }
            Some(b) => {
                debug!(
                    "ui_action: action {action} kind {:#04x} not castable yet (macro)",
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
