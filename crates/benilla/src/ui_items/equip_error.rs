//! The wire `InventoryResult` → GlobalStrings-key vocabulary for the red error line — the
//! inventory half of the refusal-message law (`ui_action::errors`: every string comes from the
//! VM's own loaded `GlobalStrings.lua`, never hardcoded; `mount_result_key` is the twin table).
//! The drain lives in [`super::feed::feed_containers`].

/// A wire `InventoryResult` refusal → its GlobalStrings key, resolved to text through the VM's
/// own loaded `GlobalStrings.lua` at the drain. The FULL build-5875 table: codes VERIFIED
/// against vmangos `ItemDefines.h` (the wire's author — sequential with every `#if` band ≤ 5875
/// included: stunned=37, dead=38, INVENTORY_FULL=50; an earlier table had the TBC-era 39/40 for
/// stunned/dead — on this wire those are CANT_DO_RIGHT_NOW / INT_BAG_ERROR); keys are the
/// GlobalStrings tag each enum entry carries, every one verified present in the shipped 1.12
/// `GlobalStrings.lua` (patch-2.MPQ — the resolution test below). Reason 1's string carries a
/// `%d` the drain fills with the packet's required level. `None` — code 59 (`EQUIP_ERR_NONE`,
/// whose ERR_CANT_BE_DISENCHANTED tag has no 1.12 string) and anything past the enum — gets the
/// drain's hex debug line.
pub(super) fn equip_error_key(reason: u8) -> Option<&'static str> {
    Some(match reason {
        1 => "ERR_CANT_EQUIP_LEVEL_I",
        2 => "ERR_CANT_EQUIP_SKILL",
        3 => "ERR_WRONG_SLOT",
        // 4 BAG_FULL; 51 BANK_FULL and the BAG_FULL3/4/6 aliases all read ERR_BAG_FULL.
        4 | 51 | 53 | 56 | 62 => "ERR_BAG_FULL",
        5 => "ERR_BAG_IN_BAG",
        6 => "ERR_TRADE_EQUIPPED_BAG",
        7 => "ERR_AMMO_ONLY",
        8 => "ERR_PROFICIENCY_NEEDED",
        9 | 12 | 18 => "ERR_NO_SLOT_AVAILABLE",
        10 | 11 => "ERR_CANT_EQUIP_EVER",
        13 => "ERR_2HANDED_EQUIPPED",
        14 => "ERR_2HSKILLNOTFOUND",
        15 | 16 => "ERR_WRONG_BAG_TYPE",
        17 => "ERR_ITEM_MAX_COUNT",
        19 | 55 => "ERR_CANT_STACK",
        20 => "ERR_NOT_EQUIPPABLE",
        21 => "ERR_CANT_SWAP",
        22 => "ERR_SLOT_EMPTY",
        23 | 54 => "ERR_ITEM_NOT_FOUND",
        24 => "ERR_DROP_BOUND_ITEM",
        25 => "ERR_OUT_OF_RANGE",
        26 => "ERR_TOO_FEW_TO_SPLIT",
        27 => "ERR_SPLIT_FAILED",
        28 => "ERR_SPELL_FAILED_REAGENTS_GENERIC",
        29 => "ERR_NOT_ENOUGH_MONEY",
        30 => "ERR_NOT_A_BAG",
        31 => "ERR_DESTROY_NONEMPTY_BAG",
        32 => "ERR_NOT_OWNER",
        33 => "ERR_ONLY_ONE_QUIVER",
        34 => "ERR_NO_BANK_SLOT",
        35 => "ERR_NO_BANK_HERE",
        36 => "ERR_ITEM_LOCKED",
        37 => "ERR_GENERIC_STUNNED",
        38 => "ERR_PLAYER_DEAD",
        39 => "ERR_CLIENT_LOCKED_OUT",
        40 => "ERR_INTERNAL_BAG_ERROR",
        // ERR_ONLY_ONE_BOLT's 1.12 string genuinely reads "quiver", same as 33's.
        41 => "ERR_ONLY_ONE_BOLT",
        42 => "ERR_ONLY_ONE_AMMO",
        43 => "ERR_CANT_WRAP_STACKABLE",
        44 => "ERR_CANT_WRAP_EQUIPPED",
        45 => "ERR_CANT_WRAP_WRAPPED",
        46 => "ERR_CANT_WRAP_BOUND",
        47 => "ERR_CANT_WRAP_UNIQUE",
        48 => "ERR_CANT_WRAP_BAGS",
        49 => "ERR_LOOT_GONE",
        50 => "ERR_INV_FULL",
        52 | 57 => "ERR_VENDOR_SOLD_OUT",
        58 => "ERR_OBJECT_IS_BUSY",
        60 => "ERR_NOT_IN_COMBAT",
        61 => "ERR_NOT_WHILE_DISARMED",
        63 => "ERR_CANT_EQUIP_RANK",
        64 => "ERR_CANT_EQUIP_REPUTATION",
        65 => "ERR_TOO_MANY_SPECIAL_BAGS",
        66 => "ERR_LOOT_CANT_LOOT_THAT_NOW",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::equip_error_key;

    /// Pins the build-5875 `InventoryResult` positions against the sender's enum (vmangos
    /// `ItemDefines.h`, every band ≤ 5875 included). The director's live repro: a full-inventory
    /// vendor buy arrives as 0x32 = 50 = INVENTORY_FULL. Stunned/dead sit at 37/38 on this wire —
    /// an earlier table had the TBC-era 39/40, which here mean "right now"/"Internal Bag Error".
    #[test]
    fn equip_error_table_matches_the_5875_enum() {
        assert_eq!(equip_error_key(50), Some("ERR_INV_FULL"));
        assert_eq!(equip_error_key(37), Some("ERR_GENERIC_STUNNED"));
        assert_eq!(equip_error_key(38), Some("ERR_PLAYER_DEAD"));
        assert_eq!(equip_error_key(39), Some("ERR_CLIENT_LOCKED_OUT"));
        assert_eq!(equip_error_key(40), Some("ERR_INTERNAL_BAG_ERROR"));
        assert_eq!(equip_error_key(1), Some("ERR_CANT_EQUIP_LEVEL_I"));
        // 59 (EQUIP_ERR_NONE → ERR_CANT_BE_DISENCHANTED) has no 1.12 string; 67 is past the
        // 5875 enum — both take the drain's hex debug fallback.
        assert_eq!(equip_error_key(59), None);
        assert_eq!(equip_error_key(67), None);
    }

    /// The RUNTIME leg on the real data (the `mount_result_key` test's pattern): every key the
    /// table can emit resolves to a non-empty string in the shipped 1.12 `GlobalStrings.lua` —
    /// the guard against a typo'd key degrading a real refusal to the hex debug line. Also pins
    /// the director's repro end-to-end (0x32 → "Inventory is full.") and reason 1's `%d` fill.
    /// Skips without client data.
    #[test]
    fn every_equip_error_key_resolves_in_the_real_global_strings() {
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

        for reason in 0..=66u8 {
            let Some(key) = equip_error_key(reason) else {
                continue; // 0=OK (filtered upstream), 59 (no 1.12 string)
            };
            let text = g(key).unwrap_or_default();
            assert!(!text.is_empty(), "{key} (reason {reason}) missing");
        }
        assert_eq!(
            g(equip_error_key(50).unwrap()).unwrap(),
            "Inventory is full."
        );
        assert_eq!(
            g(equip_error_key(1).unwrap()).unwrap().replace("%d", "30"),
            "You must reach level 30 to use that item."
        );
    }
}
