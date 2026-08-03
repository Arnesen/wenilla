//! The app-side **inspect feed** (decision 0631) — the bridge that turns another player's PUBLIC
//! descriptor into the slot views the inspect window's Lua reads, and the one place the
//! `CanInspect` range gate is computed.
//!
//! This is [`crate::ui_char`]'s pattern turned onto a *foreign* unit, and the difference between
//! the two is the whole reason this module exists rather than a flag on that one:
//!
//! | | the character window | the inspect window |
//! |---|---|---|
//! | source | `PLAYER_FIELD_INV_SLOT_*` guids (PRIVATE) | `PLAYER_VISIBLE_ITEM_*` entries (PUBLIC) |
//! | resolves via | item **objects** → templates | templates only |
//! | so it can show | counts, durability, locks, creator | icon, name, quality |
//!
//! Other players' inventory guids are server-private, so there are no item objects to read — the
//! visible-item entry is the only path (`entities::equipment`'s header records the same finding for
//! rendering their gear). That is exactly why the reference's inspect paper doll shows no stack
//! counts and no durability: the client has none either.
//!
//! Four jobs, each frame, before the VM ticks:
//!
//! - **Feed the reach map.** For every popup token that resolves to a live, inspectable player, its
//!   squared distance from us — the input to both verified range predicates (`CanInspect` and
//!   `CheckInteractDistance`), which the VM cannot compute because it holds no positions. Computed
//!   for party tokens as well as `"target"`, which is why it can't ride `UnitState` (only
//!   player/target/mouseover get one).
//! - **Drain the intents.** `NotifyInspect(unit)` → resolve the token to a player guid → send
//!   `CMSG_INSPECT` and latch the token as the inspect target. `ClearInspectPlayer()` → drop it.
//!   The window does not wait for `SMSG_INSPECT` (it carries only the echoed guid, and the ref
//!   paints immediately) — the request exists because server-side it also sets our selection.
//! - **Resolve the view.** While a target is latched: re-resolve the **token** each frame (so the
//!   window follows a re-target exactly as the ref's does, `InspectFrame_OnEvent`'s
//!   `PLAYER_TARGET_CHANGED` arm), then read the 19 visible-item entries → the ask-once item
//!   template cache → icon/name/quality/link, and push it on change. A slot whose template answer
//!   is still in flight carries its `item_id` with `icon: None` and fills on a later frame, the
//!   `ui_char` rule.
//! - **Point the booth.** The `"inspect"` body booth ([`InspectBooth`]) gets the resolved entity
//!   and the pane's yaw, so it bakes the inspected player's dressed look (decision 0631 §4).
//!
//! Events, fired on transitions for the **inspected token** (the ref's own registration set,
//! `InspectPaperDollFrame.lua:2-5` + `l.82`): `UNIT_INVENTORY_CHANGED` when any slot view changes,
//! and `UNIT_LEVEL` when the unit's level does. A first resolve counts as a transition (the
//! `ui_unit` rule).

use bevy::prelude::*;

use benilla_ui::script::{InspectView, InvSlotView, InventorySlots, ScriptValue, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{ClientCommand, GuidIndex, NetCommands, ObjectStore};
use crate::portrait::InspectBooth;
use crate::ui_items::item_link;
use crate::ui_script::UiInput;

/// The unit tokens a UnitPopup row can name for another player — the popup menus' own set
/// (`UnitPopupMenus["PLAYER"]` is driven by the target frame, `["PARTY"]` by the four party
/// frames). The reach map below is computed for exactly these each frame.
const REACH_TOKENS: [&str; 5] = ["target", "party1", "party2", "party3", "party4"];

/// The squared distance between two world positions, in the binary's own accumulation shape: `f32`
/// inputs widened to `f64`, summed `(dz² + dx²) + dy²` (wow-re's transcription of
/// `0x48a26f..0x48a27d`, the kernel `caninspect_dist2` and `check_interact_dist2` share).
///
/// Our axes are Bevy's rather than the client's WoW triple. d² is invariant under that rotation, so
/// the only conceivable divergence from the binary is a last-ulp one — which can only change the
/// verdict for a unit standing *exactly* on a threshold. The thresholds and comparison operators,
/// which are what actually decide each gate, are transcribed exactly at the two bindings
/// (`benilla_ui::script::inspect`).
fn dist_sq(q: Vec3, p: Vec3) -> f64 {
    let dx = f64::from(q.x) - f64::from(p.x);
    let dy = f64::from(q.y) - f64::from(p.y);
    let dz = f64::from(q.z) - f64::from(p.z);
    (dz * dz + dx * dx) + dy * dy
}

/// Which unit the inspect window is bound to — the ref's `InspectFrame.unit`, mirrored app-side so
/// the feed knows whether to resolve anything. The **token** is the identity (not the guid): the
/// reference re-reads the token on every retarget, so `"target"` follows the selection.
#[derive(Resource, Default)]
pub(crate) struct InspectTarget {
    pub(crate) token: Option<String>,
}

/// The last pushed view + the last level seen, so the feed pushes and fires only on real change
/// (the `ui_char`/`ui_unit` discipline).
#[derive(Resource, Default)]
struct InspectFeedState {
    last: Option<InspectView>,
    last_level: Option<u32>,
}

pub(crate) struct InspectUiPlugin;

impl Plugin for InspectUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InspectTarget>()
            .init_resource::<InspectFeedState>()
            .add_systems(Update, feed_inspect.in_set(UiInput));
    }
}

/// One inspected equipment slot, resolved from the target's PUBLIC visible-item entry. `slot0` is
/// the 0-based `EQUIPMENT_SLOT_*` index; the live-API id the Lua sees is `slot0 + 1`.
///
/// Everything an item *object* would supply is absent by construction (see the module doc), so the
/// count is the ref's own always-1 and durability/flags/locks/creator/equip-fit stay at their inert
/// defaults — an inspected item is not draggable, lockable, or repairable.
///
/// The **enchant lines are the one exception** (decision 0915): a unit's descriptor broadcasts the
/// enchants of what it wears — `PLAYER_VISIBLE_ITEM_<slot>_0 + 1 + j` — so an inspected weapon's
/// enchant is readable without any object. 1.12 fills exactly two of those seven slots (PERM,
/// TEMP: vmangos `SetVisibleItemSlot`'s `MAX_INSPECTED_ENCHANTMENT_SLOT`), and that is the same
/// data the reference's own inspect tooltip has.
fn inspect_slot_view(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    enchant_rows: Option<&crate::items::Enchants>,
    commands: &NetCommands,
    slot0: u8,
) -> Option<InvSlotView> {
    let entry = store.0.player_visible_item_entry(slot0)?;
    // Template-only ask (`guid` 0 — `Items::template`'s own documented shape): there is no item
    // object to name, and asking by entry is precisely what the real client's ItemCache does.
    let (name, quality, display) = match items.template(entry, 0, commands) {
        Some(t) => (Some(t.name.clone()), t.quality, t.display_info_id),
        None => (None, 0, 0),
    };
    let link = name.as_ref().map(|n| item_link(entry, n, quality));
    Some(InvSlotView {
        item_id: entry,
        icon: icons
            .and_then(|i| i.catalog.get(display))
            .and_then(|d| d.icon.clone()),
        count: 1,
        quality: quality as i32,
        name,
        link,
        // All 7 slots, exactly as the reference's own inspect leg copies and renders them
        // (§E7) — a 1.12 server happens to fill only PERM and TEMP. No item object here, so no
        // charges and no `SMSG_ITEM_ENCHANT_TIME_UPDATE` countdown: the reference's inspect
        // tooltip has neither either.
        enchants: crate::items::enchant_lines(
            (0..7).map(|j| {
                let id = store.0.player_visible_item_enchant(slot0, j).unwrap_or(0);
                (j, id as i32, 0, None)
            }),
            enchant_rows,
        ),
        ..Default::default()
    })
}

#[allow(clippy::too_many_arguments)]
fn feed_inspect(
    script: Option<NonSendMut<UiScript>>,
    mut target: ResMut<InspectTarget>,
    mut feed: ResMut<InspectFeedState>,
    mut booth: ResMut<InspectBooth>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    // `SpellItemEnchantment`'s name column — the inspected item's enchant line (decision 0915).
    enchants: Option<Res<crate::items::Enchants>>,
    commands: Res<NetCommands>,
    index: Res<GuidIndex>,
    stores: Query<&ObjectStore>,
    selection: Res<crate::target::Selection>,
    group: Res<crate::ui_party::GroupState>,
    // The range gate's inputs (the same Transform distance `ui_session`'s service-range check uses)
    // plus the faction pair `can_attack` needs.
    self_q: Query<(&Transform, &ObjectStore), With<crate::net::SelfPlayer>>,
    transforms: Query<&Transform>,
    factions: Option<Res<crate::target::Factions>>,
    reputations: Res<crate::net::Reputations>,
) {
    let Some(mut script) = script else {
        return;
    };

    // The reach map: for every popup token that resolves to a live, inspectable player, its squared
    // distance from us. A token is entered ONLY if it passes the two non-distance refusals vmangos
    // makes — the target is a player (`sObjectMgr.GetPlayer`) and is not attackable
    // (`IsValidAttackTarget`, `MiscHandler.cpp:945-956`). That the *client* checks those two is
    // INFERRED (the 348-byte `0x48a1b0`'s non-math part isn't in the RE record), but a wrong guess
    // can only cost a request the server would drop. Absent from the map = the bindings' in-range
    // default, so a token we simply can't resolve never grays a row.
    let self_pair = self_q.iter().next();
    let mut reach = std::collections::HashMap::new();
    if let Some((self_tf, self_store)) = self_pair {
        for token in REACH_TOKENS {
            let Some(guid) = crate::ui_unit::player_token_guid(token, &selection, &group) else {
                continue;
            };
            let Some(&entity) = index.0.get(&guid) else {
                continue;
            };
            let store = stores.get(entity).ok();
            if crate::target::can_attack(store, factions.as_deref(), &reputations, Some(self_store))
            {
                continue;
            }
            if let Ok(tf) = transforms.get(entity) {
                reach.insert(
                    token.to_string(),
                    dist_sq(tf.translation, self_tf.translation),
                );
            }
        }
    }
    script.set_inspect_reach(reach);

    // NotifyInspect(unit) → CMSG_INSPECT + latch the token. Resolving here (not in Lua) is what
    // lets the window bind to a token while the wire speaks guids.
    for token in script.take_inspect_notifies() {
        match crate::ui_unit::player_token_guid(&token, &selection, &group) {
            Some(guid) => {
                debug!("inspect: {token:?} -> {guid:#x}; sending CMSG_INSPECT");
                let _ = commands.0.send(ClientCommand::Inspect { target: guid });
                target.token = Some(token);
            }
            None => {
                debug!("inspect: {token:?} did not resolve to a player guid — nothing sent");
            }
        }
    }
    // ClearInspectPlayer() — the ref's InspectFrame_OnHide. Drop everything so the resolve below
    // stops and the booth empties.
    if script.take_inspect_clear() {
        target.token = None;
    }

    // Re-resolve the LATCHED TOKEN every frame: `"target"` now means whoever is selected now, which
    // is how the reference's retarget arm works (module doc).
    let resolved = target.token.as_ref().and_then(|token| {
        let guid = crate::ui_unit::player_token_guid(token, &selection, &group)?;
        let entity = *index.0.get(&guid)?;
        Some((token.clone(), guid, entity))
    });

    let Some((token, guid, entity)) = resolved else {
        // Nothing inspected (or the target isn't streamed): clear the view and empty the booth.
        if feed.last.is_some() {
            script.set_inspect(None);
            feed.last = None;
            feed.last_level = None;
        }
        booth.unit = None;
        return;
    };

    booth.unit = Some(entity);
    booth.yaw = script.inspect_yaw();

    let Ok(store) = stores.get(entity) else {
        return;
    };
    let mut slots: InventorySlots = Default::default();
    for slot in 1..=19u8 {
        slots[usize::from(slot)] = inspect_slot_view(
            store,
            &mut items,
            icons.as_deref(),
            enchants.as_deref(),
            &commands,
            slot - 1,
        );
    }
    let view = InspectView {
        unit: token.clone(),
        guid,
        slots,
    };
    if feed.last.as_ref() != Some(&view) {
        script.set_inspect(Some(view.clone()));
        feed.last = Some(view);
        // The ref's InspectPaperDollItemSlotButton_OnEvent listens for exactly this, filtered on
        // arg1 == the inspected unit.
        script.fire_event(
            "UNIT_INVENTORY_CHANGED",
            vec![ScriptValue::Str(token.clone())],
        );
    }
    let level = store.0.unit_level();
    if feed.last_level != level {
        feed.last_level = level;
        script.fire_event("UNIT_LEVEL", vec![ScriptValue::Str(token)]);
    }
}
