//! The §22 item-SET block: order (blank, gold header, skill line, member ladder, blank,
//! bonuses), the threshold-ascending sort, the skill gate, member cream/gray, and the
//! ask-once for unseen set ids.

use super::{lines_of, script};
use crate::script::*;

/// The §22 SET block, byte-read: a blank gold line, the gold "name (owned/total)" header, the
/// skill line (white/red) between header and the member ladder (cream when equipped / gray;
/// in-flight names wait), a second blank, then the threshold bonuses SORTED ascending — green
/// only when the skill requirement is met AND owned ≥ threshold — plus the ask-once for an
/// unseen set id.
#[test]
fn item_set_block_counts_and_colors() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    let mut vest = ItemTemplateView {
        name: "Defias Mark".into(),
        quality: 2,
        class: 4,
        subclass: 2,
        inventory_type: 5,
        item_set: 161,
        ..Default::default()
    };
    vest.stats.clear();
    s.set_item_template(6303, vest);
    // Two members equipped (the chest itself + the belt).
    let mut slots: InventorySlots = Default::default();
    slots[4] = Some(InvSlotView {
        item_id: 6303,
        ..Default::default()
    });
    slots[8] = Some(InvSlotView {
        item_id: 6304,
        ..Default::default()
    });
    s.set_inventory_slots(slots);
    let set_view = ItemSetView {
        name: "Defias Leather".into(),
        members: vec![
            (6303, Some("Defias Mark".into())),
            (6304, Some("Defias Belt".into())),
            (6305, Some("Defias Gloves".into())),
            (6306, None), // template still in flight — no line yet
            (6307, Some("Defias Boots".into())),
        ],
        // Stored high-first (the catalog keeps DBC slot order — The Gladiator ships
        // 3,2,5,4): the renderer must sort ascending at print time.
        bonuses: vec![
            (4, "+10 Attack Power.".into()),
            (2, "Increases movement speed slightly.".into()),
        ],
        ..Default::default()
    };
    s.set_item_set(161, set_view.clone());
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot10"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetItemById(6303)
        -- The spacer is the reference's own literal `0x854b2c` — a space and a newline, which is
        -- ONE row carrying one space, not the empty string (an empty AddLine adds no line at all
        -- in the reference: `0x530270` bails before its `inc [esi+0x31c]`).
        assert(TTTextLeft3:GetText() == " \n", "blank before the header, got "
               .. string.format("%q", tostring(TTTextLeft3:GetText())))
        assert(TTTextLeft4:GetText() == "Defias Leather (2/5)", "got " .. tostring(TTTextLeft4:GetText()))
        assert(TTTextLeft5:GetText() == "  Defias Mark")
        assert(TTTextLeft8:GetText() == "  Defias Boots", "in-flight member renders no line")
        assert(TTTextLeft9:GetText() == " \n", "blank before the bonuses, got "
               .. string.format("%q", tostring(TTTextLeft9:GetText())))
        assert(TTTextLeft10:GetText() == "(2) Set: Increases movement speed slightly.",
               "thresholds sort ascending, got " .. tostring(TTTextLeft10:GetText()))
        assert(TTTextLeft11:GetText() == "(4) Set: +10 Attack Power.")
    "#,
    )
    .unwrap();
    let lines = lines_of(&mut s);
    let color = |lines: &[(String, [f32; 4])], needle: &str| {
        lines
            .iter()
            .find(|(t, _)| t == needle)
            .unwrap_or_else(|| panic!("no line {needle:?}"))
            .1
    };
    let gold = [1.0, 210.0 / 255.0, 0.0, 1.0];
    let cream = [1.0, 1.0, 151.0 / 255.0, 1.0];
    let gray = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
    let green = [0.0, 1.0, 0.0, 1.0];
    let red = [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0];
    assert_eq!(
        color(&lines, "Defias Leather (2/5)"),
        gold,
        "set name is gold"
    );
    assert_eq!(
        color(&lines, "  Defias Mark"),
        cream,
        "equipped member is cream"
    );
    assert_eq!(
        color(&lines, "  Defias Gloves"),
        gray,
        "unowned member is gray"
    );
    assert_eq!(
        color(&lines, "(2) Set: Increases movement speed slightly."),
        green,
        "active bonus is green"
    );
    assert_eq!(
        color(&lines, "(4) Set: +10 Attack Power."),
        gray,
        "inactive bonus is gray"
    );
    // The skill gate (`0x5eaae0`): an unmet set-level skill reds its line — seated between
    // the header and the first member — and grays every bonus, met thresholds included.
    let mut gated = set_view;
    gated.required_skill = 165;
    gated.required_skill_rank = 250;
    gated.required_skill_name = Some("Leatherworking".into());
    s.set_item_set(161, gated);
    s.run(
        r#"
        TT:SetOwner(Slot10, "ANCHOR_RIGHT"); TT:SetItemById(6303)
        assert(TTTextLeft5:GetText() == "Requires Leatherworking (250)",
               "the skill line sits between header and members")
        assert(TTTextLeft6:GetText() == "  Defias Mark")
    "#,
    )
    .unwrap();
    let lines = lines_of(&mut s);
    assert_eq!(color(&lines, "Requires Leatherworking (250)"), red);
    assert_eq!(
        color(&lines, "(2) Set: Increases movement speed slightly."),
        gray,
        "an unmet skill grays even a met threshold"
    );
    let mut req = PlayerReqState::default();
    req.skills.insert(165, 300);
    s.set_player_req_state(req);
    s.run(r#"TT:SetOwner(Slot10, "ANCHOR_RIGHT"); TT:SetItemById(6303)"#)
        .unwrap();
    let lines = lines_of(&mut s);
    assert_eq!(
        color(&lines, "Requires Leatherworking (250)"),
        [1.0, 1.0, 1.0, 1.0]
    );
    assert_eq!(
        color(&lines, "(2) Set: Increases movement speed slightly."),
        green,
        "meeting the skill restores the green"
    );
    // An unseen set id records the ask-once.
    s.set_item_template(
        9999,
        ItemTemplateView {
            name: "Gladiator Piece".into(),
            item_set: 1,
            ..Default::default()
        },
    );
    s.run(r#"TT:SetOwner(Slot10, "ANCHOR_RIGHT"); TT:SetItemById(9999)"#)
        .unwrap();
    assert_eq!(s.take_item_set_asks(), vec![1], "the set ask recorded");
    assert!(s.take_errors().is_empty());
}
