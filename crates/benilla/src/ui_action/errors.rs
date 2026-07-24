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

/// One client-LOCAL red-line message: a GlobalStrings key plus the `DisplayError` argText
/// fills. The 1.12 error formats use at most one `%s` and one `%d` ("Requires %s",
/// "Requires %s %d" — wow-re cursor-system.md §8.8's lock-refusal toasts, decision 0545);
/// a key whose string carries no token ignores its fills.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UiError {
    pub key: &'static str,
    pub fill_s: Option<String>,
    pub fill_d: Option<u32>,
}

impl UiError {
    /// A fill-less message — the plain-key tenants (`ERR_ATTACK_MOUNTED`, the flag-locked
    /// strategy defaults).
    pub(crate) fn key(key: &'static str) -> Self {
        Self {
            key,
            fill_s: None,
            fill_d: None,
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
/// `None` (absent or empty key) = show nothing: GlobalStrings data-suppression, faithfully.
pub(super) fn ui_error_text(e: &UiError, get: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut text = get(e.key)?;
    if let Some(s) = &e.fill_s {
        text = text.replace("%s", s);
    }
    if let Some(d) = e.fill_d {
        text = text.replace("%d", &d.to_string());
    }
    (!text.is_empty()).then_some(text)
}

/// The client-side attack-start mounted refusal (decision 0481; wow-re
/// `mounted-action-gate.md` §5: the shared attack-start validator `0x612df0` reads
/// `UNIT_FIELD_MOUNTDISPLAYID` at `0x613039` and, if live, shows `DisplayError` errorId `0xa4`
/// = `ERR_ATTACK_MOUNTED` "Can't attack while mounted." — BEFORE the nearest-enemy core at
/// `0x6130b5`). The ref checks once at its single attack-start funnel; our attack initiations
/// (the Attack button, the right-click attack, the nearest-acquire) each ask here first.
pub(crate) fn attack_mounted_refusal(
    self_store: Option<&ObjectStore>,
    errors: &mut UiErrorKeys,
) -> bool {
    let mounted = self_store.is_some_and(|s| s.0.unit_mount_display_id() > 0);
    if mounted {
        debug!("attack refused locally — mounted (ERR_ATTACK_MOUNTED)");
        errors.0.push(UiError::key("ERR_ATTACK_MOUNTED"));
    }
    mounted
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
        .find(|&t| crate::ui_items::count_of(&store.0, items, t) == 0)
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
        .find(|&(id, n)| crate::ui_items::count_of(&store.0, items, id) < n)
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
