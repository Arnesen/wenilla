//! **The targeting cursor** — the client's one "this cast is waiting for a click" machine, both
//! halves: the **location** half (decision 0792, closing B132: "ground-targeted AOE all Invalid
//! target") and the **item** half (decision 0923: poisons, stones, oils, scopes, enchants).
//!
//! The reference's targeting mode IS a nonzero flag_word (`IsTargeting 0x6e48a0`); this module
//! holds benilla's mirror of that state and the systems around it, each transcribing a
//! byte-verified piece (wow-re `wave-cast.md` + `cursor-system.md` §5 + the world-click path in
//! `world-click-targeting.md`, plus 0923's own read of the two pickup seams):
//!
//! - **Cursor** ([`drive_targeting_cursor`]): while targeting, the world classifier is
//!   pre-empted (the ref's dispatcher step 2 runs before any object resolve) — **Cast** when the
//!   hovered ground point passes the range gate, **UnableCast** otherwise (`0x4820f0`'s split,
//!   computed by `CheckGroundPointInRange 0x6e6810` over `GetMinMaxRange`). The item half takes
//!   plain Cast: that range gate is a *location* predicate.
//! - **Location commit** ([`commit_ground_cast_on_click`]): the terrain leg's action-1 arm tries
//!   the ground commit first, gated only on the word (`0x492580` → `BindLocation 0x6e60f0`).
//! - **Item commit** ([`commit_item_cast_on_pick`]): the bag click (`PickupContainerItem
//!   0x4f9b30` @ `4f9c54`) and the paper-doll click (`0x4c7300` @ `4c76df`) each carry the same
//!   three-instruction rung — IsTargeting, `TargetingWantsItem 0x6e6330`, then
//!   `0x495d60(itemGuid)` and return — and `0x495d60` is [`item_target_refusal`] plus the bind.
//! - Both end in **one commit tail** (`CastLadder::commit_targeted`): the packet (`SendCast
//!   0x6e54f0`'s same block, two opcodes), the pending arm, the GCD, and the word cleared.
//! - **The ESC chain** ([`feed_targeting_to_vm`] / [`drain_stop_targeting`]): the real
//!   `UIParent.lua:1490` rung (`elseif ( SpellStopTargeting() ) then`) runs in our live VM; the
//!   feed pushes the state its `SpellIsTargeting`/`SpellStopTargeting` bindings read (and the
//!   item half's arm, which gates the VM's click reroute), the drain commits the cancel.
//!   AbortCast in targeting mode clears the word and sends **nothing**.
//!
//! Entry and the two press-cancel shapes live in the cast path itself: the resolver yields
//! [`super::cast_target::CastWireTarget::GroundTargeting`] (arm 16 / the bare DEST word) or
//! `ItemTargeting` (the bare ITEM word), the one cast-send path enters the mode here, a NEW
//! spell's press aborts-and-proceeds (`TryCast 6e4d62`), and the action bar's re-press of the
//! SAME spell toggles the mode off (`UseAction 0x4e5ee0`'s
//! `GetTargetingSpellId`+`StopTargeting` — [`super::drain`]).
//!
//! The click path is byte-pinned by wow-re's `world-click-targeting.md` (the 0792 dispatch's
//! answers): the terrain-leg commit `0x492580` has **no range gate and no error path** — it
//! binds and sends regardless, and the server judges range (`CheckGroundPointInRange 0x6e6810`
//! has exactly ONE caller binary-wide, the hover classifier `0x4820f0`: its verdict colours the
//! cursor and nothing else). While targeting, the pick flags come from the pending spell's mask
//! alone — for a dest-only word a unit is not pickable, so a click over one commits on the
//! ground behind it ([`crate::target::click::select_on_click`]'s gate transcribes the
//! unreachable select). Right-click cancels on the DOWN edge
//! ([`cancel_targeting_on_right_press`]); movement never cancels (`0x515090`'s explicit
//! IsTargeting-skip). The ground reticle draws in [`crate::target`]'s `reticle` module
//! (decision 0797) off [`ground_cast_radius`] + the cursor's range verdict.

use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_formats::SpellRange;

use crate::interact::{WorldClick, WorldRightPress};
use crate::net::SelfPlayer;
use crate::target::{CursorKind, PickOcclusion, WorldCursor};

use super::Spells;

/// Which click the standing flag_word is waiting for — the reference's two *wants* predicates,
/// each a one-instruction mask test on the same word `0xcecac0`, each consulted by exactly one
/// click seam:
///
/// - `TargetingWantsLocation 0x6e6320` (`word & 0x60`) → the terrain click's commit.
/// - `TargetingWantsItem 0x6e6330` (`word & 0x4010`) → the **bag** click's bind
///   (`PickupContainerItem 0x4f9b30` @ `4f9c5d`) and the **paper-doll** click's
///   (`0x4c7300` @ `4c76e8`) — the identical three-instruction rung in both.
///
/// The word itself is one state, which is why this is a mode of one resource and not two
/// resources: every cancel (ESC, right-press, a new cast's abort-and-proceed) clears the one word,
/// and the reference has exactly one `IsTargeting`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TargetingWants {
    /// Decision 0792 — the location half.
    Location,
    /// Decision 0923 — the item half.
    Item,
}

/// The targeting-cursor mode — benilla's `flag_word != 0` mirror: `Some` while a cast awaits the
/// click that binds its target. Entered by the one cast-send path ([`super::cast_send`]'s
/// `GroundTargeting`/`ItemTargeting` arms), cleared by whichever commit fires, the two press
/// cancels, and the ESC drain.
#[derive(Resource, Default)]
pub(crate) struct SpellTargeting(Option<Targeting>);

struct Targeting {
    spell_id: u32,
    /// What the click will commit. The ref keeps the whole pending-cast block across the cursor —
    /// the cast **item's** guid at `0xceac48` included — so `0x6e54f0`'s discriminator still picks
    /// `CMSG_USE_ITEM` when the click lands: a thrown grenade for the location half (decision
    /// 0914), a poison bottle for the item half (decision 0923).
    commit: super::cast_send::CastCommit,
    wants: TargetingWants,
}

impl SpellTargeting {
    /// `IsTargeting 0x6e48a0` — the canonical predicate.
    pub(crate) fn active(&self) -> bool {
        self.0.is_some()
    }

    /// `GetTargetingSpellId 0x6e48e0` — the spell awaiting its click, for the action bar's
    /// press-again toggle.
    pub(crate) fn spell(&self) -> Option<u32> {
        self.0.as_ref().map(|t| t.spell_id)
    }

    /// Which click seam owns the standing word — `None` when nothing is targeting.
    pub(crate) fn wants(&self) -> Option<TargetingWants> {
        self.0.as_ref().map(|t| t.wants)
    }

    pub(crate) fn enter(
        &mut self,
        spell_id: u32,
        commit: super::cast_send::CastCommit,
        wants: TargetingWants,
    ) {
        self.0 = Some(Targeting {
            spell_id,
            commit,
            wants,
        });
    }

    /// The pending cast's `(spell, commit)` when the word wants `wants` — the shape both commit
    /// systems open with, so neither can fire on the other half's word.
    fn pending_for(&self, wants: TargetingWants) -> Option<(u32, super::cast_send::CastCommit)> {
        self.0
            .as_ref()
            .filter(|t| t.wants == wants)
            .map(|t| (t.spell_id, t.commit))
    }

    pub(crate) fn clear(&mut self) {
        self.0 = None;
    }
}

/// `CheckGroundPointInRange 0x6e6810` — min²/max² from the spell's `SpellRange` row against the
/// squared caster↔point distance. Its ONE caller binary-wide is the hover-cursor classifier
/// (`0x4820f0` — wow-re `world-click-targeting.md` Q1's caller census): the verdict colours
/// Cast/UnableCast and nothing else. The click never consults it, so neither does ours. No row
/// (a failed DBC, an unknown spell) is permissive — the server validates every send anyway.
fn ground_point_in_range(row: Option<&SpellRange>, self_pos: Vec3, point: Vec3) -> bool {
    let Some(row) = row else {
        return true;
    };
    let dist_sq = self_pos.distance_squared(point);
    if row.min > 0.0 && dist_sq < row.min * row.min {
        return false;
    }
    dist_sq <= row.max * row.max
}

/// The targeting spell's `SpellRange` row, through the catalogs.
fn range_row(spells: Option<&Spells>, spell_id: u32) -> Option<&SpellRange> {
    let spells = spells?;
    spells.ranges.get(spells.catalog.get(spell_id)?.range_index)
}

/// `GetCurrentCastRadius 0x6e6350` (wow-re `ground-target-reticle.md` B2) — the reticle's
/// radius: per-effect `radius + casterLevel × perLevel` over **EffectRadiusIndex[0] and [1]
/// only** (slot 2 is never read by the client), the max with candidate 1 winning ties/NaN,
/// clamped to 20.0 (`0x4820f0`'s `[0x804478]` literal — `min`, NaN → 20). `0.0` = no radius
/// rows; the reticle then draws at its literal default size. Class-6 spell modifiers are
/// unmodelled (the 0792 residual, same as the range gate).
pub(crate) fn ground_cast_radius(spells: Option<&Spells>, spell_id: u32, level: u32) -> f32 {
    let Some(spells) = spells else { return 0.0 };
    let Some(d) = spells.catalog.get(spell_id) else {
        return 0.0;
    };
    let candidate = |slot: usize| -> f32 {
        let idx = d.effect_radius_index[slot];
        if idx == 0 {
            return 0.0;
        }
        spells
            .radii
            .get(idx)
            .map_or(0.0, |r| r.radius + level as f32 * r.per_level)
    };
    let (c0, c1) = (candidate(0), candidate(1));
    // Strict > for candidate 0; a tie or a NaN c0 falls to candidate 1 — the byte order.
    let r = if c0 > c1 { c0 } else { c1 };
    r.min(20.0)
}

/// While targeting, the world cursor is the classifier's pre-empt (`0x4820f0`, cursor-system
/// §5): **Cast** over an in-range ground point, **UnableCast** out of range / too close / no
/// ground hit at all (sky, mouselook). Runs right after [`crate::target`]'s classifier in the
/// target chain and overwrites its verdict — the ref runs this branch before the object
/// classifier ever executes, and the visible result is identical. Because it writes the *base*
/// [`WorldCursor`], it also pre-empts every UI overlay downstream ([`crate::cursor`]'s
/// repair/sell latches only arm while the base is Point) — which is the same total pre-emption
/// the reference's step 2 has.
///
/// The **item** half takes plain `Cast`, never the grayed twin: the range verdict below comes
/// from `CheckGroundPointInRange 0x6e6810`, which is a *location* predicate (its one caller
/// binary-wide is this classifier, over the ground point), and an item-targeting word has no
/// ground point to judge. Whether the reference grays the item cursor per hovered slot is
/// unpinned — the honest read is that its validity gate runs at BIND time (`0x495d60`, whose
/// mismatch is error `0x0a`), not at hover time. Named INTERIM, decision 0923.
pub(crate) fn drive_targeting_cursor(
    targeting: Res<SpellTargeting>,
    occlusion: Res<PickOcclusion>,
    spells: Option<Res<Spells>>,
    self_tf: Query<&Transform, With<SelfPlayer>>,
    mut cursor: ResMut<WorldCursor>,
) {
    let Some(spell_id) = targeting.spell() else {
        return;
    };
    let unable = match targeting.wants() {
        Some(TargetingWants::Item) | None => false,
        Some(TargetingWants::Location) => !match (occlusion.point, self_tf.single()) {
            (Some(point), Ok(tf)) => ground_point_in_range(
                range_row(spells.as_deref(), spell_id),
                tf.translation,
                point,
            ),
            _ => false,
        },
    };
    *cursor = WorldCursor {
        kind: CursorKind::Cast,
        unable,
    };
}

/// The world click's ground commit — the terrain leg's action-1 arm (`0x492580`, tried before
/// anything else the click could mean; [`crate::target::click::select_on_click`] holds its gate
/// while this mode is active, so the click neither selects nor deselects). Binds the frame's
/// pick-occlusion point and sends **unconditionally** — the leg's complete callee set has no
/// range check and no error path (wow-re `world-click-targeting.md` Q1; C2 REFUTED: the click
/// never gates on range, the server judges it, and its refusing `SMSG_CAST_RESULT` is the red
/// line) — `CMSG_CAST_SPELL` mask `0x40` + the point (WoW coords), arming the pending cast +
/// the GCD (the `SendCast 0x6e54f0` tail's two live pieces for a ground cast); the mode ends
/// with the send. No world hit (sky) → the nothing leg: no commit, mode kept.
///
/// Runs AFTER `select_on_click` in the target chain: the selection gate reads the mode's state,
/// so the commit that clears it must come later in the same frame.
pub(crate) fn commit_ground_cast_on_click(
    mut clicks: MessageReader<WorldClick>,
    occlusion: Res<PickOcclusion>,
    mut ladder: super::CastLadder,
) {
    if !ladder.ground.active() {
        // Keep the reader current so a click buffered while idle can never replay as a commit
        // the frame the mode turns on.
        clicks.clear();
        return;
    }
    if clicks.read().last().is_none() {
        return;
    }
    // `TargetingWantsLocation 0x6e6320` — an item-targeting word has no location leg, so the
    // terrain click's `BindLocation` binds nothing and the mode simply stays up.
    let Some((spell_id, commit)) = ladder.ground.pending_for(TargetingWants::Location) else {
        return;
    };
    let Some(point) = occlusion.point else {
        // The ray hit nothing (sky) — the ref's nothing-leg has no ground commit; the mode
        // stays, exactly like the UnableCast cursor said it would.
        return;
    };
    let dest = bevy_to_wow(point);
    debug!(
        "ui_action: ground cast {spell_id} committed at wow ({:.2}, {:.2}, {:.2})",
        dest[0], dest[1], dest[2]
    );
    // The shared commit tail — same block, two opcodes (`SendCast 0x6e54f0`'s one discriminator
    // survives the cursor, decision 0914: a thrown grenade commits as `CMSG_USE_ITEM` with the
    // DEST block), then the pending arm, the GCD, and the word cleared.
    ladder.commit_targeted(spell_id, commit, super::cast_send::TargetedBind::Dest(dest));
}

/// `0x495d60`'s equipped-item gate — the whole local validity law for an item target, run at
/// BIND time (the click), not at hover time. For each of the spell's three effects that is
/// `ENCHANT_ITEM` (53) / `ENCHANT_ITEM_TEMPORARY` (54) the reference tests, in this order:
///
/// - `EquippedItemSubClassMask [+0xec] != 0` ⇒ the item's class must equal `EquippedItemClass
///   [+0xe8]` **and** `(1 << subclass)` must be in the mask (`495e10`–`495e28`);
/// - `EquippedItemInventoryTypeMask [+0xf0] != 0` ⇒ `(1 << InventoryType)` must be in it
///   (`495e4d`–`495e70`).
///
/// Either miss lands on `0x496068`: `0x6e1a00(spell, 0x0a)` — the client's own **"Invalid
/// target"** red line, no packet, and the targeting word is *not* cleared (the ref returns before
/// `BindTarget`), so the cursor stays up for another try. That last detail is the one that makes
/// this feel right: a mis-click on the wrong weapon doesn't eat your poison press.
///
/// `Some(reason)` = refuse with that code; `None` = bind. A spell with no enchant effect (the
/// bare-`Targets 0x10` rows that are Disenchant and kin) has no leg to fail — the reference walks
/// its loop and falls straight through to `496056: call 0x6e5b40`, and so do we.
///
/// One narrowing, measured rather than assumed: the reference tests **all three** effect slots,
/// [`benilla_formats::SpellDisplay`] carries only slot 0, and across the whole 363-row item-target
/// family not one row hides its enchant effect in slot 1 or 2 — pinned by the formats-side family
/// test, which fails if that ever stops being true.
pub(crate) fn item_target_refusal(
    def: &benilla_formats::SpellDisplay,
    item_class: u32,
    item_subclass: u32,
    item_inventory_type: u32,
) -> Option<u8> {
    let enchants = def.effect_1 == benilla_formats::SPELL_EFFECT_ENCHANT_ITEM
        || def.effect_1 == benilla_formats::SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY;
    if !enchants {
        return None;
    }
    if def.equipped_item_subclass_mask != 0 {
        let class_ok = i64::from(item_class) == i64::from(def.equipped_item_class);
        let sub_ok =
            item_subclass < 32 && def.equipped_item_subclass_mask & (1u32 << item_subclass) != 0;
        if !(class_ok && sub_ok) {
            return Some(super::cast_target::ERR_INVALID_TARGET);
        }
    }
    if def.equipped_item_inventory_type_mask != 0
        && !(item_inventory_type < 32
            && def.equipped_item_inventory_type_mask & (1u32 << item_inventory_type) != 0)
    {
        return Some(super::cast_target::ERR_INVALID_TARGET);
    }
    None
}

/// The item half's commit — the bag and paper-doll click seams (`PickupContainerItem 0x4f9b30`
/// @ `4f9c54`–`4f9c6d` and its byte-identical doll twin `0x4c7300` @ `4c76df`–`4c76fb`: *if
/// IsTargeting and TargetingWantsItem, then `0x495d60(itemGuidLo, itemGuidHi)` and return —
/// nothing is picked up*). The VM half of that reroute lives in `benilla_ui`'s cursor seam; this
/// drain is `0x495d60` itself: resolve the clicked slot's live item, run [`item_target_refusal`],
/// and on a pass do what `496056` does — hand the item to the ONE binder, which fills the word's
/// item bit and lets `SendCast 0x6e54f0` commit. Same block, two opcodes: `CMSG_CAST_SPELL` for
/// an enchant off the Craft window, `CMSG_USE_ITEM` for a poison bottle's own ON_USE.
///
/// The post-send tail is the ground commit's (decision 0792): arm the pending cast + the GCD, and
/// clear the word. A click on an EMPTY slot binds nothing and keeps the mode — the ref's
/// `0x495d60` returns at its own null-item guard (`495da1`).
pub(crate) fn commit_item_cast_on_pick(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    self_q: Query<&crate::net::ObjectStore, With<SelfPlayer>>,
    mut ladder: super::CastLadder,
) {
    let Some(mut script) = script else {
        return;
    };
    for (bag, slot) in script.take_item_picks() {
        let Some((spell_id, commit)) = ladder.ground.pending_for(TargetingWants::Item) else {
            continue; // a click raced a cancel — the word is gone, so there is nothing to bind
        };
        let slot0 = u8::try_from(slot.saturating_sub(1)).unwrap_or(0);
        let Some(item_guid) = self_q
            .iter()
            .next()
            .and_then(|store| crate::ui_items::slot_guid(&store.0, bag, slot0, &ladder.items))
        else {
            debug!("ui_action: item pick on an empty slot (bag {bag} slot {slot}) — mode kept");
            continue;
        };
        // The gate needs the clicked item's template; an unresolved one (never seen in practice —
        // the bag needed it for the icon) binds ungated and lets the server judge, the same
        // permissive shape the rest of the click law uses.
        let entry = ladder
            .items
            .object(item_guid)
            .and_then(|o| o.object_entry());
        let clicked = match entry {
            Some(entry) => ladder
                .items
                .template(entry, item_guid, &ladder.commands)
                .map(|t| (t.class, t.subclass, t.inventory_type)),
            None => None,
        };
        let def = ladder
            .spells
            .as_deref()
            .and_then(|s| s.catalog.get(spell_id));
        if let (Some(d), Some((class, subclass, inv))) = (def, clicked) {
            if let Some(reason) = item_target_refusal(d, class, subclass, inv) {
                debug!(
                    "ui_action: cast {spell_id} refused at the item bind ({reason:#x}) — \
                     the cursor stays up"
                );
                ladder.cast_errors.0.push((spell_id, reason));
                continue;
            }
        }
        debug!("ui_action: item pick — cast {spell_id} at item {item_guid:#x}");
        ladder.commit_targeted(
            spell_id,
            commit,
            super::cast_send::TargetedBind::Item(item_guid),
        );
    }
}

/// Right-click cancels targeting — on the **DOWN edge**, the reference's WorldFrame
/// `OnMouseDown 0x483c40` → `0x492c20`: right button ∧ `IsTargeting` → `StopTargeting
/// 0x6e4900`, no packet — and the handler returns 0, so the press keeps doing everything else
/// it did (the turn-drag, the release's context click; we consume nothing either). Byte-pinned
/// by wow-re `world-click-targeting.md` Q3, whose caller census is complete: this and the
/// ESC/UseAction/TryCast paths are the ONLY input-band cancels — no keyboard caller exists.
///
/// Two qualifications, transcribed: a held cursor payload pre-empts the cancel (`0x492b50`
/// clears the payload and returns before the WorldFrame virtuals dispatch — our payload keeps
/// its own clean-click clear in [`crate::target::click::world_right_click_payload`]); and a
/// press over a UI frame never reaches the WorldFrame — [`WorldRightPress`]'s world gate
/// transcribes the certain half of wow-re's one DEFERRED (whether a UI-frame right-click also
/// cancels is unpinned there). The `0x51`-effect placement-rotate skip (`[0xceca90]`) is
/// unmodelled along with the flag itself (named residual, 0792).
pub(crate) fn cancel_targeting_on_right_press(
    mut presses: MessageReader<WorldRightPress>,
    payload_held: Res<crate::ui_script::CursorPayloadHeld>,
    mut targeting: ResMut<SpellTargeting>,
) {
    if !targeting.active() {
        // Reader hygiene, like the commit's: a press buffered while idle never replays as a
        // cancel the frame the mode turns on.
        presses.clear();
        return;
    }
    if presses.read().last().is_none() || payload_held.0 {
        return;
    }
    debug!("ui_action: targeting cancelled (right-click)");
    targeting.clear();
}

/// Push the targeting state into the live VM each frame, **before** the input pass — so a word
/// armed last frame is already standing when this frame's clicks run. Two consumers, one push:
/// `SpellIsTargeting()` / `SpellStopTargeting()`'s ESC chain reads the word itself, and the
/// engine's bag / doll pickup reroute reads the item half (`TargetingWantsItem`'s mirror).
pub(crate) fn feed_targeting_to_vm(
    targeting: Res<SpellTargeting>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
) {
    if let Some(mut script) = script {
        script.set_spell_targeting(targeting.active());
        script.set_item_pick_armed(targeting.wants() == Some(TargetingWants::Item));
    }
}

/// Drain the ESC chain's `SpellStopTargeting()` trigger (**after** the input pass) and clear
/// the mode — the ref's `StopTargeting 0x6e4900` → AbortCast-in-targeting: word cleared, no
/// packet.
pub(crate) fn drain_stop_targeting(
    mut targeting: ResMut<SpellTargeting>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
) {
    let Some(mut script) = script else {
        return;
    };
    if script.take_stop_targeting() {
        debug!("ui_action: targeting cancelled (ESC chain)");
        targeting.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x495d60`'s equipped-item gate, on the shapes the real rows have (the columns themselves
    /// are pinned against the shipped `Spell.dbc` in `benilla_formats`). Three legs, in the
    /// reference's order, and one refusal code for all of them — `0x0a` "Invalid target", the
    /// same red line a wrong unit target draws.
    #[test]
    fn the_item_bind_gate_mirrors_0x495d60() {
        use benilla_formats::SpellDisplay;
        // Item classes/subclasses/inventory types, 1.12 values.
        const CLASS_WEAPON: u32 = 2;
        const CLASS_ARMOR: u32 = 4;
        const SUB_DAGGER: u32 = 15;
        const SUB_SHIELD: u32 = 6;
        const INVTYPE_WRIST: u32 = 9;
        const INVTYPE_CHEST: u32 = 5;
        const ENCHANT: u32 = benilla_formats::SPELL_EFFECT_ENCHANT_ITEM;

        // Enchant Bracer - Minor Health (7418): armor, any subclass, WRIST only.
        let bracer = SpellDisplay {
            effect_1: ENCHANT,
            equipped_item_class: CLASS_ARMOR as i32,
            equipped_item_subclass_mask: 0x1f,
            equipped_item_inventory_type_mask: 1 << INVTYPE_WRIST,
            ..Default::default()
        };
        assert_eq!(
            item_target_refusal(&bracer, CLASS_ARMOR, 1, INVTYPE_WRIST),
            None,
            "a bracer takes the bracer enchant"
        );
        assert_eq!(
            item_target_refusal(&bracer, CLASS_ARMOR, 1, INVTYPE_CHEST),
            Some(super::super::cast_target::ERR_INVALID_TARGET),
            "the inventory-type leg (495e4d) refuses a chestpiece"
        );
        assert_eq!(
            item_target_refusal(&bracer, CLASS_WEAPON, 1, INVTYPE_WRIST),
            Some(super::super::cast_target::ERR_INVALID_TARGET),
            "the class leg (495e10) refuses a weapon"
        );

        // Instant Poison (8679): weapon class, a subclass mask, NO inventory-type requirement.
        let poison = SpellDisplay {
            effect_1: benilla_formats::SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY,
            equipped_item_class: CLASS_WEAPON as i32,
            equipped_item_subclass_mask: 0x2a5f3,
            equipped_item_inventory_type_mask: 0,
            ..Default::default()
        };
        // 8679's real mask carries dagger (15), so a rogue's own weapon passes the subclass leg.
        assert_eq!(
            item_target_refusal(&poison, CLASS_WEAPON, SUB_DAGGER, 13),
            None
        );
        assert_eq!(
            item_target_refusal(&poison, CLASS_WEAPON, 1, 13),
            None,
            "and two-handed axe (1) is in it too — poisons are broad, the class leg is the fence"
        );
        assert_eq!(
            item_target_refusal(&poison, CLASS_ARMOR, SUB_SHIELD, 14),
            Some(super::super::cast_target::ERR_INVALID_TARGET),
            "a shield is armor — the class leg alone stops it"
        );

        // Disenchant (13262): an item-targeted spell with NO enchant effect. The reference walks
        // its loop, finds no 53/54 arm, and falls straight through to the bind — anything goes,
        // and the server judges.
        let disenchant = SpellDisplay {
            effect_1: 99,
            equipped_item_class: -1,
            ..Default::default()
        };
        assert_eq!(
            item_target_refusal(&disenchant, CLASS_ARMOR, 1, INVTYPE_CHEST),
            None
        );
        assert_eq!(
            item_target_refusal(&disenchant, CLASS_WEAPON, SUB_DAGGER, 13),
            None
        );

        // A subclass past the mask's 32 bits can never be in it — shifted, that would be UB-ish
        // nonsense, so the gate refuses instead of wrapping.
        assert_eq!(
            item_target_refusal(&poison, CLASS_WEAPON, 40, 13),
            Some(super::super::cast_target::ERR_INVALID_TARGET)
        );
    }

    /// The two halves of the ONE word never answer each other's click (the reference asks
    /// `TargetingWantsLocation`/`TargetingWantsItem` at each seam, decision 0923). Without this,
    /// a terrain click while a poison is armed would ship a DEST block for an item spell.
    #[test]
    fn each_click_seam_only_sees_its_own_half() {
        let commit = super::super::cast_send::CastCommit::Spell;
        let mut t = SpellTargeting::default();
        assert_eq!(
            t.pending_for(TargetingWants::Location),
            None,
            "idle binds nothing"
        );

        t.enter(2120, commit, TargetingWants::Location);
        assert!(t.active());
        assert_eq!(
            t.pending_for(TargetingWants::Location),
            Some((2120, commit))
        );
        assert_eq!(
            t.pending_for(TargetingWants::Item),
            None,
            "a bag click cannot commit a Blizzard"
        );

        t.enter(8679, commit, TargetingWants::Item);
        assert_eq!(t.pending_for(TargetingWants::Item), Some((8679, commit)));
        assert_eq!(
            t.pending_for(TargetingWants::Location),
            None,
            "a terrain click cannot commit a poison"
        );
        // The spell id is the word's, either half — what the action bar's re-press toggle reads.
        assert_eq!(t.spell(), Some(8679));
        t.clear();
        assert!(!t.active());
    }

    /// The `0x6e6810` mirror: min²/max² against the squared caster↔point distance — the
    /// CURSOR's verdict and nothing else (its one caller binary-wide is the hover classifier;
    /// the click never asks). Permissive with no row (Blizzard's row 4 is 0–30 yd; a synthetic
    /// min exercises the too-close arm the real row can't).
    #[test]
    fn ground_point_in_range_mirrors_check_ground_point_in_range() {
        let row = |min: f32, max: f32| SpellRange { min, max, flags: 0 };
        let origin = Vec3::ZERO;
        let at = |d: f32| Vec3::new(d, 0.0, 0.0);
        let blizzard = row(0.0, 30.0);
        assert!(ground_point_in_range(Some(&blizzard), origin, at(29.9)));
        assert!(!ground_point_in_range(Some(&blizzard), origin, at(30.1)));
        let banded = row(8.0, 35.0);
        assert!(!ground_point_in_range(Some(&banded), origin, at(5.0)));
        assert!(ground_point_in_range(Some(&banded), origin, at(20.0)));
        // No row → permissive (the server still validates).
        assert!(ground_point_in_range(None, origin, at(500.0)));
    }

    /// `GetCurrentCastRadius 0x6e6350` + the `0x4820f0` clamp: slots 0/1 only (slot 2 is never
    /// read), max with candidate-1 winning ties, per-level scaling, min(r, 20). Fixture rows
    /// mirror the real table (row 14 = 8.0 Blizzard, row 8 = 5.0 Flamestrike).
    #[test]
    fn ground_cast_radius_mirrors_get_current_cast_radius() {
        use benilla_formats::{SpellDisplay, SpellRadius};
        use std::collections::HashMap;
        let mut spells = super::super::Spells::empty_for_tests();
        let display = |idx: [u32; 3]| SpellDisplay {
            effect_radius_index: idx,
            ..SpellDisplay::default()
        };
        spells.catalog = benilla_formats::SpellCatalog::from_displays(HashMap::from([
            (10, display([14, 0, 0])),
            (2120, display([8, 8, 0])),
            (777, display([0, 0, 13])), // slot 2 only — the client never reads it
            (778, display([90, 8, 0])), // per-level row in slot 0
            (779, display([10, 0, 0])), // row 10 = 30.0 — the 20.0 clamp
        ]));
        spells.radii = benilla_formats::SpellRadiusCatalog::from_rows(HashMap::from([
            (
                14,
                SpellRadius {
                    radius: 8.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                8,
                SpellRadius {
                    radius: 5.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                13,
                SpellRadius {
                    radius: 10.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                10,
                SpellRadius {
                    radius: 30.0,
                    per_level: 0.0,
                    max: 0.0,
                },
            ),
            (
                90,
                SpellRadius {
                    radius: 2.0,
                    per_level: 0.1,
                    max: 0.0,
                },
            ),
        ]));
        let s = Some(&spells);
        assert_eq!(ground_cast_radius(s, 10, 60), 8.0);
        assert_eq!(ground_cast_radius(s, 2120, 60), 5.0);
        // Slot 2 is invisible to the reticle — no rows in 0/1 reads 0 (→ the default size).
        assert_eq!(ground_cast_radius(s, 777, 60), 0.0);
        // Per-level: 2.0 + 60 × 0.1 = 8.0 beats slot 1's 5.0.
        assert_eq!(ground_cast_radius(s, 778, 60), 8.0);
        // The 20.0 clamp (`[0x804478]`).
        assert_eq!(ground_cast_radius(s, 779, 60), 20.0);
        // Unknown spell / no data at all → 0 (default size).
        assert_eq!(ground_cast_radius(s, 9999, 60), 0.0);
        assert_eq!(ground_cast_radius(None, 10, 60), 0.0);
    }
}
