//! The app-side **dressing-room feed** (decision 1060): the bridge between the window's intents
//! (`BenillaDressUpModel_Dress/TryOn/Close`, [`DressUpIntent`]) and the booth's look
//! ([`DressUpPreview`]).
//!
//! Three jobs, each frame, before the VM ticks ([`UiInput`]):
//!
//! - **Apply the intents, in order.** `Dress` drops every substitution (the ref's
//!   `SetUnit("player")` on open and `Dress()` on Reset); `TryOn(id)` records one; `Close` empties
//!   the room. Order matters — `DressUpItem` resets *then* tries on in the same breath.
//! - **Resolve each tried-on item to a display.** An item id is all a `|Hitem:` link carries, so the
//!   display id / inventory type come from the ask-once template cache
//!   ([`Items::template`] — `CMSG_ITEM_QUERY_SINGLE` on a miss, exactly as the reference's
//!   ItemCache does). A substitution whose answer is still in flight stays **pending** and lands on
//!   a later frame: that is the normal case for an item the player has never seen, e.g. one linked
//!   in chat by someone else.
//! - **Compose the look.** The player's own body + appearance + their `PLAYER_VISIBLE_ITEM_*`
//!   displays, with each resolved substitution written into the slot its `InventoryType` maps to
//!   (the shared [`equip_slot`] table — the same one the glue/select preview dresses by, so a
//!   robe lands on the chest and a wand in the ranged hand here exactly as it does there).
//!
//! **What is NOT here:** any notion of *fit*. The reference's dressing room previews whatever it is
//! handed — a plate helm on a mage, a mail chest on a rogue — because it is a look, not an equip:
//! `DressUpItemLink` reaches `TryOn` with no class/level/proficiency check anywhere in the path
//! (`DressUpFrame.lua:2-16`). Ours does the same.

use benilla_protocol::CharEnumItem;
use benilla_ui::script::{DressUpIntent, UiScript};
use bevy::prelude::*;

use crate::entities::equip_slot;
use crate::items::Items;
use crate::net::{NetCommands, NetEntity, ObjectStore, SelfPlayer};
use crate::portrait::{DressUpLook, DressUpPreview};
use crate::ui_script::UiInput;

/// The equipment slots a dressing-room look reads off the player — every rendered slot
/// (`EQUIPMENT_SLOT_*`), which is exactly the set [`equip_slot`] can map an item into.
const LOOK_SLOTS: [u8; 14] = [0, 2, 3, 4, 5, 6, 7, 8, 9, 14, 15, 16, 17, 18];

/// What the room is currently showing: the substitutions applied on top of the player's own gear,
/// and the ones still waiting on a template answer.
#[derive(Resource, Default)]
pub(crate) struct DressUpRoom {
    /// Open? `false` = the window is closed and the booth stays empty (the `Close` intent).
    open: bool,
    /// Resolved substitutions by equipment slot — `(display id, inventory type)`, the same pair the
    /// enum-shaped array carries.
    worn: [Option<CharEnumItem>; 19],
    /// Item ids whose template has not answered yet, oldest first. Retried every frame; a
    /// substitution only becomes visible once its display id is known.
    pending: Vec<u32>,
}

impl DressUpRoom {
    /// Apply one intent (see [`DressUpIntent`]).
    fn apply(&mut self, intent: DressUpIntent) {
        match intent {
            DressUpIntent::Dress => {
                self.open = true;
                self.worn = Default::default();
                self.pending.clear();
            }
            DressUpIntent::TryOn(item) => {
                self.open = true;
                self.pending.push(item);
            }
            DressUpIntent::Close => {
                self.open = false;
                self.worn = Default::default();
                self.pending.clear();
            }
        }
    }

    /// Resolve whatever is still waiting on a template answer. `Items::template` asks once and
    /// answers on a later frame; an id the server never answers for simply stays pending, showing
    /// the player's own gear in that slot rather than a hole.
    fn resolve_pending(&mut self, items: &mut Items, commands: &NetCommands) {
        let mut pending = std::mem::take(&mut self.pending);
        pending.retain(|item| {
            let Some(t) = items.template(*item, 0, commands) else {
                return true; // still in flight — keep asking
            };
            let (display_id, inventory_type) = (t.display_info_id, t.inventory_type as u8);
            if let Some(slot) = equip_slot(inventory_type) {
                self.worn[slot] = Some(CharEnumItem {
                    display_id,
                    inventory_type,
                });
            }
            // A non-worn item (a potion, a bag) resolves to no slot and simply previews nothing —
            // the reference's `TryOn` is equally happy to be handed one.
            false
        });
        self.pending = pending;
    }
}

pub(crate) struct DressUpUiPlugin;

impl Plugin for DressUpUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DressUpRoom>()
            .add_systems(Update, feed_dressup.in_set(UiInput));
    }
}

fn feed_dressup(
    script: Option<NonSendMut<UiScript>>,
    mut room: ResMut<DressUpRoom>,
    mut preview: ResMut<DressUpPreview>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    self_q: Query<(&ObjectStore, &NetEntity), With<SelfPlayer>>,
) {
    let Some(mut script) = script else {
        return;
    };
    for intent in script.take_dressup_intents() {
        room.apply(intent);
    }
    // The pane's rotate buttons own the yaw; the booth mirrors it (the paper doll's own law, 0208 §5).
    preview.yaw = script.dressup_yaw();

    room.resolve_pending(&mut items, &commands);

    let look = match (room.open, self_q.single().ok()) {
        (true, Some((store, net))) => player_look(store, net, &room, &mut items, &commands),
        _ => None,
    };
    if preview.look != look {
        preview.look = look;
    }
}

/// The player's own dressed look with the room's substitutions written in — `None` while the
/// descriptor cannot answer race/sex, or before the body display is known (the frame or two right
/// after entering the world).
fn player_look(
    store: &ObjectStore,
    net: &NetEntity,
    room: &DressUpRoom,
    items: &mut Items,
    commands: &NetCommands,
) -> Option<DressUpLook> {
    let s = &store.0;
    let mut equipment = [CharEnumItem::default(); 19];
    for slot in LOOK_SLOTS {
        let idx = slot as usize;
        // A substitution wins over what the player is actually wearing — that IS the preview.
        if let Some(worn) = room.worn[idx] {
            equipment[idx] = worn;
            continue;
        }
        let Some(entry) = s.player_visible_item_entry(slot).filter(|e| *e != 0) else {
            continue;
        };
        // Template-only ask, like the inspect feed's: the visible-item field carries an item
        // ENTRY, and the display id lives on its template.
        if let Some(t) = items.template(entry, 0, commands) {
            equipment[idx] = CharEnumItem {
                display_id: t.display_info_id,
                inventory_type: t.inventory_type as u8,
            };
        }
    }
    Some(DressUpLook {
        display_id: net.display_id?,
        race: s.unit_race()?,
        sex: s.unit_gender()?,
        skin: s.player_skin().unwrap_or(0),
        face: s.player_face().unwrap_or(0),
        hair_style: s.player_hair_style().unwrap_or(0),
        hair_color: s.player_hair_color().unwrap_or(0),
        facial_hair: s.player_facial_hair().unwrap_or(0),
        equipment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use benilla_protocol::{EntityKind, ItemInfo, ObjectFields};

    use crate::items::test_template;
    use crate::net::ClientCommand;

    /// A self-player descriptor: a human male wearing `entries` (equipment slot → item entry).
    /// The raw field indices are the wire's own (`UNIT_FIELD_BYTES_0` 36, `PLAYER_BYTES` 193,
    /// `PLAYER_VISIBLE_ITEM_1_CREATOR` 258 + 12 per slot, entry at +2) — the same literal-index
    /// idiom the descriptor fixtures elsewhere use, since the constants are crate-private to
    /// benilla-protocol.
    fn player(entries: &[(u8, u32)]) -> ObjectStore {
        let mut pairs = vec![
            (36u16, 1 | 1 << 8),  // race 1 (human), class 1, gender 0 (male)
            (193u16, 3 | 4 << 8), // skin 3, face 4, hair 0, hair colour 0
        ];
        for (slot, entry) in entries {
            pairs.push((258 + 2 + 12 * u16::from(*slot), *entry));
        }
        ObjectStore(ObjectFields::from_pairs(&pairs))
    }

    fn net() -> NetEntity {
        NetEntity {
            kind: EntityKind::Player,
            display_id: Some(49),
            scale: 1.0,
        }
    }

    fn commands() -> (NetCommands, crossbeam_channel::Receiver<ClientCommand>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (NetCommands(tx), rx)
    }

    /// An item template with a known display + inventory type.
    fn worn(name: &str, display_info_id: u32, inventory_type: u32) -> ItemInfo {
        ItemInfo {
            display_info_id,
            inventory_type,
            ..test_template(name)
        }
    }

    /// A try-on replaces exactly its own slot and leaves the rest of the player's gear standing —
    /// the whole point of the room: you see YOUR character in the item, not a mannequin.
    #[test]
    fn a_try_on_substitutes_only_its_own_slot() {
        let (cmds, _rx) = commands();
        let mut items = Items::default();
        // Worn: a chest (slot 4) and a sword in the main hand (slot 15).
        items.insert_template(1000, Some(worn("Worn Chest", 5000, 5)));
        items.insert_template(1500, Some(worn("Worn Sword", 5500, 21)));
        // Tried on: a different chest.
        items.insert_template(2000, Some(worn("Shiny Chest", 7000, 5)));
        let store = player(&[(4, 1000), (15, 1500)]);

        let mut room = DressUpRoom::default();
        room.apply(DressUpIntent::Dress);
        room.apply(DressUpIntent::TryOn(2000));
        room.resolve_pending(&mut items, &cmds);

        let look = player_look(&store, &net(), &room, &mut items, &cmds).expect("a look");
        assert_eq!(
            look.equipment[4].display_id, 7000,
            "the tried-on chest shows"
        );
        assert_eq!(
            look.equipment[15].display_id, 5500,
            "the sword the player is actually holding is untouched"
        );
        assert_eq!(look.race, 1);
        assert_eq!(look.sex, 0);
        assert_eq!((look.skin, look.face), (3, 4));
        assert_eq!(look.display_id, 49, "the player's own body");
    }

    /// Reset (`DressUpModel:Dress()`) drops every substitution — the player's own gear comes back —
    /// and closing the window empties the room entirely.
    #[test]
    fn reset_drops_substitutions_and_close_empties_the_room() {
        let (cmds, _rx) = commands();
        let mut items = Items::default();
        items.insert_template(1000, Some(worn("Worn Chest", 5000, 5)));
        items.insert_template(2000, Some(worn("Shiny Chest", 7000, 5)));
        let store = player(&[(4, 1000)]);

        let mut room = DressUpRoom::default();
        room.apply(DressUpIntent::TryOn(2000));
        room.resolve_pending(&mut items, &cmds);
        assert_eq!(
            player_look(&store, &net(), &room, &mut items, &cmds)
                .unwrap()
                .equipment[4]
                .display_id,
            7000
        );

        room.apply(DressUpIntent::Dress);
        assert_eq!(
            player_look(&store, &net(), &room, &mut items, &cmds)
                .unwrap()
                .equipment[4]
                .display_id,
            5000,
            "Reset puts the player's own chest back on"
        );

        room.apply(DressUpIntent::Close);
        assert!(!room.open, "closing empties the room (the booth goes dark)");
    }

    /// An item whose template has not answered yet stays PENDING rather than previewing nothing:
    /// linked-in-chat items are the normal case here, and the first click on one always misses the
    /// cache. It lands on the frame the answer does — and the ask goes out exactly once.
    #[test]
    fn an_unknown_item_waits_for_its_template_then_lands() {
        let (cmds, rx) = commands();
        let mut items = Items::default();
        items.insert_template(1000, Some(worn("Worn Chest", 5000, 5)));
        let store = player(&[(4, 1000)]);

        let mut room = DressUpRoom::default();
        room.apply(DressUpIntent::TryOn(2000));
        room.resolve_pending(&mut items, &cmds);
        assert_eq!(room.pending, vec![2000], "still waiting on the answer");
        assert_eq!(
            player_look(&store, &net(), &room, &mut items, &cmds)
                .unwrap()
                .equipment[4]
                .display_id,
            5000,
            "until it lands, the player's own gear is what shows"
        );
        // Exactly one query for the unknown entry (`Items` asks once).
        let asks = rx
            .try_iter()
            .filter(|c| matches!(c, ClientCommand::ItemQuery { entry: 2000, .. }))
            .count();
        assert_eq!(asks, 1);

        items.insert_template(2000, Some(worn("Shiny Chest", 7000, 5)));
        room.resolve_pending(&mut items, &cmds);
        assert!(room.pending.is_empty());
        assert_eq!(
            player_look(&store, &net(), &room, &mut items, &cmds)
                .unwrap()
                .equipment[4]
                .display_id,
            7000,
            "the answer landing is what makes it show"
        );
    }

    /// A non-worn item (a potion) is handed to `TryOn` by any ctrl-click on one, and previews
    /// nothing — it maps to no equipment slot. It must not stay pending forever either.
    #[test]
    fn a_non_worn_item_previews_nothing_and_does_not_linger() {
        let (cmds, _rx) = commands();
        let mut items = Items::default();
        items.insert_template(3000, Some(worn("Healing Potion", 9000, 0)));
        let mut room = DressUpRoom::default();
        room.apply(DressUpIntent::TryOn(3000));
        room.resolve_pending(&mut items, &cmds);
        assert!(room.pending.is_empty(), "resolved, just not worn anywhere");
        assert!(room.worn.iter().all(Option::is_none));
    }
}
