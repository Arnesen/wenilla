//! **Weapon-icon substitution** — the handful of spells that show an *equipped weapon's* icon
//! instead of their own (decisions 0230 + 0231; wow-re `attack-icon-substitution.md`).
//!
//! Two spells' worth of law, but it is character-level rather than spell-level: the melee
//! auto-attack borrows the main hand's icon (or `Spell-Reset` when unarmed), a ranged auto-repeat
//! shot borrows the ranged slot's (unless it is thrown, which keeps the spell's own face). Both
//! track the *equipped item*, which a weapon swap changes without ever touching the action table —
//! which is why [`super::feed`] refreshes these every frame rather than on the identity gate, and
//! why `ui_spellbook` pre-resolves them once per page.

use benilla_formats::SpellDisplay;

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore};

/// Equipment slot 15 = `EQUIPMENT_SLOT_MAINHAND` (vmangos `EquipmentSlots`).
const EQUIPMENT_SLOT_MAINHAND: u8 = 15;

/// Equipment slot 17 = `EQUIPMENT_SLOT_RANGED` — the ranged helper `0x4e6990`'s read
/// (`[ecx+0x88]`, `0x88 = 17×8`; wow-re `attack-icon-substitution.md` §5).
const EQUIPMENT_SLOT_RANGED: u8 = 17;

/// Weapon subclass 16 = thrown — the ranged icon helper's skip (`0x4e6990`'s `0x5d9f90 == 0x10`
/// test): a thrown weapon never substitutes its icon, so Throw keeps the spell's own face.
const ITEM_SUBCLASS_THROWN: u32 = 16;

/// The client's unarmed/disarmed auto-attack icon (wow-re `attack-icon-substitution.md`, the
/// hardcoded string at `0x84bf58`) — what the melee auto-attack shows when there is no main-hand
/// weapon to borrow from, instead of spell 6603's `Temp` placeholder (decision 0231).
const SPELL_RESET_ICON: &str = "Interface\\Buttons\\Spell-Reset";

/// The equipped main-hand weapon's inventory icon (slot 15 → the item's `ItemDisplayInfo` icon,
/// the chain the bags/paper doll use). `None` when unarmed or the item hasn't streamed yet.
fn main_hand_weapon_icon(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Option<String> {
    let guid = store.0.player_inv_slot(EQUIPMENT_SLOT_MAINHAND)?;
    let entry = items.object(guid)?.object_entry()?;
    let display = items.template(entry, guid, commands)?.display_info_id;
    icons?.catalog.get(display)?.icon.clone()
}

/// The character's melee auto-attack icon (decision 0231; the client's melee helper `0x4e6870`):
/// the **current shapeshift form's own attack face** when its `SpellShapeshiftForm` row carries
/// one (the `+0x34` AttackIconID read, `0x4e68af`–`0x4e68da` — a cat's paw, a bear's swipe; wow-re
/// `action-spell-icon-apis.md` §3.3, closing decision 0231's deferred form case), else the
/// equipped main-hand weapon's icon, else [`SPELL_RESET_ICON`]. Character-level — independent of
/// WHICH auto-attack spell (they all show this), so the spellbook can pre-resolve it once for its
/// whole page.
pub(crate) fn melee_auto_attack_icon(
    store: &ObjectStore,
    forms: &std::collections::HashMap<u32, benilla_formats::ShapeshiftForm>,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> String {
    let form = store.0.unit_shapeshift_form();
    if form != 0 {
        if let Some(icon) = forms
            .get(&u32::from(form))
            .and_then(|f| f.attack_icon.clone())
        {
            return icon;
        }
    }
    main_hand_weapon_icon(store, items, icons, commands)
        .unwrap_or_else(|| SPELL_RESET_ICON.to_string())
}

/// The equipped ranged weapon's inventory icon (slot 17 → `ItemDisplayInfo`), for the ranged
/// icon substitution (`0x4e6990`, decision 0231's deferred case — wow-re
/// `attack-icon-substitution.md` §5): a **thrown** weapon is skipped (the helper's
/// `0x5d9f90 == 0x10` test), and `None` — missing weapon, thrown, or an unstreamed item — falls
/// back to the spell's OWN icon at the caller, never `Spell-Reset` (the helper's `0x4e6a44` null
/// return hands over to the normal SpellIconID path).
pub(crate) fn ranged_weapon_icon(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Option<String> {
    let guid = store.0.player_inv_slot(EQUIPMENT_SLOT_RANGED)?;
    let entry = items.object(guid)?.object_entry()?;
    let template = items.template(entry, guid, commands)?;
    if template.subclass == ITEM_SUBCLASS_THROWN {
        return None;
    }
    let display = template.display_info_id;
    icons?.catalog.get(display)?.icon.clone()
}

/// Whether `spell` substitutes an equipped weapon's icon at all — the two resolvers' shared
/// pre-test (melee: the effect trigger; ranged: the paired attribute bits). The per-frame icon
/// refresh keys on this, so a ranged-weapon swap re-feeds Auto Shot like a main-hand swap
/// re-feeds Attack.
pub(super) fn substitutes_weapon_icon(spell: &SpellDisplay) -> bool {
    spell.is_melee_auto_attack() || spell.ranged_icon_substitution()
}

/// The icon `spell` shows on the action bar when it substitutes an equipped weapon's
/// ([`substitutes_weapon_icon`]): the melee auto-attack shows [`melee_auto_attack_icon`]
/// (weapon or `Spell-Reset`); a ranged auto-repeat shot ([`SpellDisplay::ranged_icon_substitution`])
/// shows [`ranged_weapon_icon`]. `None` for any other spell, for a ranged shot with no
/// substitutable weapon, or when there is no character to read the weapon from — the caller uses
/// the spell's own icon.
pub(super) fn auto_attack_icon(
    spell: &SpellDisplay,
    store: Option<&ObjectStore>,
    forms: &std::collections::HashMap<u32, benilla_formats::ShapeshiftForm>,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Option<String> {
    let store = store?;
    if spell.is_melee_auto_attack() {
        return Some(melee_auto_attack_icon(store, forms, items, icons, commands));
    }
    if spell.ranged_icon_substitution() {
        return ranged_weapon_icon(store, items, icons, commands);
    }
    None
}
