//! The emission-order law and the red requirement checks: the verified line families, the
//! player-state-tracked reds (level/class/rep), and the slot|type cells' independent
//! proficiency reds (hard miss on TYPE; alternate/dual-wield on SLOT).

use std::collections::HashMap;

use super::{axe, lines_of, right_color, script};
use crate::script::*;

/// The full line law over a rich weapon + the red checks: a level-25 warrior fails the level
/// requirement (red) but passes the class list (white).
#[test]
fn item_line_law_and_red_requirements() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(871, axe());
    s.set_player_req_state(PlayerReqState {
        level: 25,
        class_id: 1, // Warrior — allowed
        race_id: 1,
        skills: HashMap::new(),
        ..Default::default()
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetItemById(871)
        assert(tt:IsShown(), "SetItemById shows")
    "#,
    )
    .unwrap();
    let lines = lines_of(&mut s);
    let texts: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "Ravager",
            "Binds when equipped",
            "Two-Hand",
            "68 - 103 Damage",
            "+ 2 - 4 Shadow Damage",
            "(25.3 damage per second)",
            // Display order, NOT wire order: the fixture feeds (Stamina, Strength) but the
            // 0x808e88 table prints Strength first (STR,AGI,STA,INT,SPI,HP,MANA).
            "+9 Strength",
            "+12 Stamina",
            "+10 Shadow Resistance",
            "Durability 90 / 90",
            "Classes: Warrior, Rogue",
            "Requires Level 37",
            "Chance on hit: Ravager",
            "\"A wicked axe of the Scarlet Crusade.\"",
        ],
        "the verified line order (0276)"
    );
    // The name wears the Rare blue; the failed level line is red; the passed class line white.
    assert_eq!(lines[0].1, [0.0, 0.439, 0.867, 1.0], "rare-blue name");
    let class_line = &lines[10];
    assert_eq!(
        class_line.1,
        [1.0, 1.0, 1.0, 1.0],
        "class list passes → white"
    );
    let level_line = &lines[11];
    assert_eq!(
        level_line.1,
        [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0],
        "level 25 < 37 → red"
    );
    // The right column of the slot|type pair carries the subclass.
    let ty: String = s.eval("return TTTextRight3:GetText()").unwrap();
    assert_eq!(ty, "Axe");
    // The speed column of the damage pair.
    let speed: String = s.eval("return TTTextRight4:GetText()").unwrap();
    assert_eq!(speed, "Speed 3.50");
    assert!(s.take_errors().is_empty());
}

/// The red law flips with player state: leveling past the requirement turns the line white; a
/// class outside the list turns the class line red.
#[test]
fn red_lines_track_player_state() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(871, axe());
    s.set_player_req_state(PlayerReqState {
        level: 60,
        class_id: 8, // Mage — not in {Warrior, Rogue}
        race_id: 1,
        skills: HashMap::new(),
        ..Default::default()
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot2"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetItemById(871)
    "#,
    )
    .unwrap();
    let lines = lines_of(&mut s);
    let find = |needle: &str| {
        lines
            .iter()
            .find(|(t, _)| t.starts_with(needle))
            .unwrap_or_else(|| panic!("no line starting {needle}"))
            .1
    };
    assert_eq!(
        find("Requires Level"),
        [1.0, 1.0, 1.0, 1.0],
        "60 ≥ 37 → white"
    );
    assert_eq!(
        find("Classes:"),
        [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0],
        "mage → red"
    );
    assert!(s.take_errors().is_empty());
}

/// The §5-verified families folded back 2026-07-10 (tooltip-content-law.md): SIGNABLE green,
/// UNIQUE before STARTS_QUEST, LOCKED red, six-equal resistances collapse to the ALL line (and
/// Holy never prints singly), a known taught spell reds "Already known", the description gold —
/// and NO openable line: `SetItemById` is a template source, and the whole openable/readable/
/// creator tail rides the ref's item-OBJECT gate (`0x52e1c7`/`0x52e2e0` — byte-read 2026-07-20;
/// the instance-tail law itself is in [`instance_tail_creator_and_readable`]).
#[test]
fn verified_families_signable_locked_resists_known() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(
        5518,
        ItemTemplateView {
            name: "Sealed Charter".into(),
            quality: 1,
            flags: 0x2000 | 0x4, // signable + openable
            max_count: 1,
            start_quest: 42,
            lock_id: 7,
            resistances: [5, 5, 5, 5, 5, 5],
            spell_triggers: vec![(6, 2020, "Recipe: Stew".into())],
            description: "Sign here.".into(),
            ..Default::default()
        },
    );
    // The player already knows the taught spell 2020.
    s.set_spellbook(SpellBookState {
        tabs: Vec::new(),
        slots: vec![SpellSlotView {
            spell_id: 2020,
            ..Default::default()
        }],
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot5"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetItemById(5518)
    "#,
    )
    .unwrap();
    let lines = lines_of(&mut s);
    let texts: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "Sealed Charter",
            "<Right Click for Details>",
            "Unique",
            "This Item Begins a Quest",
            "Locked",
            "+5 to All Resistances",
            "Already known",
            "\"Sign here.\"",
        ],
        "the verified gated families in the verified order"
    );
    let color = |needle: &str| lines.iter().find(|(t, _)| t == needle).unwrap().1;
    let red = [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0];
    assert_eq!(color("Locked"), red, "LOCKED is red");
    assert_eq!(
        color("Already known"),
        red,
        "SPELL_KNOWN is unconditional red"
    );
    assert_eq!(
        color("<Right Click for Details>"),
        [0.0, 1.0, 0.0, 1.0],
        "SIGNABLE is green"
    );
    assert_eq!(
        color("\"Sign here.\""),
        [1.0, 210.0 / 255.0, 0.0, 1.0],
        "the description is the byte-verified gold"
    );
    // Holy exclusion: a lone Holy resist prints nothing.
    s.set_item_template(
        5519,
        ItemTemplateView {
            name: "Blessed Trinket".into(),
            quality: 1,
            resistances: [10, 0, 0, 0, 0, 0],
            ..Default::default()
        },
    );
    s.run(
        r#"
        TT:SetOwner(Slot5, "ANCHOR_RIGHT")
        TT:SetItemById(5519)
        assert(TT:NumLines() == 1, "a lone Holy resist prints no line, got " .. TT:NumLines())
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// The slot|type line's independent cell reds (byte-read at the builder's coloring legs
/// against the verified AddLine-CORE signature — NB law §10's prose swaps the cells): a hard
/// proficiency miss (`0xc4d4a0[class]` bit `1 << subclass`) reds the TYPE cell; the SLOT cell
/// reds when the weapon is usable only via its alternate subclass (ItemSubClass
/// prereq/postreq) or is an off-hand weapon without Dual Wield (`0x5eab70`). Plus the
/// reputation-requirement red (§1-RED's standing leg) and the hidden-type suppression.
#[test]
fn proficiency_and_reputation_reds() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    let mut item = axe(); // class 2, subclass 1 (2H axe), invtype 17 "Two-Hand"
    item.required_rep_faction = 72; // Stormwind
    item.required_rep_rank = 5; // Honored
    item.required_rep_line = Some("Requires Stormwind - Honored".into());
    s.set_item_template(871, item.clone());
    // A player who knows 2H axes (bit 1) and stands Honored: everything white.
    let mut req = PlayerReqState {
        level: 60,
        class_id: 1,
        race_id: 1,
        ..Default::default()
    };
    req.proficiency.insert(2, 1 << 1);
    req.rep_ranks.insert(72, 5);
    s.set_player_req_state(req.clone());
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot9"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetItemById(871)
    "#,
    )
    .unwrap();
    let white = [1.0, 1.0, 1.0, 1.0];
    let red = [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0];
    let color = |lines: &[(String, [f32; 4])], needle: &str| {
        lines
            .iter()
            .find(|(t, _)| t == needle)
            .unwrap_or_else(|| panic!("no line {needle:?}"))
            .1
    };
    let lines = lines_of(&mut s);
    assert_eq!(color(&lines, "Two-Hand"), white, "proficient slot is white");
    assert_eq!(
        right_color(&mut s, "Axe"),
        white,
        "proficient type is white"
    );
    assert_eq!(
        color(&lines, "Requires Stormwind - Honored"),
        white,
        "met reputation is white"
    );
    // 1H-axes-only mask, no alternate on the view: the hard miss reds the TYPE cell and the
    // slot stays white. Friendly standing reds the rep line.
    let mut req2 = req;
    req2.proficiency.insert(2, 1 << 0);
    req2.rep_ranks.insert(72, 4);
    s.set_player_req_state(req2.clone());
    s.run(r#"TT:SetOwner(Slot9, "ANCHOR_RIGHT"); TT:SetItemById(871)"#)
        .unwrap();
    let lines = lines_of(&mut s);
    assert_eq!(
        color(&lines, "Two-Hand"),
        white,
        "slot survives a hard miss"
    );
    assert_eq!(right_color(&mut s, "Axe"), red, "the type cell carries it");
    assert_eq!(
        color(&lines, "Requires Stormwind - Honored"),
        red,
        "unmet reputation is red"
    );
    // The same mask with the view's alternate resolved to 1H axes (the real (2,1) row's
    // prerequisite): usable-via-alternate reds the SLOT cell instead; the type goes white.
    let mut alt = item;
    alt.proficiency_alt = Some(0);
    s.set_item_template(871, alt);
    s.run(r#"TT:SetOwner(Slot9, "ANCHOR_RIGHT"); TT:SetItemById(871)"#)
        .unwrap();
    let lines = lines_of(&mut s);
    assert_eq!(color(&lines, "Two-Hand"), red, "alt-usable reds the slot");
    assert_eq!(
        right_color(&mut s, "Axe"),
        white,
        "the alternate covers the type"
    );
    // An off-hand weapon with its dagger bit set still reds the SLOT cell without Dual
    // Wield; learning it (an effect-40 spell) clears the red.
    s.set_item_template(
        872,
        ItemTemplateView {
            name: "Left-Hand Blade".into(),
            class: 2,
            subclass: 15,
            inventory_type: 22,
            ..Default::default()
        },
    );
    let mut req3 = req2.clone();
    req3.proficiency.insert(2, 1 << 15);
    s.set_player_req_state(req3.clone());
    s.run(r#"TT:SetOwner(Slot9, "ANCHOR_RIGHT"); TT:SetItemById(872)"#)
        .unwrap();
    let lines = lines_of(&mut s);
    assert_eq!(
        color(&lines, "Off Hand"),
        red,
        "no Dual Wield reds the slot"
    );
    assert_eq!(
        right_color(&mut s, "Dagger"),
        white,
        "the type is proficient"
    );
    req3.can_dual_wield = true;
    s.set_player_req_state(req3);
    s.run(r#"TT:SetOwner(Slot9, "ANCHOR_RIGHT"); TT:SetItemById(872)"#)
        .unwrap();
    let lines = lines_of(&mut s);
    assert_eq!(color(&lines, "Off Hand"), white, "Dual Wield clears it");
    // An item class with NO proficiency entry never reds (the map only ever holds classes
    // the server sent masks for).
    s.set_item_template(
        118,
        ItemTemplateView {
            name: "Tattered Cloth Vest".into(),
            class: 4,    // armor — but req2 carries no class-4 entry
            subclass: 1, // cloth
            inventory_type: 5,
            ..Default::default()
        },
    );
    s.run(r#"TT:SetOwner(Slot9, "ANCHOR_RIGHT"); TT:SetItemById(118)"#)
        .unwrap();
    let lines = lines_of(&mut s);
    assert_eq!(
        color(&lines, "Chest"),
        white,
        "a class with no mask entry stays white"
    );
    // A hidden subclass (displayFlags bit 0 — the Miscellaneous family) prints no type cell.
    s.set_item_template(
        889,
        ItemTemplateView {
            name: "Plain Band".into(),
            class: 4,
            subclass: 0,
            inventory_type: 11,
            hide_subclass: true,
            ..Default::default()
        },
    );
    s.run(
        r#"
        TT:SetOwner(Slot9, "ANCHOR_RIGHT"); TT:SetItemById(889)
        assert(TTTextLeft2:GetText() == "Finger")
        assert(TTTextRight2:GetText() == nil or TTTextRight2:GetText() == "",
               "a ring never prints its Miscellaneous type")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// ITEM_MIN_LEVEL's `> 1` gate (byte-VERIFIED `0x52d2cf`: `cmp esi,0x1 / jle skip`): a level-1
/// requirement — every starter consumable's shape (bread/water carry `RequiredLevel 1`) —
/// prints NO line, exactly like no requirement at all; level 2 is the first that prints.
#[test]
fn required_level_one_is_hidden() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    for (id, req) in [(11, 0u32), (12, 1), (13, 2)] {
        s.set_item_template(
            id,
            ItemTemplateView {
                name: format!("Req{req}"),
                required_level: req,
                ..Default::default()
            },
        );
    }
    s.run(
        r#"
        local a = CreateFrame("Button", "SlotR"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT"); tt:SetItemById(11)
        assert(tt:NumLines() == 1, "req 0: name only, got " .. tt:NumLines())
        tt:SetOwner(a, "ANCHOR_RIGHT"); tt:SetItemById(12)
        assert(tt:NumLines() == 1, "req 1 hides like req 0, got " .. tt:NumLines())
        tt:SetOwner(a, "ANCHOR_RIGHT"); tt:SetItemById(13)
        assert(tt:NumLines() == 2, "req 2 prints, got " .. tt:NumLines())
        assert(TTTextLeft2:GetText() == "Requires Level 2")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// The instance tail (byte-read 2026-07-20; wow-re §1-CREATOR/-OPENABLE cross-checked, their
/// 0f4824e2): a REAL-instance hover (`SetBagItem`) appends the creator line — "Written by %s"
/// (white) when the instance carries letter text, the green-escaped "<Made by %s>" otherwise —
/// then READABLE off the instance text id (its template's PageText is 0 — the
/// director-reported gap). An unresolved creator (name query in flight) emits no line. LOCKED
/// yields to the instance UNLOCKED bit — which is also what un-gates ITEM_OPENABLE: a bag hover
/// on an openable instance shows the green `<Right Click to Open>` (director-observed on a clam;
/// wow-re `right-click-open.md` §1 re-derived the `p6` leg selector that had it suppressed).
/// A **running cooldown** takes SetBagItem's other leg and suppresses the line — the two are
/// structurally exclusive on this binding (decision 0896).
#[test]
fn instance_tail_creator_and_readable() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(
        889,
        ItemTemplateView {
            name: "Plain Letter".into(),
            quality: 1,
            ..Default::default()
        },
    );
    s.set_item_template(
        2589,
        ItemTemplateView {
            name: "Heavy Chest".into(),
            quality: 1,
            flags: 0x4,
            lock_id: 7,
            ..Default::default()
        },
    );
    let slot = |id: u32, readable: bool, creator: Option<&str>, flags: u32| ContainerSlot {
        item_id: id,
        count: 1,
        quality: Some(1),
        readable,
        creator: creator.map(str::to_string),
        flags,
        ..Default::default()
    };
    s.set_item_template(
        7973,
        ItemTemplateView {
            name: "Small Barnacled Clam".into(),
            quality: 1,
            flags: 0x4,
            ..Default::default()
        },
    );
    let mut slots = HashMap::new();
    slots.insert(1, slot(889, true, Some("One"), 0)); // the mail letter
    slots.insert(2, slot(889, true, None, 0)); // creator name still in flight
    slots.insert(3, slot(2589, false, Some("Geoffrey"), 0)); // crafted, still locked
    slots.insert(4, slot(2589, false, None, 0x4)); // unlocked → LOCKED gone, open line on
    slots.insert(5, slot(7973, false, None, 0)); // the clam: lockless, openable outright
                                                 // The same clam with a RUNNING cooldown — `SetBagItem`'s p6=1 leg, which skips the openable
                                                 // tree entirely (the ITEM_COOLDOWN_TIME line takes its place in the reference; that line has
                                                 // no feed here yet, so this slot renders bare).
    slots.insert(
        6,
        ContainerSlot {
            cooldown: Some((1_000, 30_000, true)),
            ..slot(7973, false, None, 0)
        },
    );
    s.set_container(
        0,
        Some(ContainerState {
            name: Some("Backpack".into()),
            num_slots: 6,
            slots,
        }),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "SlotL"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT"); tt:SetBagItem(0, 1)
    "#,
    )
    .unwrap();
    let lines = lines_of(&mut s);
    let texts: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        texts,
        vec!["Plain Letter", "Written by One", "<Right Click to Read>"],
        "the letter: writer line + instance-gated READABLE"
    );
    assert_eq!(lines[1].1, [1.0, 1.0, 1.0, 1.0], "WRITTEN_BY is white");
    assert_eq!(lines[2].1, [0.0, 1.0, 0.0, 1.0], "READABLE is green");
    // Creator unresolved → no writer line, READABLE stays (the re-push repaints later).
    s.run(r#"TT:SetOwner(getglobal("SlotL"), "ANCHOR_RIGHT"); TT:SetBagItem(0, 2)"#)
        .unwrap();
    let texts: Vec<String> = lines_of(&mut s).into_iter().map(|(t, _)| t).collect();
    assert_eq!(texts, vec!["Plain Letter", "<Right Click to Read>"]);
    // Crafted + locked: the green-escaped Made-by; no open line while LockID gates it.
    s.run(r#"TT:SetOwner(getglobal("SlotL"), "ANCHOR_RIGHT"); TT:SetBagItem(0, 3)"#)
        .unwrap();
    let texts: Vec<String> = lines_of(&mut s).into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        texts,
        vec!["Heavy Chest", "Locked", "|cff00ff00<Made by Geoffrey>|r"],
        "CREATED_BY carries the string's own green escape; locked chest hides OPENABLE"
    );
    // The instance UNLOCKED bit retires the LOCKED line AND satisfies the openable lock sub-gate.
    s.run(r#"TT:SetOwner(getglobal("SlotL"), "ANCHOR_RIGHT"); TT:SetBagItem(0, 4)"#)
        .unwrap();
    let lines = lines_of(&mut s);
    let texts: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(texts, vec!["Heavy Chest", "<Right Click to Open>"]);
    assert_eq!(lines[1].1, [0.0, 1.0, 0.0, 1.0], "OPENABLE is green");
    // The director's case: a lockless LOOTABLE template (a clam) is openable outright — name +
    // the green line, nothing between them.
    s.run(r#"TT:SetOwner(getglobal("SlotL"), "ANCHOR_RIGHT"); TT:SetBagItem(0, 5)"#)
        .unwrap();
    let texts: Vec<String> = lines_of(&mut s).into_iter().map(|(t, _)| t).collect();
    assert_eq!(texts, vec!["Small Barnacled Clam", "<Right Click to Open>"]);
    // The same clam mid-cooldown takes SetBagItem's OTHER leg (p6=1) — the openable tree is
    // skipped wholesale, so the green line is gone (the reference prints ITEM_COOLDOWN_TIME in
    // its place; unfed here). `hasCooldown`, the binding's own return, is the same boolean.
    let has_cd: bool = s
        .eval(r#"TT:SetOwner(getglobal("SlotL"), "ANCHOR_RIGHT"); return TT:SetBagItem(0, 6)"#)
        .unwrap();
    assert!(has_cd, "the cooldown leg is what the return value reports");
    let texts: Vec<String> = lines_of(&mut s).into_iter().map(|(t, _)| t).collect();
    assert_eq!(
        texts,
        vec!["Small Barnacled Clam"],
        "a running cooldown suppresses ITEM_OPENABLE — the two lines are exclusive here"
    );
    assert!(s.take_errors().is_empty());
}
