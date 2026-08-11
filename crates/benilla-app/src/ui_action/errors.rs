//! The red error line's queues and resolvers — every route into `UIErrorsFrame`'s top line
//! except the cast-fail two-layer display (its own module, [`super::cast_fail`]):
//!
//! - [`CastErrors`] — the wire `(spell_id, reason)` pairs (`SMSG_CAST_RESULT` + local cast
//!   refusals), resolved by [`super::cast_fail`] at the drain.
//! - [`MountErrors`] — the `SMSG_MOUNTRESULT`/`SMSG_DISMOUNTRESULT` code pairs, resolved by
//!   [`mount_result_key`] (decision 0441 P2).
//! - [`UiErrorKeys`] — client-LOCAL refusals straight by GlobalStrings key, the
//!   `CGGameUI::DisplayError` route for errors with no wire code and no spell record:
//!   `ERR_ATTACK_MOUNTED` (decision 0481) and the GameObject lock-refusal toasts
//!   ("Requires Herbalism", decision 0545) — the latter carry [`UiError`]'s `%s`/`%d`
//!   argText fills, resolved by [`ui_error_text`].
//!
//! All three drain in `super::feed_actions`, firing `UI_ERROR_MESSAGE` per resolved line;
//! every string comes from the VM's own loaded `GlobalStrings.lua`, never hardcoded, so an
//! absent key shows nothing (the reference's data-suppression face) and localization rides
//! for free.

use crate::ui_items::{count_of, InventoryScope};
use bevy::prelude::*;

use crate::net::ObjectStore;

/// Cast failures queued for the UI error line, as `(spell_id, reason)` — the wire pair from
/// `SMSG_CAST_RESULT` and the local refusals alike. The spell id rides along because the
/// display layer keys several messages on the failing spell's record ([`super::cast_fail`]:
/// NO_POWER's power family, the 0x28/0x3c cooldown families).
#[derive(Resource, Default)]
pub(crate) struct CastErrors(pub Vec<(u32, u8)>);

/// (Dis)mount refusals queued for the UI error line, as the wire pair `(mount, code)` from
/// `SMSG_MOUNTRESULT`/`SMSG_DISMOUNTRESULT` (decision 0441 P2) — resolved to text through the
/// VM's own GlobalStrings by key ([`mount_result_key`]), the [`CastErrors`] shape exactly.
#[derive(Resource, Default)]
pub(crate) struct MountErrors(pub Vec<(bool, u32)>);

/// One `DisplayError` message: a GlobalStrings key plus the argText fills. The 1.12 error
/// formats use at most one `%s` and one `%d` ("Requires %s", "Requires %s %d" — wow-re
/// cursor-system.md §8.8's lock-refusal toasts, decision 0545); a key whose string carries no
/// token ignores its fills. Not red-line-only: this is the payload of the reference's ONE
/// `CGGameUI::DisplayError` (`0x496720`) whatever surface the message's record names — the
/// quest refusals carry their chat lines in it too (decision 0669).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UiError {
    pub key: &'static str,
    pub fill_s: Option<String>,
    pub fill_d: Option<u32>,
    /// The message record's **type arm** (wow-re `fish-msg-handlers.md`, byte-proven — the
    /// registry entry's `+4` type, last written `mov edx,1` at `0x486235`): the reference's
    /// `DisplayError` pipeline is one, and the type selects the UIErrorsFrame event — type 1
    /// fires the **yellow** `UI_INFO_MESSAGE` (0xe1), type 2 the **red** `UI_ERROR_MESSAGE`
    /// (0xe0). `true` = the yellow info arm (the fishing verdicts are the first tenants);
    /// `false` = the red error arm, the default.
    pub info: bool,
}

impl UiError {
    /// A fill-less message — the plain-key tenants (`ERR_ATTACK_MOUNTED`, the flag-locked
    /// strategy defaults).
    pub(crate) fn key(key: &'static str) -> Self {
        Self {
            key,
            fill_s: None,
            fill_d: None,
            info: false,
        }
    }

    /// A fill-less **type-1** message — the yellow `UI_INFO_MESSAGE` arm (see [`UiError::info`]).
    pub(crate) fn info_key(key: &'static str) -> Self {
        Self {
            info: true,
            ..Self::key(key)
        }
    }
}

/// Client-LOCAL refusals queued for the UI error line straight by GlobalStrings key — the
/// `CGGameUI::DisplayError` route for errors that carry no wire code and no spell record
/// (`ERR_ATTACK_MOUNTED` was the first tenant; the GameObject lock-refusal toasts of
/// decision 0545 are the formatted ones). The [`MountErrors`] shape without the code table.
#[derive(Resource, Default)]
pub(crate) struct UiErrorKeys(pub Vec<UiError>);

/// Resolve one [`UiError`] to its displayed text — `GetText(key)` + the `%s`/`%d` argText
/// substitution ("Requires %s" + "Herbalism" → "Requires Herbalism", cursor-system.md §8.8).
/// `None` (absent or empty key) = show nothing: GlobalStrings data-suppression, faithfully
/// (the ref's own `[record+0x00]` null/empty guard at `0x4967bd`/`0x4967c5`).
pub(crate) fn ui_error_text(e: &UiError, get: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut text = get(e.key)?;
    if let Some(s) = &e.fill_s {
        text = text.replace("%s", s);
    }
    if let Some(d) = e.fill_d {
        text = text.replace("%d", &d.to_string());
    }
    (!text.is_empty()).then_some(text)
}

/// `UNIT_FIELD_FLAGS` bits the attack-start validator refuses on, paired with the message each
/// raises — read at `0x612eec`+ in the binary's own test order (wow-re
/// `object-layer/scratch/pet-command-validators.md` §2). All four are crowd control: the actor is
/// not refusing, it is unable.
///
/// Each has a second face in the reference — errorId `0xa9`
/// `ERR_ATTACK_PREVENTED_BY_MECHANIC_S`, which substitutes a resolved mechanic name — chosen by
/// five `0x6e9…` resolvers whose bodies wow-re records as **not carved**. We raise the plain form
/// only; the fill is a strictly better message for the same refusal, never a different one.
const ATTACK_FLAG_REFUSALS: [(u32, &str); 4] = [
    (0x0004_0000, "ERR_ATTACK_STUNNED"),
    (0x0002_0000, "ERR_ATTACK_PACIFIED"),
    (0x0080_0000, "ERR_ATTACK_FLEEING"),
    (0x0040_0000, "ERR_ATTACK_CONFUSED"),
];

/// Phase A of the shared attack-start validator `0x612df0` — **the actor's own eligibility**
/// (wow-re `object-layer/scratch/pet-command-validators.md` §2, carved 2026-08-05; it supersedes
/// the mounted-only fragment decision 0481 built from the one gate that was known then).
///
/// `0x612df0(ecx = actor, &outGuid)` is ONE function with three call sites, and **the actor is
/// whoever the caller passes in `ecx`** — the player for the melee attack-start router `0x6131aa`,
/// and **the pet** for the pet bar's ATTACK arm (`0x4bd40d` passes `edi`, the pet object). Every
/// gate below reads the actor's own descriptor, so one function answers both; that is why this
/// takes an actor rather than reaching for the self store the way the fragment did.
///
/// A veto is total: `EAX = 0`, one of the ten consecutive `ERR_ATTACK_*` registry ids `0xa0`–`0xa9`
/// through `CGGameUI::DisplayError`, and **no packet at all** — on the pet arm the refusal's
/// `0x4bd414 je 0x4bd4c6` lands on the function epilogue, not on the shared send.
///
/// Order is the binary's, and it is observable: a dead **and** mounted actor says "Can't attack
/// while dead."
///
/// `dead` is `0x605f30(actor)`, which for any non-player actor — a pet never carries the player
/// typemask bit — degenerates to exactly `health <= 0`. Its further leg for a *player* actor
/// (`[[obj+0xe68]+8]` bit 4, reached only when health is positive, so plainly the ghost state) is
/// byte-read but unnamed in wow-re's note, so it is left to the caller: pass `dead` yourself when
/// you know more than the health field does.
pub(crate) fn attack_actor_refusal(
    actor: Option<&ObjectStore>,
    self_guid: Option<u64>,
    errors: &mut UiErrorKeys,
) -> bool {
    // No descriptor is no refusal. The reference resolves the actor object first and skips the
    // whole chain when it cannot (`0x4bd403`: no pet ⇒ send unmodified) — an un-streamed unit is
    // not an ineligible one.
    let Some(fields) = actor.map(|s| &s.0) else {
        return false;
    };
    let key = if fields.unit_health().is_some_and(|h| h == 0) {
        "ERR_ATTACK_DEAD"
    } else if fields
        .unit_charmed_by()
        .is_some_and(|g| Some(g) != self_guid)
    {
        // Charmed by somebody who is not us. Charmed BY US is not a refusal — a mind-controlled
        // unit is one you are allowed to swing with.
        "ERR_ATTACK_CHARMED"
    } else if let Some((_, key)) = ATTACK_FLAG_REFUSALS
        .iter()
        .find(|(bit, _)| fields.unit_flags() & bit != 0)
    {
        key
    } else if fields.unit_mount_display_id() > 0 {
        "ERR_ATTACK_MOUNTED"
    } else {
        return false;
    };
    debug!("attack refused locally by the actor's own state — {key}");
    errors.0.push(UiError::key(key));
    true
}

/// The ref's pre-send totem/reagent possession check — `CheckReagentsAndTotems 0x6e4000`,
/// byte-verified (decision 0552; wow-re `cast-fail-strings.md` "Loose end 2"): TryCast runs it
/// for EVERY cast path (action bar, Lua, the GameObject-use opener) **before any packet is
/// built**. Totems first (2 slots, a bag **presence** test — the Mining Pick / Skinning Knife /
/// Thieves' Tools tools), then reagents (8 slots, a bag **count** test). The first failing slot
/// refuses the cast LOCALLY — reason `0x78`/`0x5c` into [`CastErrors`] (whose drain fills
/// "Requires <item>" / "Missing reagent: <item>") and **no send** — which is the only way
/// "Requires Mining Pick" can ever appear: vmangos answers a sent pickless cast with
/// `ITEM_GONE` ("Item is gone"), and its own source marks the totem reason "client-side only".
/// A missing self store skips the check, like the ref's `IsActivePlayer` gate (the client can
/// only see its own bags). Returns `true` when the cast must be refused.
pub(crate) fn reagent_totem_refusal(
    spell_id: u32,
    def: Option<&benilla_formats::SpellDisplay>,
    self_store: Option<&ObjectStore>,
    items: &crate::items::Items,
    errors: &mut CastErrors,
) -> bool {
    let (Some(d), Some(store)) = (def, self_store) else {
        return false;
    };
    // Totems before reagents — the ref's in-function loop order.
    let reason = if first_missing_totem(d, store, items).is_some() {
        0x78
    } else if first_short_reagent(d, store, items).is_some() {
        0x5c
    } else {
        return false;
    };
    debug!("cast {spell_id} refused locally — missing totem/reagent ({reason:#04x})");
    errors.0.push((spell_id, reason));
    true
}

/// The first totem (tool) slot whose item is absent from our bags — the `0x6e4000` totem loop's
/// failing slot, re-derived (a presence test: any owned count satisfies).
pub(super) fn first_missing_totem(
    d: &benilla_formats::SpellDisplay,
    store: &ObjectStore,
    items: &crate::items::Items,
) -> Option<u32> {
    d.totems
        .iter()
        .copied()
        .filter(|&t| t != 0)
        .find(|&t| count_of(&store.0, items, t, InventoryScope::CARRIED) == 0)
}

/// The first reagent slot whose owned count falls short — the `0x6e4000` reagent loop's failing
/// slot, re-derived.
pub(super) fn first_short_reagent(
    d: &benilla_formats::SpellDisplay,
    store: &ObjectStore,
    items: &crate::items::Items,
) -> Option<u32> {
    d.reagents
        .iter()
        .copied()
        .filter(|&(id, _)| id != 0)
        .find(|&(id, n)| count_of(&store.0, items, id, InventoryScope::CARRIED) < n)
        .map(|(id, _)| id)
}

/// The (dis)mount result code → its `ERR_MOUNT_*`/`ERR_DISMOUNT_*` GlobalStrings key. The code
/// tables are vmangos `UnitDefines.h` (`UnitMountResult`/`UnitDismountResult`); every key was
/// verified present in the shipped 1.12 `GlobalStrings.lua` (patch-2.MPQ, extracted 2026-07-17)
/// — including the deliberately-shipped `ERR_MOUNT_OTHER` = "UNKNOWN MOUNT ERROR" and the
/// INTERNAL-ERROR dismount strings. The success codes (10 mounting / 3 dismounting) are silent.
pub(super) fn mount_result_key(mount: bool, code: u32) -> Option<&'static str> {
    if mount {
        match code {
            0 => Some("ERR_MOUNT_INVALIDMOUNTEE"),
            1 => Some("ERR_MOUNT_TOOFARAWAY"),
            2 => Some("ERR_MOUNT_ALREADYMOUNTED"),
            3 => Some("ERR_MOUNT_NOTMOUNTABLE"),
            4 => Some("ERR_MOUNT_NOTYOURPET"),
            5 => Some("ERR_MOUNT_OTHER"),
            6 => Some("ERR_MOUNT_LOOTING"),
            7 => Some("ERR_MOUNT_RACECANTMOUNT"),
            8 => Some("ERR_MOUNT_SHAPESHIFTED"),
            9 => Some("ERR_MOUNT_FORCEDDISMOUNT"),
            _ => None, // 10 = OK; anything past the table stays the debug log's business
        }
    } else {
        match code {
            0 => Some("ERR_DISMOUNT_NOPET"),
            1 => Some("ERR_DISMOUNT_NOTMOUNTED"),
            2 => Some("ERR_DISMOUNT_NOTYOURPET"),
            _ => None, // 3 = OK
        }
    }
}
#[cfg(test)]
mod mount_error_tests {
    use super::mount_result_key;

    #[test]
    fn success_codes_are_silent_and_failures_map() {
        assert_eq!(mount_result_key(true, 10), None); // MOUNTRESULT_OK
        assert_eq!(mount_result_key(false, 3), None); // DISMOUNTRESULT_OK
        assert_eq!(mount_result_key(true, 2), Some("ERR_MOUNT_ALREADYMOUNTED"));
        assert_eq!(mount_result_key(true, 8), Some("ERR_MOUNT_SHAPESHIFTED"));
        assert_eq!(mount_result_key(false, 1), Some("ERR_DISMOUNT_NOTMOUNTED"));
        // Off-table codes stay the debug log's business — no red line.
        assert_eq!(mount_result_key(true, 11), None);
        assert_eq!(mount_result_key(false, 4), None);
    }

    /// The RUNTIME leg on the real data (the `cast_fail` pattern): every key this table can
    /// emit resolves to a non-empty string in the shipped 1.12 `GlobalStrings.lua` — the guard
    /// against a typo'd key silently swallowing the red line. Skips without client data.
    #[test]
    fn every_mount_key_resolves_in_the_real_global_strings() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let src = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .expect("GlobalStrings.lua in the chain");
        let s = benilla_ui::script::UiScript::new().expect("VM");
        s.run(&String::from_utf8_lossy(&src)).expect("runs clean");
        let g = |key: &str| s.lua().globals().get::<String>(key).ok();

        for (mount, codes) in [(true, 0..=9), (false, 0..=2)] {
            for code in codes {
                let key = mount_result_key(mount, code).expect("failure code maps");
                let text = g(key).unwrap_or_default();
                assert!(!text.is_empty(), "{key} missing from GlobalStrings");
            }
        }
        assert_eq!(
            g("ERR_MOUNT_ALREADYMOUNTED").unwrap(),
            "You're already mounted!"
        );
        assert_eq!(g("ERR_DISMOUNT_NOTMOUNTED").unwrap(), "You're not mounted!");
    }
}

#[cfg(test)]
mod ui_error_tests {
    use super::{ui_error_text, UiError};

    fn filled(key: &'static str, s: Option<&str>, d: Option<u32>) -> UiError {
        UiError {
            key,
            fill_s: s.map(String::from),
            fill_d: d,
            info: false,
        }
    }

    /// The DisplayError argText substitution against a fake getter: `%s` then `%d`, key-absent
    /// and key-empty both silent (the GlobalStrings data-suppression face).
    #[test]
    fn fills_substitute_and_absent_keys_are_silent() {
        let get = |key: &str| match key {
            "REQ_S" => Some("Requires %s".to_string()),
            "REQ_SI" => Some("Requires %s %d".to_string()),
            "PLAIN" => Some("Can't attack while mounted.".to_string()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        let t = |e: &UiError| ui_error_text(e, &get);
        assert_eq!(
            t(&filled("REQ_S", Some("Herbalism"), None)).as_deref(),
            Some("Requires Herbalism")
        );
        assert_eq!(
            t(&filled("REQ_SI", Some("Mining"), Some(100))).as_deref(),
            Some("Requires Mining 100")
        );
        assert_eq!(
            t(&UiError::key("PLAIN")).as_deref(),
            Some("Can't attack while mounted.")
        );
        assert_eq!(t(&UiError::key("EMPTY")), None);
        assert_eq!(t(&UiError::key("ABSENT")), None);
    }

    /// The RUNTIME leg on the real data (the `cast_fail`/mount pattern): every GlobalStrings
    /// key the lock-refusal toasts (decision 0545, wow-re cursor-system.md §8.8) and the totem
    /// fill can emit resolves in the shipped 1.12 `GlobalStrings.lua`, with the exact ref-quoted
    /// formats — the guard against a typo'd key silently swallowing the red line. Skips without
    /// client data.
    #[test]
    fn every_lock_refusal_key_resolves_in_the_real_global_strings() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let src = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .expect("GlobalStrings.lua in the chain");
        let s = benilla_ui::script::UiScript::new().expect("VM");
        s.run(&String::from_utf8_lossy(&src)).expect("runs clean");
        let g = |key: &str| s.lua().globals().get::<String>(key).ok();

        assert_eq!(g("ERR_USE_LOCKED_WITH_SPELL_S").unwrap(), "Requires %s");
        assert_eq!(
            g("ERR_USE_LOCKED_WITH_SPELL_KNOWN_SI").unwrap(),
            "Requires %s %d"
        );
        assert_eq!(g("ERR_USE_LOCKED_WITH_ITEM_S").unwrap(), "Requires %s");
        assert_eq!(g("ERR_USE_LOCKED").unwrap(), "Item is locked.");
        assert_eq!(g("ERR_DOOR_LOCKED").unwrap(), "The door is locked.");
        assert_eq!(
            g("ERR_BUTTON_LOCKED").unwrap(),
            "That has already been used."
        );
        assert_eq!(g("ERR_USE_CANT_OPEN").unwrap(), "You can't open that.");
        // The ENGINE's own by-key refusal (`benilla_ui`'s `place_action`, errorId `0x9e`). The
        // key is the load-bearing half: an unresolvable one makes `ui_error_text` answer None and
        // the refusal quietly goes back to being the divergence 0666 named.
        assert_eq!(
            g("ERR_PASSIVE_ABILITY").unwrap(),
            "You can't put a passive ability in the action bar."
        );
        // The wire-side totem fill's template (feed_actions' 0x78 arm): "Requires Mining Pick".
        assert_eq!(g("SPELL_FAILED_TOTEMS").unwrap(), "Requires %s");

        // End to end through the formatter — the two gathering lines the director will see.
        let herb = filled("ERR_USE_LOCKED_WITH_SPELL_S", Some("Herbalism"), None);
        assert_eq!(
            ui_error_text(&herb, &g).as_deref(),
            Some("Requires Herbalism")
        );
        let vein = filled(
            "ERR_USE_LOCKED_WITH_SPELL_KNOWN_SI",
            Some("Mining"),
            Some(155),
        );
        assert_eq!(
            ui_error_text(&vein, &g).as_deref(),
            Some("Requires Mining 155")
        );
    }
}

#[cfg(test)]
mod attack_actor_tests {
    use super::*;
    use benilla_protocol::ObjectFields;

    const HEALTH: u16 = 22;
    const FLAGS: u16 = 46;
    const CHARMEDBY: u16 = 10;
    const MOUNT: u16 = 133;

    fn actor(pairs: &[(u16, u32)]) -> ObjectStore {
        // A live, unowned, unmounted, unimpaired unit unless a case says otherwise.
        let mut all = vec![(HEALTH, 100)];
        all.extend_from_slice(pairs);
        ObjectStore(ObjectFields::from_pairs(&all))
    }

    fn refusal(store: Option<&ObjectStore>, self_guid: Option<u64>) -> Option<&'static str> {
        let mut errors = UiErrorKeys::default();
        let refused = attack_actor_refusal(store, self_guid, &mut errors);
        assert_eq!(
            refused,
            !errors.0.is_empty(),
            "a refusal and a message are the same event — the ref raises one per veto"
        );
        errors.0.first().map(|e| e.key)
    }

    /// Every gate of `0x612df0`'s Phase A, each on its own, against an otherwise-healthy actor.
    #[test]
    fn each_actor_gate_raises_its_own_error() {
        assert_eq!(
            refusal(Some(&actor(&[])), Some(7)),
            None,
            "a fit actor swings"
        );
        assert_eq!(
            refusal(Some(&actor(&[(HEALTH, 0)])), Some(7)),
            Some("ERR_ATTACK_DEAD")
        );
        assert_eq!(
            refusal(Some(&actor(&[(MOUNT, 1147)])), Some(7)),
            Some("ERR_ATTACK_MOUNTED")
        );
        for (bit, key) in ATTACK_FLAG_REFUSALS {
            assert_eq!(
                refusal(Some(&actor(&[(FLAGS, bit)])), Some(7)),
                Some(key),
                "unit flag {bit:#x}"
            );
        }
        // An unrelated flag bit is not a refusal — the mask is four specific bits, not "any flag".
        assert_eq!(refusal(Some(&actor(&[(FLAGS, 0x1000)])), Some(7)), None);
    }

    /// `0x612e33`'s charm test compares against the ACTIVE PLAYER's guid, so being charmed **by
    /// us** is not a refusal — that is the whole point of mind control, and reading the field as a
    /// plain "is charmed" boolean would make every controlled unit unable to swing.
    #[test]
    fn charmed_by_us_still_swings_and_charmed_away_does_not() {
        let mine = actor(&[(CHARMEDBY, 7), (CHARMEDBY + 1, 0)]);
        assert_eq!(refusal(Some(&mine), Some(7)), None);

        let theirs = actor(&[(CHARMEDBY, 9), (CHARMEDBY + 1, 0)]);
        assert_eq!(refusal(Some(&theirs), Some(7)), Some("ERR_ATTACK_CHARMED"));
        // Not knowing our own guid yet cannot make somebody else's charm look like ours.
        assert_eq!(refusal(Some(&theirs), None), Some("ERR_ATTACK_CHARMED"));
    }

    /// The order is the binary's, and it is observable: the FIRST gate to fail names the message,
    /// so a dead-and-mounted actor says "Can't attack while dead."
    #[test]
    fn the_first_failing_gate_names_the_message() {
        let both = actor(&[(HEALTH, 0), (MOUNT, 1147), (FLAGS, 0x0004_0000)]);
        assert_eq!(refusal(Some(&both), Some(7)), Some("ERR_ATTACK_DEAD"));
        // …and with the death removed, the flag block still precedes the mount check.
        let cc = actor(&[(MOUNT, 1147), (FLAGS, 0x0004_0000)]);
        assert_eq!(refusal(Some(&cc), Some(7)), Some("ERR_ATTACK_STUNNED"));
    }

    /// An un-streamed actor is not an ineligible one: the reference skips the whole chain when it
    /// cannot resolve the object (`0x4bd403` — no pet ⇒ send unmodified). A descriptor that simply
    /// has not sent a health field yet must not read as a corpse.
    #[test]
    fn an_unresolved_actor_is_never_a_refusal() {
        assert_eq!(refusal(None, Some(7)), None);
        assert_eq!(
            refusal(Some(&ObjectStore(ObjectFields::default())), Some(7)),
            None
        );
    }

    /// The RUNTIME leg (the mount/lock-refusal pattern): all ten of the consecutive `ERR_ATTACK_*`
    /// registry ids `0xa0`–`0xa9` resolve to non-empty strings in the shipped 1.12
    /// `GlobalStrings.lua`. This is the guard against a typo'd key turning a refusal into a silent
    /// one — a veto that shows nothing is indistinguishable from a click that did nothing.
    #[test]
    fn every_attack_error_key_resolves_in_the_real_global_strings() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let src = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .expect("GlobalStrings.lua in the chain");
        let s = benilla_ui::script::UiScript::new().expect("VM");
        s.run(&String::from_utf8_lossy(&src)).expect("runs clean");
        let g = |key: &str| s.lua().globals().get::<String>(key).ok();

        for key in [
            "ERR_NO_ATTACK_TARGET",               // 0xa0
            "ERR_INVALID_ATTACK_TARGET",          // 0xa1
            "ERR_ATTACK_STUNNED",                 // 0xa2
            "ERR_ATTACK_PACIFIED",                // 0xa3
            "ERR_ATTACK_MOUNTED",                 // 0xa4
            "ERR_ATTACK_FLEEING",                 // 0xa5
            "ERR_ATTACK_CONFUSED",                // 0xa6
            "ERR_ATTACK_CHARMED",                 // 0xa7
            "ERR_ATTACK_DEAD",                    // 0xa8
            "ERR_ATTACK_PREVENTED_BY_MECHANIC_S", // 0xa9
        ] {
            assert!(!g(key).unwrap_or_default().is_empty(), "{key} missing");
        }
        // The one wow-re quotes from the file, as a spot check that the ids line up with the keys.
        assert_eq!(g("ERR_ATTACK_DEAD").unwrap(), "Can't attack while dead.");
    }
}

#[cfg(test)]
mod totem_reagent_tests {
    use super::*;
    use benilla_formats::SpellDisplay;
    use benilla_protocol::ObjectFields;

    fn store() -> ObjectStore {
        // Empty bags: no pack fields streamed → every count reads 0, everything is "missing".
        ObjectStore(ObjectFields::default())
    }

    fn spell(totems: [u32; 2], reagents: [(u32, u32); 8]) -> SpellDisplay {
        SpellDisplay {
            totems,
            reagents,
            ..Default::default()
        }
    }

    /// The pre-send check's routing (`0x6e4000`, decision 0552) against empty bags: a totem
    /// spell refuses 0x78, a reagent spell 0x5c, totems win when both lack (the ref's loop
    /// order), a materials-free spell passes, and absent def/store skip the check (the ref's
    /// `IsActivePlayer` gate) — the cast then goes out for the server to judge.
    #[test]
    fn missing_materials_refuse_with_the_refs_reasons() {
        let items = crate::items::Items::default();
        let st = store();
        let mining = spell([2901, 0], [(0, 0); 8]);
        let mut errors = CastErrors::default();
        assert!(reagent_totem_refusal(
            2575,
            Some(&mining),
            Some(&st),
            &items,
            &mut errors
        ));
        assert_eq!(errors.0.as_slice(), &[(2575, 0x78)]);

        let mut reagents = [(0, 0); 8];
        reagents[0] = (17056, 1); // Slow Fall's Light Feather
        let slow_fall = spell([0, 0], reagents);
        let mut errors = CastErrors::default();
        assert!(reagent_totem_refusal(
            130,
            Some(&slow_fall),
            Some(&st),
            &items,
            &mut errors
        ));
        assert_eq!(errors.0.as_slice(), &[(130, 0x5c)]);

        let both = spell([2901, 0], reagents);
        let mut errors = CastErrors::default();
        assert!(reagent_totem_refusal(
            1,
            Some(&both),
            Some(&st),
            &items,
            &mut errors
        ));
        assert_eq!(errors.0.as_slice(), &[(1, 0x78)]);

        let plain = spell([0, 0], [(0, 0); 8]);
        let mut errors = CastErrors::default();
        assert!(!reagent_totem_refusal(
            133,
            Some(&plain),
            Some(&st),
            &items,
            &mut errors
        ));
        assert!(!reagent_totem_refusal(
            2575,
            None,
            Some(&st),
            &items,
            &mut errors
        ));
        assert!(!reagent_totem_refusal(
            2575,
            Some(&mining),
            None,
            &items,
            &mut errors
        ));
        assert!(errors.0.is_empty());
    }

    /// The failing-slot selection the fill re-derives: the first MISSING totem / first SHORT
    /// reagent (against empty bags, the first nonzero of each).
    #[test]
    fn first_failing_slot_is_named() {
        let items = crate::items::Items::default();
        let st = store();
        let mut reagents = [(0, 0); 8];
        reagents[1] = (17056, 1);
        let d = spell([0, 7005], reagents);
        assert_eq!(first_missing_totem(&d, &st, &items), Some(7005));
        assert_eq!(first_short_reagent(&d, &st, &items), Some(17056));
        let none = spell([0, 0], [(0, 0); 8]);
        assert_eq!(first_missing_totem(&none, &st, &items), None);
        assert_eq!(first_short_reagent(&none, &st, &items), None);
    }
}
