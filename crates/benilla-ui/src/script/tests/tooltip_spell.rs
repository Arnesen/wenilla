//! The engine spell/aura tooltip channel (decision 0274 P2, law per 0276): the verified line
//! shapes — name|rank, ONE cost|range line, ONE casttime|cooldown line, the passive omission,
//! the aura variant (white description + the SetPlayerBuff-only remaining line), and SetAction's
//! pure delegation.

use super::common::script;
use crate::script::*;

fn fireball() -> SpellTooltipView {
    SpellTooltipView {
        name: "Fireball".into(),
        rank: Some("Rank 1".into()),
        cost: Some("30 Mana".into()),
        range: Some("30 yd range".into()),
        cast_time: Some("1.5 sec cast".into()),
        cooldown: None,
        description: "Hurls a fiery ball that causes 14 to 22 Fire damage.".into(),
        ..Default::default()
    }
}

/// The spellbook hover: name|rank gray, cost|range one line, casttime line, gold description.
#[test]
fn spellbook_hover_renders_the_verified_shape() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(133, fireball());
    s.set_spellbook(SpellBookState {
        tabs: Vec::new(),
        slots: vec![SpellSlotView {
            spell_id: 133,
            name: "Fireball".into(),
            ..Default::default()
        }],
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "SB1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetSpell(1, "spell")
        assert(tt:IsShown(), "SetSpell shows")
        assert(tt:NumLines() == 4, "name + cost|range + casttime + description, got " .. tt:NumLines())
        assert(TTTextLeft1:GetText() == "Fireball")
        -- Byte-verified: SetSpell passes showRank=0 — the spellbook hover shows NO rank column
        -- (only SetAction shows it).
        assert(not TTTextRight1:IsShown(), "no rank column from SetSpell")
        assert(TTTextLeft2:GetText() == "30 Mana")
        assert(TTTextRight2:GetText() == "30 yd range", "cost|range is ONE line")
        assert(TTTextLeft3:GetText() == "1.5 sec cast")
        assert(TTTextLeft4:GetText() ~= nil)
    "#,
    )
    .unwrap();
    // The description wears the byte-verified gold.
    s.resolve();
    let quads = s.extract();
    let gold = quads.iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t.starts_with("Hurls") && (c[1] - 210.0/255.0).abs() < 1e-6)
    });
    assert!(gold, "spell description is gold");
    assert!(s.take_errors().is_empty());
}

/// **`bookType` decides which book the hover reads** — the fork `SetSpell 0x532d10` makes at
/// `0x532e1c`/`0x532e2a` (`[4*i + 0xb6f098]` for the pet, `[4*i + 0xb700f0]` otherwise).
///
/// The defect (1050): the binding took the argument and dropped it, so a pet-book hover indexed the
/// PLAYER's slot list — the imp's first spell showed the player's first spell, "Attack" and its
/// crit line, on the director's screen. Both books are populated here with a *different* spell at
/// the same slot id, which is the only arrangement that can tell the two apart.
#[test]
fn a_pet_book_hover_reads_the_pets_book_not_the_players() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(133, fireball());
    s.set_spell_tooltip(
        3110,
        SpellTooltipView {
            name: "Firebolt".into(),
            description: "Hurls a fiery ball.".into(),
            ..Default::default()
        },
    );
    s.set_spellbook(SpellBookState {
        tabs: Vec::new(),
        slots: vec![SpellSlotView {
            spell_id: 133,
            name: "Fireball".into(),
            ..Default::default()
        }],
    });
    s.set_pet_book(PetBookState {
        token: Some("DEMON".into()),
        slots: vec![SpellSlotView {
            spell_id: 3110,
            name: "Firebolt".into(),
            ..Default::default()
        }],
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "SB1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetSpell(1, "pet")
        assert(TTTextLeft1:GetText() == "Firebolt", "pet slot 1, got " .. TTTextLeft1:GetText())
        -- The same slot id in the other book is the OTHER spell.
        tt:SetSpell(1, "spell")
        assert(TTTextLeft1:GetText() == "Fireball", "player slot 1, got " .. TTTextLeft1:GetText())
        -- The compare is case-insensitive against "pet" ALONE (0x532e13's SStrCmpI), so every
        -- other string is the player's book — including a mis-cased one being the pet's.
        tt:SetSpell(1, "PeT")
        assert(TTTextLeft1:GetText() == "Firebolt", "case-insensitive pet")
        tt:SetSpell(1, "book")
        assert(TTTextLeft1:GetText() == "Fireball", "any other string is the player's book")
        -- A non-string third argument is not "the player's book": the reference bails outright
        -- (0x532dc0's isstring test), leaving whatever was shown.
        tt:SetSpell(1, 7)
        assert(TTTextLeft1:GetText() == "Fireball", "a non-string arg shows nothing new")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// The tracking icon's hover (SetTrackingSpell): a GOLD name over the white (aura-variant)
/// description — the shape the director's reference A/B pinned (2026-07-20), distinct from
/// SetPlayerBuff's white name. No cost/casttime lines, no duration-remaining line.
#[test]
fn tracking_hover_renders_gold_name_over_white_description() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_tracking(Some(TrackingState {
        spell_id: 2580,
        name: Some("Find Minerals".into()),
        icon: Some("Interface\\Icons\\Trade_Mining".into()),
        cancelable: true,
    }));
    s.set_spell_tooltip(
        2580,
        SpellTooltipView {
            name: "Find Minerals".into(),
            cost: Some("a cost cell that must NOT render".into()),
            cast_time: Some("nor this".into()),
            description: "Finding Minerals.".into(),
            aura_description: "Finding Minerals.".into(),
            ..Default::default()
        },
    );
    s.run(
        r#"
        local f = CreateFrame("Frame", "TRK"); f:SetPoint("CENTER", 0, 0); f:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(f, "ANCHOR_BOTTOMLEFT")
        tt:SetTrackingSpell()
        assert(tt:IsShown(), "SetTrackingSpell shows")
        assert(tt:NumLines() == 2, "name + description only, got " .. tt:NumLines())
        assert(TTTextLeft1:GetText() == "Find Minerals")
        assert(TTTextLeft2:GetText() == "Finding Minerals.")
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();
    let gold_name = quads.iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t == "Find Minerals" && (c[1] - 210.0/255.0).abs() < 1e-6)
    });
    let white_desc = quads.iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t == "Finding Minerals." && (c[1] - 1.0).abs() < 1e-6 && (c[2] - 1.0).abs() < 1e-6)
    });
    assert!(gold_name, "the tracking name line is GOLD (director A/B)");
    assert!(white_desc, "the description is the aura-variant white");
    assert!(s.take_errors().is_empty());
}

/// A passive spell omits the casttime|cooldown line whole — never a "Passive" text line.
#[test]
fn passive_omits_the_casttime_line() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(
        1461,
        SpellTooltipView {
            name: "Frost Warding".into(),
            rank: None,
            cost: None,
            range: None,
            cast_time: None, // passive
            cooldown: Some("never shows without a casttime".into()),
            description: "Increases your frost ward.".into(),
            ..Default::default()
        },
    );
    s.set_spellbook(SpellBookState {
        tabs: Vec::new(),
        slots: vec![SpellSlotView {
            spell_id: 1461,
            name: "Frost Warding".into(),
            ..Default::default()
        }],
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "SB2"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetSpell(1, "spell")
        assert(tt:NumLines() == 2, "name + description only, got " .. tt:NumLines())
        local all = ""
        for i = 1, tt:NumLines() do all = all .. (getglobal("TTTextLeft" .. i):GetText() or "") end
        assert(not string.find(all, "Passive"), "no Passive text line")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// The buff hover: the aura variant — WHITE description + the duration-remaining line (only
/// this entry point), computed off the aura's GetTime expiry.
#[test]
fn player_buff_hover_is_the_aura_variant() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(
        1459,
        SpellTooltipView {
            name: "Arcane Intellect".into(),
            rank: Some("Rank 1".into()),
            cost: Some("60 Mana".into()), // ignored by the aura variant
            range: Some("30 yd range".into()), // ignored
            cast_time: Some("Instant".into()), // ignored
            cooldown: None,
            description: "Increases Intellect by 2.".into(),
            aura_description: "Intellect increased by 2.".into(),
            ..Default::default()
        },
    );
    s.tick(10.0); // GetTime = 10
    s.set_auras(
        "player",
        Some(vec![AuraState {
            spell_id: 1459,
            name: Some("Arcane Intellect".into()),
            duration: 1800.0,
            expiration_time: 100.0, // 90 s left at now = 10
            helpful: true,
            ..Default::default()
        }]),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "BF1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        -- 0, not 1: SetPlayerBuff takes a 1.12 CACHE POSITION (see `script::aura`'s header).
        tt:SetPlayerBuff(0)
        assert(tt:NumLines() == 3, "name + description + remaining, got " .. tt:NumLines())
        assert(TTTextLeft2:GetText() == "Intellect increased by 2.", "the AURA description column")
        assert(TTTextLeft3:GetText() == "2 minutes remaining", "got " .. TTTextLeft3:GetText())
    "#,
    )
    .unwrap();
    // The aura description is WHITE (the byte-verified aura/spell difference).
    s.resolve();
    let quads = s.extract();
    let white = quads.iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t.starts_with("Intellect increased") && *c == [1.0, 1.0, 1.0, 1.0])
    });
    assert!(white, "aura description is white, not gold");
    // …and the NAME is GOLD: the aura builder `0x52f880` writes it through the gold wrapper
    // `0x530380`, where the spell builder uses the plain `0x530270`. Pinned against the
    // reference's own buff hover (2026-07-25 report B53), which the white name did not match.
    let gold_name = quads.iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t == "Arcane Intellect" && (c[1] - 210.0 / 255.0).abs() < 1e-6)
    });
    assert!(gold_name, "aura name is gold, not white");
    assert!(s.take_errors().is_empty());
}

/// Law §3-BUFF's right column: the buff hover names the DISPEL CLASS where the spell hover would
/// put a gray "Rank N" — "Magic" on Ice Armor, the half of B53 the gold name didn't cover — and it
/// is gold, sharing the aura name's wrapper. A `rank` on the same view must NOT displace it.
#[test]
fn player_buff_hover_names_the_dispel_class_in_gold() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(
        168,
        SpellTooltipView {
            name: "Ice Armor".into(),
            rank: Some("Rank 1".into()), // the spell variant's column — never the aura's
            dispel_type: Some("Magic".into()),
            aura_description: "Encases the caster in a layer of ice.".into(),
            ..Default::default()
        },
    );
    s.set_auras(
        "player",
        Some(vec![AuraState {
            spell_id: 168,
            name: Some("Ice Armor".into()),
            helpful: true,
            ..Default::default()
        }]),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "BF1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetPlayerBuff(0)
        assert(TTTextRight1:GetText() == "Magic",
            "the dispel class, got " .. tostring(TTTextRight1:GetText()))
    "#,
    )
    .unwrap();
    s.resolve();
    let gold_dispel = s.extract().iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t == "Magic" && (c[1] - 210.0 / 255.0).abs() < 1e-6)
    });
    assert!(
        gold_dispel,
        "the dispel column is gold, not the rank's gray"
    );
    assert!(s.take_errors().is_empty());
}

/// Law §3.6's equipped-item line and §3.8's reagents line — the two lines the 2026-07-25 reports
/// found missing (B54, B56). Order: cast|cooldown → requires-item → requires-form → reagents →
/// description, each requirement red while unmet.
#[test]
fn requirement_and_reagent_lines_render_in_law_order() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(
        5019,
        SpellTooltipView {
            name: "Shoot".into(),
            range: Some("30 yd range".into()),
            cast_time: Some("Instant".into()),
            requires_item: Some("Requires Wands".into()),
            item_met: false,
            reagents: Some("Reagents: |cffff2020Light Feather|r".into()),
            description: "Attack with an equipped wand.".into(),
            ..Default::default()
        },
    );
    s.set_spellbook(SpellBookState {
        tabs: Vec::new(),
        slots: vec![SpellSlotView {
            spell_id: 5019,
            name: "Shoot".into(),
            ..Default::default()
        }],
    });
    s.run(
        r#"
        local a = CreateFrame("Button", "SB2"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetSpell(1, "spell")
        assert(tt:NumLines() == 6, "name + range + cast + requires + reagents + desc, got " .. tt:NumLines())
        assert(TTTextLeft1:GetText() == "Shoot")
        assert(TTTextLeft2:GetText() == "30 yd range", "cost absent: range moves left")
        assert(TTTextLeft3:GetText() == "Instant")
        assert(TTTextLeft4:GetText() == "Requires Wands", "law §3.6 sits above the reagents")
        assert(TTTextLeft5:GetText():find("Light Feather") ~= nil, "law §3.8")
        assert(TTTextLeft6:GetText() == "Attack with an equipped wand.", "description last")
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();
    let red = quads.iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t == "Requires Wands" && *c == [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0])
    });
    assert!(red, "an unmet equipped-item requirement is red");
    // The reagent's inline `|cffff2020` escape reaches the region VERBATIM: the engine paints
    // the line's BASE colour white and the app's text layer resolves the escape into colour runs
    // (`benilla::ui_text::markup`, covered by its own `color_runs_survive_the_wrap`). So what this
    // layer owns is that the escape is neither stripped nor pre-flattened.
    let verbatim = quads.iter().any(|q| {
        matches!(&q.content, QuadContent::Text { text: Some(t), color: Some(c), .. }
            if t == "Reagents: |cffff2020Light Feather|r" && *c == [1.0, 1.0, 1.0, 1.0])
    });
    assert!(
        verbatim,
        "the reagents line reaches the region escape-intact, base white"
    );
    assert!(s.take_errors().is_empty());
}

/// The target-frame aura hover: SetUnitBuff/SetUnitDebuff render the aura variant for the token's
/// sign-filtered list — WITHOUT the duration-remaining line (byte-verified: only SetPlayerBuff
/// appends it, and no other unit carries a duration on the 1.12 wire anyway).
#[test]
fn unit_buff_and_debuff_hover_render_the_aura_variant_without_remaining() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(
        589,
        SpellTooltipView {
            name: "Shadow Word: Pain".into(),
            aura_description: "Suffering 12 Shadow damage every 3 sec.".into(),
            ..Default::default()
        },
    );
    s.set_spell_tooltip(
        1126,
        SpellTooltipView {
            name: "Mark of the Wild".into(),
            aura_description: "Armor increased by 25.".into(),
            ..Default::default()
        },
    );
    s.set_auras(
        "target",
        Some(vec![
            AuraState {
                spell_id: 1126,
                name: Some("Mark of the Wild".into()),
                helpful: true,
                ..Default::default()
            },
            AuraState {
                spell_id: 589,
                name: Some("Shadow Word: Pain".into()),
                helpful: false,
                ..Default::default()
            },
        ]),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "TF1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_BOTTOMRIGHT", 15, -25)
        tt:SetUnitDebuff("target", 1)
        assert(tt:NumLines() == 2, "name + description, NO remaining line, got " .. tt:NumLines())
        assert(TTTextLeft1:GetText() == "Shadow Word: Pain", "the sign filter picks the debuff")
        assert(TTTextLeft2:GetText() == "Suffering 12 Shadow damage every 3 sec.")
        tt:SetOwner(a, "ANCHOR_BOTTOMRIGHT", 15, -25)
        tt:SetUnitBuff("target", 1)
        assert(TTTextLeft1:GetText() == "Mark of the Wild", "the sign filter picks the buff")
        tt:SetOwner(a, "ANCHOR_BOTTOMRIGHT", 15, -25)
        tt:SetUnitDebuff("target", 2)
        assert(tt:NumLines() == 0 and not tt:IsShown(), "past the list: no empty plate")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// **`SetPlayerBuff` reads a 1.12 cache position, not an Era ordinal** — the index space the whole
/// `GetPlayerBuff*` family shares (`script::aura`'s header; pinned by `ref-BuffFrame.lua:105`,
/// which passes `GetPlayerBuff`'s return straight in).
///
/// Every assertion here is a case the previous 1-based, sign-filtered reading got wrong, and got
/// wrong *silently* — the plate still rendered, just for the wrong aura:
///
/// - position 1 is the SECOND aura, not the first again;
/// - a debuff is reachable at all (the old sign filter defaulted to helpful, so no position ever
///   resolved to one);
/// - `-1` clears instead of showing the first buff. `BigWigs/Raids/Naxxramas/Loatheb.lua:260-271`
///   feeds an unchecked `GetPlayerBuff(i, "HARMFUL")` in and breaks its scan when line 1 goes nil,
///   so "shows something" there is an addon that never stops scanning;
/// - a surplus filter argument is ignored (`CT_BuffMod/CT_BuffFrame.lua:151` passes one).
#[test]
fn player_buff_hover_indexes_the_cache_position_not_a_filtered_ordinal() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    for (id, name, desc) in [
        (1126, "Mark of the Wild", "Armor increased by 25."),
        (2457, "Battle Stance", "A balanced combat stance."),
        (589, "Shadow Word: Pain", "Suffering 12 Shadow damage."),
    ] {
        s.set_spell_tooltip(
            id,
            SpellTooltipView {
                name: name.into(),
                aura_description: desc.into(),
                ..Default::default()
            },
        );
    }
    // The player's cache: two buffs then a debuff, ONE list, insertion-ordered (decision 0257).
    s.set_auras(
        "player",
        Some(vec![
            AuraState {
                spell_id: 1126,
                name: Some("Mark of the Wild".into()),
                helpful: true,
                ..Default::default()
            },
            AuraState {
                spell_id: 2457,
                name: Some("Battle Stance".into()),
                helpful: true,
                ..Default::default()
            },
            AuraState {
                spell_id: 589,
                name: Some("Shadow Word: Pain".into()),
                helpful: false,
                ..Default::default()
            },
        ]),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "BF1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        local function at(i, extra)
            tt:SetOwner(a, "ANCHOR_RIGHT")
            tt:SetPlayerBuff(i, extra)
            return TTTextLeft1 and TTTextLeft1:GetText()
        end
        assert(at(0) == "Mark of the Wild", "position 0 is the first aura, got " .. tostring(at(0)))
        assert(at(1) == "Battle Stance", "position 1 is the SECOND, got " .. tostring(at(1)))
        assert(at(2) == "Shadow Word: Pain", "the debuff shares the index space, got " .. tostring(at(2)))
        -- The handle really is GetPlayerBuff's return, round-tripped.
        assert(at(GetPlayerBuff(0, "HARMFUL")) == "Shadow Word: Pain", "round-trip through GetPlayerBuff")
        -- A surplus filter argument is ignored, as the reference's binding ignores it.
        assert(at(1, "HELPFUL|HARMFUL") == "Battle Stance", "surplus argument must be ignored")
        -- Misses CLEAR (Loatheb's scan terminates on a nil first line) rather than leaving a plate.
        tt:SetOwner(a, "ANCHOR_RIGHT"); tt:SetPlayerBuff(3)
        assert(tt:NumLines() == 0 and not tt:IsShown(), "past the cache: no plate")
        tt:SetOwner(a, "ANCHOR_RIGHT"); tt:SetPlayerBuff(-1)
        assert(tt:NumLines() == 0 and not tt:IsShown(), "-1 must clear, not show the first buff")
        tt:SetOwner(a, "ANCHOR_RIGHT"); tt:SetPlayerBuff(GetPlayerBuff(9, "HARMFUL"))
        assert(tt:NumLines() == 0 and not tt:IsShown(), "an unguarded -1 from GetPlayerBuff clears")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty(), "{:?}", s.take_errors());
}

/// SetAction delegates by payload kind: a SPELL slot renders the spell view; an empty slot shows
/// nothing; a miss records the ask.
#[test]
fn action_hover_delegates_by_kind() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_spell_tooltip(133, fireball());
    s.set_action(
        24,
        Some(ActionSlot {
            kind: 0x00, // SPELL
            action: 133,
            ..Default::default()
        }),
    );
    s.set_action(
        25,
        Some(ActionSlot {
            kind: 0x00,
            action: 5143, // not in the store — the ask channel fires
            ..Default::default()
        }),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "AB1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetAction(24)
        assert(TTTextLeft1:GetText() == "Fireball", "spell slot renders the spell view")
        assert(TTTextRight1:GetText() == "Rank 1", "SetAction shows the rank column")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetAction(25)
        assert(tt:NumLines() == 0 and not tt:IsShown(), "a total miss shows no empty plate")
    "#,
    )
    .unwrap();
    assert_eq!(s.take_spell_tooltip_asks(), vec![5143], "the miss asked");
    assert!(s.take_errors().is_empty());
}

/// The tooltip's left-column texts in order.
fn left_lines(s: &mut UiScript) -> Vec<String> {
    let n: i64 = s.eval("return TT:NumLines()").unwrap();
    (1..=n)
        .map(|i| {
            s.eval::<String>(&format!(
                "return getglobal('TTTextLeft{i}'):GetText() or ''"
            ))
            .unwrap()
        })
        .collect()
}

/// One trainer service fixture with an explicit tooltip subject.
fn trainer_service(spell_id: u32, name: &str, tooltip: TrainerTooltip) -> TrainerService {
    TrainerService {
        spell_id,
        tooltip,
        name: Some(name.into()),
        texture: Some("Interface\\Icons\\Temp".into()),
        category: TrainerServiceCategory::Available,
        group_key: 26,
        group_name: "Arms".into(),
        ..Default::default()
    }
}

/// `SetTrainerService` is a SELECTOR: it renders no line of its own and routes to whichever shared
/// builder the app-side law picked. All three arms are pinned here — the item arm, the spell arm
/// (with `altCaster` suppressing the reagents block), and a header row, which is a no-op.
///
/// The index is a VISIBLE row index, so row 1 is the "Arms" header and the services start at 2 —
/// the same interleave `SetTradeSkillItem` has, and the same way to get it wrong.
#[test]
fn set_trainer_service_selects_the_builder_and_never_renders_its_own_line() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(
        2847,
        ItemTemplateView {
            name: "Copper Shortsword".into(),
            quality: 1,
            class: 2,
            subclass: 7,
            ..Default::default()
        },
    );
    s.set_spell_tooltip(
        200,
        SpellTooltipView {
            name: "Heroic Strike".into(),
            cost: Some("15 Rage".into()),
            reagents: Some("Reagents: Linen Cloth".into()),
            description: "A strong attack.".into(),
            ..Default::default()
        },
    );
    s.set_trainer(Some(TrainerState {
        services: vec![
            trainer_service(2756, "Copper Shortsword", TrainerTooltip::Item(2847)),
            trainer_service(
                100,
                "Heroic Strike",
                TrainerTooltip::Spell {
                    spell_id: 200,
                    alt_caster: false,
                },
            ),
            trainer_service(
                101,
                "Train Growl",
                TrainerTooltip::Spell {
                    spell_id: 200,
                    alt_caster: true,
                },
            ),
        ],
        ..Default::default()
    }));

    s.run(
        r#"
        local a = CreateFrame("Button", "AB1"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        CreateFrame("GameTooltip", "TT")

        -- Row 1 is the "Arms" header: a no-op, not a shifted hit onto a neighbouring service.
        TT:SetOwner(a, "ANCHOR_RIGHT")
        TT:SetTrainerService(1)
        assert(TT:NumLines() == 0, "a header row renders nothing")

        -- Row 2: the recipe -> the ITEM builder, full item tooltip (not the spell's name).
        TT:SetOwner(a, "ANCHOR_RIGHT")
        TT:SetTrainerService(2)
        assert(TTTextLeft1:GetText() == "Copper Shortsword", "the item arm renders the item")
        assert(TT:NumLines() > 1, "the ITEM builder, not a one-line stub")
    "#,
    )
    .unwrap();

    // Row 3: the class-trainer service -> the SPELL builder on the TAUGHT spell, reagents shown.
    s.run(
        r#"
        TT:SetOwner(AB1, "ANCHOR_RIGHT")
        TT:SetTrainerService(3)
    "#,
    )
    .unwrap();
    let texts = left_lines(&mut s);
    assert_eq!(
        texts[0], "Heroic Strike",
        "the spell arm hops to the taught"
    );
    assert!(
        texts.iter().any(|t| t == "Reagents: Linen Cloth"),
        "altCaster clear -> the reagents block renders: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "A strong attack."),
        "the gold description is part of the shared builder's law: {texts:?}"
    );

    // Row 4: the same view, altCaster set -> the reagents block is gone, everything else stays.
    s.run(
        r#"
        TT:SetOwner(AB1, "ANCHOR_RIGHT")
        TT:SetTrainerService(4)
    "#,
    )
    .unwrap();
    let texts = left_lines(&mut s);
    assert!(
        !texts.iter().any(|t| t == "Reagents: Linen Cloth"),
        "altCaster gates the reagents block: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "A strong attack."),
        "and gates ONLY that block: {texts:?}"
    );
    assert!(s.take_errors().is_empty());
}

/// `SetCraftSpell` is `SetTrainerService`'s structural twin: a selector, not the two-line
/// name/description stub it used to be. Both arms are pinned, and the assertion that matters most
/// is the negative one — a rod recipe's hover shows the ROD's item tooltip while the same row's
/// ICON is the spell's (Law D, decision 1107). Icon and tooltip disagreeing on one row is the
/// verified shape, not a bug to reconcile.
#[test]
fn set_craft_spell_selects_the_builder_like_the_trainer_hover_does() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(
        6218,
        ItemTemplateView {
            name: "Runed Copper Rod".into(),
            quality: 1,
            ..Default::default()
        },
    );
    s.set_spell_tooltip(
        7420,
        SpellTooltipView {
            name: "Enchant Bracer - Minor Health".into(),
            cast_time: Some("Instant cast".into()),
            description: "Permanently enchant bracers.".into(),
            ..Default::default()
        },
    );
    let recipe = |spell_id: u32, name: &str, tooltip: CraftTooltip| CraftRecipe {
        spell_id,
        tooltip,
        name: name.into(),
        sub_name: String::new(),
        difficulty: TradeSkillDifficulty::Optimal,
        num_available: 1,
        icon: Some("Interface\\Icons\\Spell_Holy_Heal".into()),
        description: Some("the OLD two-line stub's second line".into()),
        needs_item_target: false,
        reagents: vec![],
        tools: vec![],
        spell_level: 0,
    };
    s.set_craft(Some(CraftState {
        name: "Enchanting".into(),
        rank: 100,
        max_rank: 150,
        craft_type: 3,
        recipes: vec![
            recipe(
                7420,
                "Enchant Bracer - Minor Health",
                CraftTooltip::Spell(7420),
            ),
            recipe(7421, "Runed Copper Rod", CraftTooltip::Item(6218)),
        ],
    }));

    s.run(
        r#"
        local a = CreateFrame("Button", "AB2"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        CreateFrame("GameTooltip", "TT")
        TT:SetOwner(a, "ANCHOR_RIGHT")
        TT:SetCraftSpell(1)
    "#,
    )
    .unwrap();
    let texts = left_lines(&mut s);
    assert_eq!(texts[0], "Enchant Bracer - Minor Health");
    assert!(
        texts.iter().any(|t| t == "Instant cast"),
        "the SHARED spell builder's law, not a two-line stub: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|t| t == "the OLD two-line stub's second line"),
        "the row's own `description` field is no longer the tooltip's source: {texts:?}"
    );

    s.run(
        r#"
        TT:SetOwner(AB2, "ANCHOR_RIGHT")
        TT:SetCraftSpell(2)
    "#,
    )
    .unwrap();
    let texts = left_lines(&mut s);
    assert_eq!(
        texts[0], "Runed Copper Rod",
        "a CREATE_ITEM recipe hovers the ITEM, though its icon is the spell's: {texts:?}"
    );
    assert!(s.take_errors().is_empty());
}
