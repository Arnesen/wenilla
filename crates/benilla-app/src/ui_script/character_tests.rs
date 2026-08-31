//! The shipped **character window** driven end-to-end, engine-only (no Bevy): the real
//! `assets/ui/CharacterFrame.xml` loaded behind `Fonts.xml`/`UiPanels.xml`/`GameTooltip.xml` and fed
//! a synthetic player snapshot + combat-stats + one equipped item — mirroring `questlog_tests.rs`'s
//! harness for the paper-doll slice (decision 0208 phase 1a).
//!
//! NOTE (session state, not a property of this file): at the time this test was written, the
//! `benilla` crate's non-test code (`ui_unit.rs`, `ui_script/mod.rs`'s `demo_unit_feed`) had not yet
//! been updated for `UnitState`'s new `race`/`class`/`class_file`/`race_file`/`sex` fields (a
//! concurrent, unrelated change landing in `benilla-ui/src/script/unit.rs` this same session) — so
//! `cargo test -p benilla` could not compile AT ALL, for reasons entirely outside this file. Every
//! assertion below was independently cross-verified against the real `benilla-ui` engine through a
//! throwaway external harness (a scratch Cargo project depending on `benilla-ui` by path) before
//! landing here, so its correctness doesn't rest on `cargo test -p benilla` having run — but that
//! command itself is still owed once the unrelated breakage clears, per this crate's own gates.

use benilla_ui::script::{
    ExtractedQuad, InvSlotView, InventorySlots, QuadContent, ScriptValue, SoundRequest, UiScript,
    UnitCombatStats, UnitState,
};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the questlog/panel
/// tests' loader, duplicated so this file is self-contained).
fn load_xml(s: &UiScript, file: &str) {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ui")
            .join(file),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "{file}: loader errors: {:?}",
        report.errors
    );
}

/// A Night Elf Warrior, level 12 — the fixture every test below shares for the level/name line.
fn player_unit() -> UnitState {
    UnitState {
        exists: true,
        name: Some("Benilla".into()),
        health: 100,
        max_health: 100,
        level: 12,
        power_type: 0,
        power: 50,
        max_power: 50,
        dead: false,
        reaction: 0,
        race: Some("Night Elf".into()),
        race_file: Some("NightElf".into()),
        class: Some("Warrior".into()),
        class_file: Some("WARRIOR".into()),
        sex: 2,
        is_player: true,
        ..Default::default()
    }
}

/// A minimal combat-stats snapshot: STR effective 15 (a +2 pos buff, so the stat line shows green),
/// 120 armor (school 0), 3 arcane resistance (school 6), a mainhand-only melee weapon (2.6s speed,
/// 10-15 damage, 80 AP) and NO ranged weapon equipped (exercises the "N/A" ranged fallback).
fn combat_stats() -> UnitCombatStats {
    UnitCombatStats {
        stats: [15, 12, 20, 8, 9],
        stat_pos: [2, 0, 0, 0, 0],
        resistances: [120, 5, 0, 0, 0, 0, 3],
        min_damage: 10.0,
        max_damage: 15.0,
        attack_power: 80,
        main_attack_time_ms: 2600,
        main_weapon_skill: (300, 5),
        ..Default::default()
    }
}

/// One item in the head slot (inventory slot id 1, per `GetInventorySlotInfo("HeadSlot")`) —
/// everything else stays empty (exercises the empty-slot art + slot-name tooltip fallback).
/// `equip_slots: vec![1]` — fits ONLY the head slot, decision 0208 phase 1b's fit rule.
fn inventory_with_head_item() -> InventorySlots {
    let mut slots: InventorySlots = Default::default();
    slots[1] = Some(InvSlotView {
        already_bound: false,
        bar_placeable: true,
        durability: None,
        flags: 0,
        item_id: 1234,
        icon: Some("Interface\\Icons\\INV_Helmet_01".into()),
        count: 1,
        contents_count: None,
        quality: 2,
        name: Some("Test Helm".into()),
        link: Some("|cff1eff00|Hitem:1234:0:0:0|h[Test Helm]|h|r".into()),
        locked: false,
        equip_slots: vec![1],
        creator: None,
        enchants: Vec::new(),
    });
    slots
}

/// A one-item backpack whose slot 1 item fits ONLY the head slot (`equip_slots: vec![1]`) — the
/// bag-side half of the doll-interaction tests below.
fn backpack_with_fitting_helm() -> benilla_ui::script::ContainerState {
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        benilla_ui::script::ContainerSlot {
            petition: None,
            already_bound: false,
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Helmet_02".into()),
            count: 1,
            quality: Some(3),
            item_id: 2000,
            link: Some("|cff0070dd|Hitem:2000:0:0:0|h[Another Helm]|h|r".into()),
            locked: false,
            equip_slots: vec![1],
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
            enchants: Vec::new(),
        },
    );
    benilla_ui::script::ContainerState {
        name: Some("Backpack".into()),
        num_slots: 16,
        slots,
    }
}

/// The loader itself: every file the window depends on parses and materializes with no errors —
/// 59 frames (the container + tab + paper-doll page: 19 slots + ammo + 5 attribute rows + armor/
/// attack/damage/ranged rows + 5 resistance frames + the model pane + its 2 rotate buttons + chrome).
#[test]
fn shipped_character_frame_loads_clean() {
    let s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");
}

/// The whole contract in one end-to-end drive: `ToggleCharacter` (the 'C' binding's entry point)
/// opens through `ShowUIPanel`, the level/name lines read the player snapshot, the stat/armor/
/// resistance lines read the combat-stats snapshot (with the ref's own buff-coloring), the ranged
/// block falls back to N/A with no ranged weapon, an equipped item's icon renders once
/// `UNIT_INVENTORY_CHANGED` fires, rotating the model pane advances the booth yaw + plays the kit,
/// and a second toggle closes it again — each transition its own sound.
#[test]
fn shipped_character_frame_drives_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");

    s.set_unit("player", Some(player_unit()));
    s.set_player_combat_stats(Some(combat_stats()));
    s.set_inventory_slots(inventory_with_head_item());

    // Hidden at load: no sound queued (never transitions on startup).
    assert!(
        s.take_sounds().is_empty(),
        "no sound at load (never transitions)"
    );
    assert!(!s.eval::<bool>("return CharacterFrame:IsVisible()").unwrap());

    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert!(s.errors().is_empty(), "open errors: {:?}", s.errors());
    assert!(s.eval::<bool>("return CharacterFrame:IsVisible()").unwrap());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igCharacterInfoOpen".into())],
        "opening plays igCharacterInfoOpen"
    );

    assert_eq!(
        s.eval::<String>("return CharacterNameText:GetText()")
            .unwrap(),
        "Benilla"
    );
    assert_eq!(
        s.eval::<String>("return CharacterLevelText:GetText()")
            .unwrap(),
        "Level 12 Night Elf Warrior"
    );
    // STR effective 15, posBuff 2 > 0 → green (ref PaperDollFrame_SetStats).
    assert_eq!(
        s.eval::<String>("return CharacterStatFrame1StatText:GetText()")
            .unwrap(),
        "|cff20ff2015|r"
    );
    // The labels are the TEMPLATE's own $parentLabel, filled from Lua exactly like the ref
    // (PaperDollFrame_SetStats l.140 for the attributes, _OnLoad l.7-13 for the fixed rows) —
    // never a re-declared region in an instance, which this engine renders as a SECOND
    // anchorless default-font FontString (the white/overlapping-label regression).
    assert_eq!(
        s.eval::<String>("return CharacterStatFrame1Label:GetText()")
            .unwrap(),
        "Strength:"
    );
    assert_eq!(
        s.eval::<String>("return CharacterAttackFrameLabel:GetText()")
            .unwrap(),
        "Melee Attack"
    );
    // Geometry resolves below (GetLeft is nil until the first resolve).
    s.resolve();
    // The template's label sits LEFT-anchored at its row's left edge.
    assert_eq!(
        s.eval::<f32>("return CharacterStatFrame1Label:GetLeft()")
            .unwrap(),
        s.eval::<f32>("return CharacterStatFrame1:GetLeft()")
            .unwrap(),
    );
    // Exactly ONE region carries the label text, in the small gold font (GameFontNormalSmall:
    // 10px, 1.0/0.82/0) — a duplicate or a default-font fallback both fail here.
    let quads = s.extract();
    let strength_labels: Vec<_> = quads
        .iter()
        .filter_map(|q| match &q.content {
            QuadContent::Text {
                text: Some(t),
                color,
                font_height,
                ..
            } if t == "Strength:" => Some((*color, *font_height)),
            _ => None,
        })
        .collect();
    assert_eq!(
        strength_labels.len(),
        1,
        "exactly one Strength: label region, got {}",
        strength_labels.len()
    );
    let (color, height) = strength_labels[0];
    assert_eq!(height, Some(10.0), "GameFontNormalSmall is 10px");
    let c = color.expect("the label carries the font object's color");
    assert!(
        (c[0] - 1.0).abs() < 1e-3 && (c[1] - 0.82).abs() < 1e-3 && c[2].abs() < 1e-3,
        "gold NORMAL_FONT_COLOR, got {c:?}"
    );
    // Armor (school 0) reads straight off UnitArmor's base.
    assert_eq!(
        s.eval::<String>("return CharacterArmorFrameStatText:GetText()")
            .unwrap(),
        "120"
    );
    // MagicResFrame1 = id 6 (arcane) — resistances[6] = 3.
    assert_eq!(
        s.eval::<String>("return MagicResText1:GetText()").unwrap(),
        "3"
    );
    // No ranged weapon in this fixture: the N/A fallback (ref PaperDollFrame_SetRangedAttack).
    assert_eq!(
        s.eval::<String>("return CharacterRangedAttackFrameStatText:GetText()")
            .unwrap(),
        "N/A"
    );

    // The equipped head item's icon shows once UNIT_INVENTORY_CHANGED fires (the real app's own
    // sequencing: a snapshot push is followed by the event, never inferred from the push alone).
    s.fire_event(
        "UNIT_INVENTORY_CHANGED",
        vec![ScriptValue::Str("player".into())],
    );
    assert!(
        s.errors().is_empty(),
        "inventory refresh errors: {:?}",
        s.errors()
    );
    s.resolve();
    let head_icon = s.extract().iter().any(|q| {
        matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("INV_Helmet_01"))
    });
    assert!(head_icon, "the head slot renders the equipped item's icon");

    // Hovering an EMPTY slot (Neck, never populated) shows the slot-name tooltip fallback.
    let neck_center = {
        let l: f32 = s.eval("return CharacterNeckSlot:GetLeft()").unwrap();
        let r: f32 = s.eval("return CharacterNeckSlot:GetRight()").unwrap();
        let t: f32 = s.eval("return CharacterNeckSlot:GetTop()").unwrap();
        let b: f32 = s.eval("return CharacterNeckSlot:GetBottom()").unwrap();
        ((l + r) * 0.5, (t + b) * 0.5)
    };
    s.mouse_move(neck_center.0, neck_center.1);
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "the empty Neck slot shows a tooltip"
    );

    // Hovering a resistance icon WRAPS its long subtext at the tooltip wrap column — the ref
    // passes wrap=1 on every stat/resistance subtext AddLine (ref-PaperDollFrame.xml:115 and
    // siblings); a transcription that drops the flag renders the ~380px sentence as one
    // unwrapped line (the director's tooltip max-width report).
    let res_center = {
        let l: f32 = s.eval("return MagicResFrame1:GetLeft()").unwrap();
        let r: f32 = s.eval("return MagicResFrame1:GetRight()").unwrap();
        let t: f32 = s.eval("return MagicResFrame1:GetTop()").unwrap();
        let b: f32 = s.eval("return MagicResFrame1:GetBottom()").unwrap();
        ((l + r) * 0.5, (t + b) * 0.5)
    };
    s.mouse_move(res_center.0, res_center.1);
    assert!(s.errors().is_empty(), "res hover errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "the resistance icon shows a tooltip"
    );
    s.resolve();
    let quads = s.extract();
    let sub_rect = quads
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. }
                if t.starts_with("Increases the ability to resist") =>
            {
                q.rect
            }
            _ => None,
        })
        .expect("the resistance subtext line renders");
    let w = sub_rect.right - sub_rect.left;
    assert!(
        w <= benilla_ui::widget::TOOLTIP_WRAP_WIDTH + 0.5,
        "the subtext line wraps at the tooltip wrap column, got width {w}"
    );

    // Rotating the model pane advances the booth yaw from the ref's default 0.61 by +0.03/click and
    // plays the rotate kit (ref UIParent.lua:1421-1442).
    s.run("BenillaPaperDollModel_RotateRight(CharacterModelFrame)")
        .unwrap();
    assert!(
        (s.paperdoll_yaw() - 0.64).abs() < 0.001,
        "yaw = {}",
        s.paperdoll_yaw()
    );
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igInventoryRotateCharacter".into())]
    );

    // ToggleCharacter() again closes it through HideUIPanel, playing the close kit.
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert!(s.errors().is_empty(), "close errors: {:?}", s.errors());
    assert!(!s.eval::<bool>("return CharacterFrame:IsVisible()").unwrap());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igCharacterInfoClose".into())],
        "closing plays igCharacterInfoClose"
    );
}

/// Regression — the close button's z-order (the reported "missing red X"): the character window
/// carries no border art of its own; its whole frame-and-background IS `PaperDollFrame`'s
/// BACKGROUND layer, a full-window page created AFTER the close button. Every frame defaults to
/// level 0, so within the window draw order is creation order (`order.rs`) — the page's art painted
/// over the earlier-created button and it vanished. The button's `OnLoad` raises its frame level
/// (the same idiom `CharacterNameFrame` uses), so its art must paint AFTER every one of the
/// page's own quads. Checked on the real extracted draw order, not merely the level integer.
#[test]
fn close_button_draws_above_the_paper_doll_page() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");
    s.set_unit("player", Some(player_unit()));
    // The window must be SHOWN for extract() to emit quads (hidden="true" by default).
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    s.resolve();

    // extract() returns quads already sorted ascending by draw order, so index == paint order.
    let quads = s.extract();
    let owner = |q: &ExtractedQuad| s.quad_owner_name(q.target);

    // The window's page art draws first; its LAST quad is the top-most page pixel.
    let page_last = quads
        .iter()
        .rposition(|q| owner(q).as_deref() == Some("PaperDollFrame"))
        .expect("the paper-doll page renders its background art");

    // The close button's red-X normal texture must render at all …
    let close_x = quads
        .iter()
        .position(|q| {
            owner(q).as_deref() == Some("CharacterFrameCloseButton")
                && matches!(&q.content,
                    QuadContent::Texture { path: Some(p), .. } if p.contains("MinimizeButton-Up"))
        })
        .expect("the close button renders its normal (red-X) texture");

    // … and it must paint AFTER the page — otherwise it's buried, the reported bug.
    assert!(
        close_x > page_last,
        "close button (draw #{close_x}) must paint after the paper-doll page (last at #{page_last})"
    );
}

/// Before the app's first player snapshot lands, `UnitRace`/`UnitClass` answer nil,nil (this
/// engine's own timing — unlike the real client's synchronous local data, this codebase's race/
/// class stream in). The level line's "?" placeholders (CharacterFrame.xml's own `SetLevel`
/// comment) keep `format()` from erroring outright, rather than the window failing to open at all.
#[test]
fn level_line_survives_no_player_snapshot_yet() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");

    // No set_unit("player", ...) call at all — UnitLevel/UnitRace/UnitClass answer their absent
    // shapes (0 / nil,nil / nil,nil, `unit.rs`'s own contract).
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert!(s.errors().is_empty(), "open errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<String>("return CharacterLevelText:GetText()")
            .unwrap(),
        "Level 0 ? ?"
    );

    // A stray extractor's frame-count sanity check: some frame renders (the window itself), no
    // panics anywhere in the drive.
    let quads: Vec<ExtractedQuad> = {
        s.resolve();
        s.extract()
    };
    assert!(!quads.is_empty());
}

/// Decision 0208 phase 1b, end to end: clicking an OCCUPIED doll slot picks the item up onto the
/// cursor (the SAME payload `GetCursorInfo`/`CursorHasItem` read) and dims the slot's icon
/// (`IsInventoryItemLocked` — the held-here derivation, no server round-trip). A second click on
/// the SAME slot cancels: the cursor empties and the dim clears.
#[test]
fn clicking_an_occupied_doll_slot_picks_it_up_and_locks_it() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");
    s.set_unit("player", Some(player_unit()));
    s.set_player_combat_stats(Some(combat_stats()));
    s.set_inventory_slots(inventory_with_head_item());
    // The window must be SHOWN for `extract()` to emit its quads below (hidden="true" by
    // default) — the icon-dim check needs a real render pass, not just the engine-side state.
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    s.take_sounds();
    s.resolve();

    assert!(!s.eval::<bool>("return IsInventoryItemLocked(1)").unwrap());
    assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());

    s.run(r#"BenillaPaperDollSlot_OnClick(CharacterHeadSlot, "LeftButton")"#)
        .unwrap();
    assert!(s.errors().is_empty(), "click errors: {:?}", s.errors());

    assert!(s.eval::<bool>("return CursorHasItem()").unwrap());
    let (kind, id) = s
        .eval::<(String, i64)>("local k, id = GetCursorInfo() return k, id")
        .unwrap();
    assert_eq!((kind.as_str(), id), ("item", 1234));
    assert!(
        s.eval::<bool>("return IsInventoryItemLocked(1)").unwrap(),
        "the picked slot locks"
    );
    // The icon vertex-dims (the bag slots' own convention, this file's own Update comment).
    s.resolve();
    let head_icon_dim = s.extract().iter().any(|q| {
        matches!(&q.content, QuadContent::Texture { path: Some(p), color: Some(c), .. }
            if p.contains("INV_Helmet_01") && c[0] < 0.5)
    });
    assert!(head_icon_dim, "the picked slot's icon dims");

    // Clicking the SAME slot again cancels — mirrors the bag's own same-slot-cancel contract.
    s.run(r#"BenillaPaperDollSlot_OnClick(CharacterHeadSlot, "LeftButton")"#)
        .unwrap();
    assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());
    assert!(!s.eval::<bool>("return IsInventoryItemLocked(1)").unwrap());
}

/// Decision 0208 phase 1b's `CURSOR_UPDATE` highlight: holding a BAG item that fits ONLY the
/// head slot locks the head slot's highlight and leaves every other slot unlocked (the neck slot
/// checked here) — the wiring test for `BenillaPaperDollSlot_LockHighlight`/`_UnlockHighlight`
/// (a Lua-level spy, since no `GetVertexColor` getter exists to read the emulated tint back —
/// this file's own header comment on the emulation).
#[test]
fn cursor_update_highlights_fitting_doll_slots_while_holding_a_bag_item() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");
    s.set_unit("player", Some(player_unit()));
    s.set_player_combat_stats(Some(combat_stats()));
    s.resolve();

    s.run(
        r#"
        lockHighlighted, unlockHighlighted = {}, {}
        local origLock, origUnlock = BenillaPaperDollSlot_LockHighlight, BenillaPaperDollSlot_UnlockHighlight
        BenillaPaperDollSlot_LockHighlight = function(b) lockHighlighted[b:GetName()] = true; origLock(b) end
        BenillaPaperDollSlot_UnlockHighlight = function(b) unlockHighlighted[b:GetName()] = true; origUnlock(b) end
        "#,
    )
    .unwrap();

    s.set_container(0, Some(backpack_with_fitting_helm()));
    s.run("PickupContainerItem(0, 1)").unwrap();
    s.tick(0.0); // dispatches the queued CURSOR_UPDATE to every registered doll slot
    assert!(s.errors().is_empty(), "{:?}", s.errors());

    assert!(
        s.eval::<bool>("return lockHighlighted['CharacterHeadSlot'] == true")
            .unwrap(),
        "the fitting slot locks its highlight"
    );
    assert!(
        s.eval::<bool>("return unlockHighlighted['CharacterNeckSlot'] == true")
            .unwrap(),
        "a non-fitting slot stays unhighlighted"
    );
}

/// The model pane's click-with-payload path (ref `CharacterModelFrame_OnMouseUp`): a left-click
/// release on the model pane while holding a fitting BAG item auto-equips it — the queued
/// `(bag, slot)` source, cursor cleared.
#[test]
fn model_pane_click_auto_equips_a_held_bag_item() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");
    s.set_unit("player", Some(player_unit()));
    s.set_player_combat_stats(Some(combat_stats()));
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    s.take_sounds();
    s.resolve();

    s.set_container(0, Some(backpack_with_fitting_helm()));
    s.run("PickupContainerItem(0, 1)").unwrap();
    assert!(s.eval::<bool>("return CursorHasItem()").unwrap());

    let center = {
        let l: f32 = s.eval("return CharacterModelFrame:GetLeft()").unwrap();
        let r: f32 = s.eval("return CharacterModelFrame:GetRight()").unwrap();
        let t: f32 = s.eval("return CharacterModelFrame:GetTop()").unwrap();
        let b: f32 = s.eval("return CharacterModelFrame:GetBottom()").unwrap();
        ((l + r) * 0.5, (t + b) * 0.5)
    };
    s.mouse_button(center.0, center.1, "LeftButton", true);
    s.mouse_button(center.0, center.1, "LeftButton", false);
    assert!(s.errors().is_empty(), "click errors: {:?}", s.errors());

    assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());
    assert_eq!(s.take_container_autoequips(), vec![(0, 1)]);
}

/// A BROKEN equipped item (durability 0) paints its doll slot red — the icon AND the slot ring,
/// both the ref's 0.9,0,0 (PaperDollItemSlotButton_Update l.670-676; director-directed with the
/// armor guy). Repairing it restores both to white.
#[test]
fn broken_equipped_item_tints_its_doll_slot_red() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");
    s.set_unit("player", Some(player_unit()));
    s.set_player_combat_stats(Some(combat_stats()));

    let mut inv = inventory_with_head_item();
    inv[1].as_mut().unwrap().durability = Some((0, 40));
    s.set_inventory_slots(inv);
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    s.fire_event(
        "UNIT_INVENTORY_CHANGED",
        vec![ScriptValue::Str("player".into())],
    );
    assert!(s.errors().is_empty(), "update errors: {:?}", s.errors());
    s.resolve();
    let color_of = |s: &mut UiScript, needle: &str| {
        s.extract().iter().find_map(|q| match &q.content {
            QuadContent::Texture {
                path: Some(p),
                color,
                ..
            } if p.contains(needle) => Some(color.unwrap_or([1.0, 1.0, 1.0, 1.0])),
            _ => None,
        })
    };
    let red = [0.9, 0.0, 0.0, 1.0];
    assert_eq!(
        color_of(&mut s, "INV_Helmet_01"),
        Some(red),
        "the broken helm's icon paints red"
    );
    // The head slot's ring (its NormalTexture Quickslot2 art) paints red too. Other doll slots
    // share the art but rest white — assert the red one exists among them.
    let ring_red = s.extract().iter().any(|q| {
        matches!(&q.content,
        QuadContent::Texture { path: Some(p), color: Some(c), .. }
            if p.contains("Quickslot2") && (c[0] - 0.9).abs() < 1e-5 && c[1] == 0.0)
    });
    assert!(ring_red, "the broken slot's ring paints red");

    // Repaired: both restore to white.
    let mut inv = inventory_with_head_item();
    inv[1].as_mut().unwrap().durability = Some((40, 40));
    s.set_inventory_slots(inv);
    s.fire_event(
        "UNIT_INVENTORY_CHANGED",
        vec![ScriptValue::Str("player".into())],
    );
    s.resolve();
    assert_eq!(
        color_of(&mut s, "INV_Helmet_01"),
        Some([1.0, 1.0, 1.0, 1.0]),
        "repair restores the icon"
    );
    let ring_red = s.extract().iter().any(|q| {
        matches!(&q.content,
        QuadContent::Texture { path: Some(p), color: Some(c), .. }
            if p.contains("Quickslot2") && (c[0] - 0.9).abs() < 1e-5 && c[1] == 0.0)
    });
    assert!(!ring_red, "repair restores the ring");
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// The director's own gesture, by screen point (2026-07-17 report: "tab switching flaky, can't
/// switch back to Character from Skills"): open the window, click the Skills tab, click a skill
/// row (arming the detail pane), click the Character tab, and round-trip once more — every click
/// routed through the pointer pipeline (`mouse_move`/`mouse_button`) exactly as the app does, so
/// a frame silently eating the tab row's clicks (or a handler error breaking the chain) fails
/// HERE, not on the director's screen. Zero collected handler errors allowed anywhere.
#[test]
fn tab_round_trip_with_a_selected_skill_by_point() {
    use benilla_ui::script::{SkillEntry, SkillsState};

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "ScrollTemplates.xml");
    // Not optional, though this list ran without it: a MISSING template is a loader *warning*, not
    // an error, so an under-loaded list passes load_xml's assert and then fails later on geometry
    // that silently never got built. SkillDetailScrollFrame inherits UIPanelScrollFrameTemplate.
    load_xml(&s, "UIPanelTemplates.xml");
    load_xml(&s, "CharacterFrame.xml");
    load_xml(&s, "SkillFrame.xml");
    s.set_unit("player", Some(player_unit()));
    s.set_skills(SkillsState {
        entries: vec![
            SkillEntry {
                skill_id: 95,
                name: "Defense".into(),
                value: 12,
                max: 60,
                temp_bonus: 0,
                perm_bonus: 0,
                min_level: 0,
                cost_index: 0,
                category_id: 6,
                category_name: "Weapon Skills".into(),
                category_order: 1,
                description: "Defensive expertise.".into(),
                abandonable: false,
                mono: false,
            },
            SkillEntry {
                skill_id: 164,
                name: "Blacksmithing".into(),
                value: 62,
                max: 75,
                temp_bonus: 0,
                perm_bonus: 0,
                min_level: 0,
                cost_index: 0,
                category_id: 11,
                category_name: "Professions".into(),
                category_order: 2,
                description: "Working with metals.".into(),
                // The real 5875 split: primary professions carry SkillRaceClassInfo 0x20.
                abandonable: true,
                mono: false,
            },
        ],
    });

    // The center of the unique visible Text quad `text` — resolves layout first, so it's always
    // the CURRENT rect. Panics with the visible-text inventory if absent (a paint regression).
    fn text_center(s: &mut UiScript, text: &str) -> (f32, f32) {
        s.resolve();
        let quads = s.extract();
        let rect = quads
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Text { text: Some(t), .. } if t == text => q.rect,
                _ => None,
            })
            .unwrap_or_else(|| {
                let visible: Vec<String> = quads
                    .iter()
                    .filter_map(|q| match &q.content {
                        QuadContent::Text { text: Some(t), .. } => Some(t.clone()),
                        _ => None,
                    })
                    .collect();
                panic!("no visible text quad {text:?}; visible texts: {visible:?}");
            });
        (
            (rect.left + rect.right) * 0.5,
            (rect.bottom + rect.top) * 0.5,
        )
    }
    fn click(s: &mut UiScript, (x, y): (f32, f32)) {
        s.mouse_move(x, y);
        s.mouse_button(x, y, "LeftButton", true);
        s.mouse_button(x, y, "LeftButton", false);
    }
    let shown = |s: &mut UiScript, name: &str| {
        s.eval::<bool>(&format!("return {name}:IsVisible()"))
            .unwrap()
    };

    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert!(shown(&mut s, "PaperDollFrame"), "opens on the doll page");

    // Tab to Skills — by point, the director's gesture.
    let tab2 = text_center(&mut s, "Skills");
    click(&mut s, tab2);
    assert!(shown(&mut s, "SkillFrame"), "Skills tab shows the page");
    assert!(!shown(&mut s, "PaperDollFrame"), "doll page yields");

    // Select a skill row — arms the detail pane (the report's precondition).
    let row = text_center(&mut s, "Blacksmithing");
    click(&mut s, row);
    assert!(
        s.eval::<i64>("return GetSelectedSkill()").unwrap() > 0,
        "the row click selects"
    );
    assert!(shown(&mut s, "SkillDetailStatusBar"), "detail bar arms");
    // The description body renders through the SKILL_DESCRIPTION format (skillType is "" in every
    // reachable 1.12 branch) — the nil-global half of the report, pinned.
    assert_eq!(
        s.eval::<String>("return SkillDetailDescriptionText:GetText()")
            .unwrap(),
        "|cffffffff|r Working with metals.",
        "the detail description renders via SKILL_DESCRIPTION"
    );

    // The abandon slice: a primary profession offers the unlearn button; its confirm formats the
    // name; accepting queues the CMSG_UNLEARN_SKILL intent BY SKILL ID and removes nothing
    // locally (the server's SetSkill(id,0,0) round trip owns the removal).
    assert!(
        shown(&mut s, "SkillDetailStatusBarUnlearnButton"),
        "the unlearn button shows for a profession"
    );
    s.run("SkillDetailStatusBarUnlearnButton:Click()").unwrap();
    assert!(
        shown(&mut s, "StaticPopup1"),
        "the UNLEARN_SKILL confirm opens"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "Do you want to unlearn Blacksmithing?"
    );
    s.run("StaticPopup1Button1:Click()").unwrap();
    assert_eq!(
        s.take_skill_abandons(),
        vec![164],
        "accept queues the skill id"
    );
    assert!(!shown(&mut s, "StaticPopup1"), "accept closes the confirm");
    assert_eq!(
        s.eval::<i64>("return GetNumSkillLines()").unwrap(),
        4,
        "nothing is removed locally"
    );

    // A weapon line never offers it.
    let defense = text_center(&mut s, "Defense");
    click(&mut s, defense);
    assert!(
        !shown(&mut s, "SkillDetailStatusBarUnlearnButton"),
        "no unlearn button for Defense"
    );
    // Restore the profession selection so the round trip below leaves familiar state.
    let row = text_center(&mut s, "Blacksmithing");
    click(&mut s, row);

    // Tab BACK to Character — the reported failure.
    let tab1 = text_center(&mut s, "Character");
    assert_eq!(
        s.hit_test_name(tab1.0, tab1.1).as_deref(),
        Some("CharacterFrameTab1"),
        "the Character tab OWNS its point while Skills is up (the wheel catcher must not)"
    );
    click(&mut s, tab1);
    assert!(
        shown(&mut s, "PaperDollFrame"),
        "the Character tab switches back (the 2026-07-17 report)"
    );
    assert!(!shown(&mut s, "SkillFrame"), "Skills page yields");

    // Once more around — "flaky" only shows up on repetition.
    let tab2 = text_center(&mut s, "Skills");
    click(&mut s, tab2);
    assert!(shown(&mut s, "SkillFrame"), "second trip to Skills");
    let tab1 = text_center(&mut s, "Character");
    click(&mut s, tab1);
    assert!(shown(&mut s, "PaperDollFrame"), "second trip back");

    assert!(
        s.errors().is_empty(),
        "zero handler errors across the whole trip: {:?}",
        s.errors()
    );
}

/// The rotate arrows, driven by the pointer exactly as a hand does (the director's report:
/// "the 2 arrows for turning barely move it at all"). The ref's feel is TWO mechanisms, and the
/// pane used to ship only the smaller one:
///
/// - a **tap** fires `OnClick` on BOTH edges (`RegisterForClicks("LeftButtonDown","LeftButtonUp")`,
///   ref PaperDollFrame.xml l.241-243) — 2 × 0.03 rad, ~3.4°, which is all a tap is meant to be;
/// - **holding** spins at `ROTATIONS_PER_SECOND` = half a turn per second through the pane's
///   `OnUpdate` (ref `Model_OnUpdate`, UIParent.lua:1444-1462) — 100× the tap per second, and the
///   only reason the arrows feel like anything.
///
/// The ref's held branches also run OPPOSITE to its click helpers (held-LEFT adds where
/// click-LEFT subtracts); that quirk is quoted, so it is asserted here rather than "corrected".
#[test]
fn rotate_arrows_tap_twice_and_spin_while_held() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    // ROTATIONS_PER_SECOND lives here (the ref's UIParent.lua:2), like it does in the real client.
    load_xml(&s, "UIParent.xml");
    load_xml(&s, "CharacterFrame.xml");
    s.set_unit("player", Some(player_unit()));
    s.set_player_combat_stats(Some(combat_stats()));
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    s.resolve();

    // The left arrow's centre, in screen points.
    let btn = "CharacterModelFrameRotateLeftButton";
    let (bx, by) = {
        let l: f32 = s.eval(&format!("return {btn}:GetLeft()")).unwrap();
        let r: f32 = s.eval(&format!("return {btn}:GetRight()")).unwrap();
        let t: f32 = s.eval(&format!("return {btn}:GetTop()")).unwrap();
        let b: f32 = s.eval(&format!("return {btn}:GetBottom()")).unwrap();
        ((l + r) * 0.5, (t + b) * 0.5)
    };

    // Press and HOLD. The press edge is itself a click (−0.03 off the 0.61 default).
    s.mouse_move(bx, by);
    s.mouse_button(bx, by, "LeftButton", true);
    let pressed = s.paperdoll_yaw();
    assert!(
        (pressed - 0.58).abs() < 1e-4,
        "the press edge nudges once: {pressed}"
    );

    // Held for half a second: half of half a turn = +π/2 (the held-LEFT branch ADDS).
    s.tick(0.5);
    assert!(s.errors().is_empty(), "OnUpdate errors: {:?}", s.errors());
    let spun = s.paperdoll_yaw();
    assert!(
        (spun - (pressed + std::f32::consts::FRAC_PI_2)).abs() < 1e-3,
        "half a second held spins half of half a turn: {pressed} → {spun}"
    );

    // Release: the second click edge (another −0.03), and the spin stops dead.
    s.mouse_button(bx, by, "LeftButton", false);
    let released = s.paperdoll_yaw();
    assert!(
        (released - (spun - 0.03)).abs() < 1e-4,
        "the release edge nudges again: {spun} → {released}"
    );
    s.tick(0.5);
    assert!(
        (s.paperdoll_yaw() - released).abs() < 1e-6,
        "a released button does not keep spinning"
    );

    // The right arrow spins the other way (held-RIGHT subtracts).
    let rbtn = "CharacterModelFrameRotateRightButton";
    let (rx, ry) = {
        let l: f32 = s.eval(&format!("return {rbtn}:GetLeft()")).unwrap();
        let r: f32 = s.eval(&format!("return {rbtn}:GetRight()")).unwrap();
        let t: f32 = s.eval(&format!("return {rbtn}:GetTop()")).unwrap();
        let b: f32 = s.eval(&format!("return {rbtn}:GetBottom()")).unwrap();
        ((l + r) * 0.5, (t + b) * 0.5)
    };
    s.mouse_move(rx, ry);
    s.mouse_button(rx, ry, "LeftButton", true);
    let before = s.paperdoll_yaw();
    s.tick(0.25);
    let after = s.paperdoll_yaw();
    assert!(
        (after - (before - std::f32::consts::FRAC_PI_4)).abs() < 1e-3,
        "the right arrow spins the other way: {before} → {after}"
    );
    s.mouse_button(rx, ry, "LeftButton", false);
    assert!(s.errors().is_empty(), "no handler errors: {:?}", s.errors());
}

/// **The keybind and the tab row must agree** (the director's 2026-08-06 report: *"go on skills tab
/// and press c, it goes back to char tab but without switching the actual tab ui below properly,
/// then if I click on char tab, it closes the whole window"*).
///
/// The cause was a dropped line: the reference selects the tab inside `ToggleCharacter` itself
/// (`PanelTemplates_SetTab(CharacterFrame, subFrame:GetID())`, ref `CharacterFrame.lua:11`), on
/// EVERY path. Ours only did it in the tab button's own `OnClick`, so the two entry points into the
/// window diverged: a keybind switched the page and left the row selecting the old tab, and the
/// next click on the *now-showing* page's tab hit `ToggleCharacter`'s "this page is already
/// visible" branch and shut the window instead of switching back.
///
/// Driven through the same entry point the `C` / `SHIFT-P` bindings use — a bare
/// `ToggleCharacter(page)` call, never the button — because that is precisely the path the row used
/// to miss.
#[test]
fn a_keybind_page_switch_moves_the_tab_row_with_it() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "UIParent.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "ScrollTemplates.xml");
    load_xml(&s, "UIPanelTemplates.xml");
    load_xml(&s, "OptionsFrameTemplates.xml");
    load_xml(&s, "CharacterFrame.xml");
    load_xml(&s, "PetPaperDollFrame.xml");
    load_xml(&s, "ReputationFrame.xml");
    load_xml(&s, "SkillFrame.xml");
    load_xml(&s, "HonorFrame.xml");
    s.set_unit("player", Some(player_unit()));

    // THE INVARIANT the fix rests on: a page's `id=` is its slot in this window's own tab row, and
    // `BENILLA_CHARACTERFRAME_SUBFRAMES` is the same 1:1 mapping written the other way round. It
    // did its job: this loop is what made the Reputation page's arrival LOUD, since Skills had to
    // move from 3 to 4 in the same breath. All five slots are the reference's own now
    // (Character/Pet/Reputation/Skills/Honor, ref `CharacterFrame.xml:79-168`), so the loop covers
    // the whole row rather than a prefix of it — there is nothing left past the end for a page to
    // arrive into unnoticed.
    for i in 1..=5 {
        let id: i64 = s
            .eval(&format!(
                "return getglobal(BENILLA_CHARACTERFRAME_SUBFRAMES[{i}]):GetID()"
            ))
            .unwrap();
        assert_eq!(
            id, i,
            "page {i} of BENILLA_CHARACTERFRAME_SUBFRAMES must carry id={i}"
        );
    }

    let selected = |s: &mut UiScript| {
        s.eval::<i64>("return PanelTemplates_GetSelectedTab(CharacterFrame)")
            .unwrap()
    };
    // A tab wears its "Active" (=Disabled) art exactly when it is the selected one — the visible
    // half of the same fact, so a SetTab that updated the number without repainting fails here too.
    let wearing_active_art = |s: &mut UiScript, tab: u32| {
        s.eval::<bool>(&format!(
            "return CharacterFrameTab{tab}MiddleDisabled:IsVisible()"
        ))
        .unwrap()
    };
    let shown = |s: &mut UiScript, name: &str| {
        s.eval::<bool>(&format!("return {name}:IsVisible()"))
            .unwrap()
    };

    // Open on Character (the `C` binding), then move to Skills the same way.
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert_eq!(selected(&mut s), 1);
    s.run(r#"ToggleCharacter("SkillFrame")"#).unwrap();
    assert!(shown(&mut s, "SkillFrame"));
    assert_eq!(selected(&mut s), 4, "the row follows a keybind to Skills");
    assert!(wearing_active_art(&mut s, 4));
    assert!(!wearing_active_art(&mut s, 1));

    // THE REPORT: `C` from the Skills page. The page goes back to Character — and so must the row.
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert!(shown(&mut s, "PaperDollFrame"));
    assert!(!shown(&mut s, "SkillFrame"));
    assert_eq!(
        selected(&mut s),
        1,
        "the tab row followed the keybind back to Character"
    );
    assert!(wearing_active_art(&mut s, 1));
    assert!(!wearing_active_art(&mut s, 4));

    // …and the second half of the report — *"then if I click on char tab, it closes the whole
    // window"* — falls out of the same fix rather than needing its own. `PanelTemplates_SelectTab`
    // DISABLES the tab it selects (a selected tab is not re-clickable), so with the row in step the
    // Character tab is inert while the Character page is up. The window only closed because the row
    // still said Skills: tab 1 was left enabled, the click reached `ToggleCharacter`, and the page
    // it named was already visible — the HideUIPanel arm.
    s.run("CharacterFrameTab1:Click()").unwrap();
    assert!(
        shown(&mut s, "CharacterFrame"),
        "the selected tab is disabled — clicking it cannot close the window"
    );
    assert!(shown(&mut s, "PaperDollFrame"), "…or change the page");
    assert!(s.errors().is_empty(), "no handler errors: {:?}", s.errors());
}

/// **The tab kit's XML-facing entry point, driven the way an addon drives it.**
///
/// Four corpus addons (Enchantrix, Outfitter, SimpleActionSets, TheoryCraft) put
/// `PanelTemplates_Tab_OnClick(<frame>)` straight into a tab's `<OnClick>` and let the kit do the
/// rest; this client had every other member of the kit and not that one, because our own windows
/// wire their tabs to their own handlers and never reached for the generic entry point.
///
/// **It is built on a row of this test's own, and that is the finding, not a convenience.** The
/// reference's tab buttons carry `id="1".."4"` (ref `CharacterFrame.xml:79-133`) and ours carry
/// none: our row is id-based through `BenillaCharacterFrameTab_OnClick(id)`, which closes over the
/// number instead of reading it off the widget. So `CharacterFrameTab2:GetID()` is **0** here, and
/// driving OUR row through the generic entry point would select tab 0 — correct code meeting a
/// window that does not obey the contract it reads. An addon's own tabs do carry ids, which is why
/// the four callers above work; this builds a row that obeys the reference's contract and drives
/// that.
///
/// The `this`/frame split is the whole reason the function exists: `this` is the clicked TAB and
/// the owning FRAME is a separate argument, which is the shape `SimpleActionSets.xml:342` relies on
/// when it passes `this:GetParent()`.
#[test]
fn an_addons_tab_click_selects_through_the_generic_entry_point() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "UIParent.xml");

    // A conforming row: tabs named `<frame>Tab1..N` (what `PanelTemplates_UpdateTabs` getglobals)
    // and each carrying its own id, exactly as an addon's XML declares them.
    s.run(
        r#"
        TabKitFrame = CreateFrame("Frame", "TabKitFrame", UIParent)
        PanelTemplates_SetNumTabs(TabKitFrame, 2)
        for i = 1, 2 do
            local t = CreateFrame("Button", "TabKitFrameTab" .. i, TabKitFrame,
                                  "CharacterFrameTabButtonTemplate")
            t:SetID(i)
        end
        PanelTemplates_SetTab(TabKitFrame, 1)
        "#,
    )
    .unwrap();
    let selected = |s: &mut UiScript| {
        s.eval::<i64>("return PanelTemplates_GetSelectedTab(TabKitFrame)")
            .unwrap()
    };
    assert_eq!(selected(&mut s), 1);

    // The addon idiom, verbatim: `this` is the tab, the frame is the argument.
    s.run("this = TabKitFrameTab2; PanelTemplates_Tab_OnClick(TabKitFrame); this = nil")
        .unwrap();
    assert_eq!(
        selected(&mut s),
        2,
        "the kit takes the tab's OWN id from `this`, not from the frame it was handed"
    );
    // The row repainted, not just the number — the same visible half the keybind test asserts.
    assert!(
        s.eval::<bool>("return TabKitFrameTab2MiddleDisabled:IsVisible()")
            .unwrap(),
        "the newly selected tab wears the Active art"
    );
    assert!(s.errors().is_empty(), "no handler errors: {:?}", s.errors());
}
