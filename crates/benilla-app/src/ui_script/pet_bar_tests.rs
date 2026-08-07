//! The pet action bar (`PetActionBar.xml`) driven end to end through the REAL shipped XML
//! (decision 0982) — the `multibar_stance_tests` pattern: a self-contained loader, then the
//! whole chain from a pushed slot list to the quads it actually paints.

use benilla_ui::script::{PetActionView, QuadContent, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error.
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
        "loader errors in {file}: {:?}",
        report.errors
    );
}

/// The pet bar's own load prerequisites, in manifest order: UiPanels (`SetDesaturation`, the
/// disabled-bar grey), UIParent (the managed bottom stack its OnShow/OnHide re-fires), Cooldown
/// (`CooldownFrame_SetTimer`), ActionBar (the `BenillaActionBar` anchor target) — then the bar.
fn load_pet_bar(s: &UiScript) {
    for file in [
        "UiPanels.xml",
        "UIParent.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "PetActionBar.xml",
    ] {
        load_xml(s, file);
    }
}

/// GlobalStrings is loaded from the MPQ at runtime, not by the loader — a token slot's `name` is
/// a KEY into it, so the tests declare the two keys they read. (That the real keys exist in the
/// shipped file is a separate fact, asserted in `ui_pet`'s own tests by name.)
fn declare_token_strings(s: &UiScript) {
    s.run(
        "PET_ACTION_ATTACK = 'Attack' \
         PET_MODE_DEFENSIVE = 'Defensive'",
    )
    .unwrap();
}

/// The words the drag moves — `ACT_COMMAND`/Attack and `ACT_ENABLED`/Claw as the server packs
/// them. Carried on the views because the drag is word arithmetic (decision 1010).
const ATTACK_WORD: u32 = 0x0700_0002;
const CLAW_WORD: u32 = 0xC100_0BC2;

/// A hunter's bar as the server actually sends it: Attack (a lit command token), an empty spell
/// slot, Claw (a spell with autocast running), and Defensive (a lit reaction token).
fn hunter_slots() -> Vec<PetActionView> {
    let mut slots = vec![PetActionView::default(); 10];
    slots[0] = PetActionView {
        name: Some("PET_ACTION_ATTACK".into()),
        texture: Some("PET_ATTACK_TEXTURE".into()),
        is_token: true,
        active: true,
        attack_active: true,
        packed: ATTACK_WORD,
        ..Default::default()
    };
    slots[3] = PetActionView {
        name: Some("Claw".into()),
        subtext: Some("Rank 3".into()),
        texture: Some("Interface\\Icons\\Ability_Druid_Rake".into()),
        spell_id: Some(3010),
        autocast_allowed: true,
        autocast_enabled: true,
        packed: CLAW_WORD,
        ..Default::default()
    };
    slots[8] = PetActionView {
        name: Some("PET_MODE_DEFENSIVE".into()),
        texture: Some("PET_DEFENSIVE_TEXTURE".into()),
        is_token: true,
        active: true,
        packed: 0x0600_0001,
        ..Default::default()
    };
    slots
}

/// Count `path` quads **in the pet bar's own row**. The prerequisite `ActionBar.xml` brings 12
/// action buttons wearing the same `UI-Quickslot2` ring, so an unscoped count would be measuring
/// the main bar. Cut by the quad's CENTRE, not an edge: the button rings overhang their buttons by
/// a dozen pixels each way (54 px of ring on a 30 px button), so the two rows' extents actually
/// overlap even though their centres — 21 for the main bar, 70..75 for the pet bar — do not.
fn textures(quads: &[benilla_ui::script::ExtractedQuad], path: &str) -> usize {
    quads
        .iter()
        .filter(|q| q.rect.is_none_or(|r| (r.top + r.bottom) / 2.0 >= 50.0))
        .filter(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
        .count()
}

fn texture_rect(
    quads: &[benilla_ui::script::ExtractedQuad],
    path: &str,
) -> Option<benilla_ui::layout::Rect> {
    quads
        .iter()
        .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
        .and_then(|q| q.rect)
}

/// The whole bar, end to end: hidden with no pet, shown with one, the three slot classes each
/// painting what they should, and hidden again when the pet goes.
#[test]
fn the_shipped_pet_bar_drives_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);

    // No pet: the bar is hidden, and nothing of it reaches the quad pass.
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.resolve();
    assert_eq!(
        textures(&s.extract(), "Interface\\PetActionBar\\UI-PetBar"),
        0,
        "no pet, no shelf"
    );

    // A pet appears. The tick is what runs the bar's OnUpdate, i.e. the sparkle trail — it is an
    // animation, so it exists from the first frame after the repaint, not from the repaint.
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s.tick(0.05);
    s.resolve();
    let quads = s.extract();

    // The shelf art: both UI-PetBar strips draw.
    assert_eq!(
        textures(&quads, "Interface\\PetActionBar\\UI-PetBar"),
        2,
        "the two shelf strips"
    );

    // A TOKEN slot resolved its texture through the global the app named — the `getglobal` fork.
    // Seeing the ICON PATH here (not the literal "PET_ATTACK_TEXTURE") is the whole proof.
    assert_eq!(
        textures(&quads, "Interface\\Icons\\Ability_GhoulFrenzy"),
        1,
        "Attack's icon came from PET_ATTACK_TEXTURE"
    );
    assert_eq!(
        textures(&quads, "Interface\\Icons\\Ability_Defend"),
        1,
        "Defensive's icon came from PET_DEFENSIVE_TEXTURE"
    );
    // A SPELL slot took its path verbatim.
    assert_eq!(textures(&quads, "Interface\\Icons\\Ability_Druid_Rake"), 1);

    // Occupancy: 3 named slots show their filled ring, the 7 unnamed ones are hidden outright
    // (the reference hides an unnamed button rather than showing an empty well).
    assert_eq!(
        textures(&quads, "Interface\\Buttons\\UI-Quickslot2"),
        3,
        "one filled ring per occupied slot"
    );
    assert_eq!(
        textures(&quads, "Interface\\Buttons\\UI-Quickslot"),
        0,
        "an unnamed pet slot hides; it does not draw the empty ring"
    );

    // The checked ring is on both lit slots, and nowhere else.
    assert_eq!(
        textures(&quads, "Interface\\Buttons\\CheckButtonHilight"),
        2,
        "Attack + Defensive are lit; Claw is not"
    );

    // Autocast: the static ring on the one slot that allows it, and the sparkle trail running.
    assert_eq!(
        textures(&quads, "Interface\\Buttons\\UI-AutoCastableOverlay"),
        1,
        "only Claw can autocast"
    );
    assert_eq!(
        textures(&quads, "Interface\\Buttons\\GlowStar"),
        32,
        "4 emitters x 8 trail stars, on the one autocasting slot"
    );

    // Geometry, quoted from the ref: the bar's TOPLEFT is BenillaActionBar's BOTTOMLEFT +(36,97),
    // and BenillaActionBar is the 1024x53 frame at screen BOTTOM ⇒ its BOTTOMLEFT is (0,0), so the
    // frame spans y[54,97]. Button 1 sits at the frame's own BOTTOMLEFT +(36,2) ⇒ x[72,102]
    // y[56,86] — the same 72 the reference lands on (its PETACTIONBAR_XPOS 36 + the button's 36).
    let attack =
        texture_rect(&quads, "Interface\\Icons\\Ability_GhoulFrenzy").expect("Attack icon");
    assert_eq!(
        (attack.left, attack.bottom, attack.right, attack.top),
        (72.0, 56.0, 102.0, 86.0)
    );

    // The pet goes: the bar hides again, sparkles and all.
    s.set_pet_actions(false, true, true, Vec::new());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.tick(0.05);
    s.resolve();
    let gone = s.extract();
    assert_eq!(textures(&gone, "Interface\\PetActionBar\\UI-PetBar"), 0);
    assert_eq!(textures(&gone, "Interface\\Buttons\\GlowStar"), 0);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The click law: left runs the slot, right flips autocast, and a left click on the ATTACK slot
/// while the pet is already attacking calls it off instead of re-ordering it (the reference's
/// `IsPetAttackActive` fork — the one branch that makes the Attack button a toggle).
#[test]
fn clicks_route_through_the_attack_toggle_fork() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();

    // Button 1 (Attack) spans x[72,102] y[56,86] ⇒ centre (87,71). Claw is slot 4: the buttons
    // chain +8 on a 30 px width, so button 4's left = 72 + 3*38 = 186 ⇒ centre (201,71).
    let click = |s: &mut UiScript, x: f32, y: f32, button: &str| {
        s.mouse_button(x, y, button, true);
        s.mouse_button(x, y, button, false);
    };

    // The pet IS attacking, so a left click on Attack calls it off — no action is queued.
    click(&mut s, 87.0, 71.0, "LeftButton");
    assert_eq!(s.take_pet_stop_attacks(), 1);
    assert!(
        s.take_pet_actions().is_empty(),
        "the call-off replaces the press, it does not accompany it"
    );

    // Not attacking any more: the same click orders the attack.
    let mut slots = hunter_slots();
    slots[0].active = false;
    slots[0].attack_active = false;
    s.set_pet_actions(true, true, true, slots);
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();
    click(&mut s, 87.0, 71.0, "LeftButton");
    assert_eq!(s.take_pet_actions(), vec![1]);
    assert_eq!(s.take_pet_stop_attacks(), 0);

    // Right-clicking the spell slot flips its autocast; right-clicking a command token does not
    // (a token has no spell id for the wire verb to name).
    click(&mut s, 201.0, 71.0, "RightButton");
    assert_eq!(s.take_pet_autocast_toggles(), vec![4]);
    click(&mut s, 87.0, 71.0, "RightButton");
    assert!(
        s.take_pet_autocast_toggles().is_empty(),
        "a command token cannot autocast"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **A click takes the ring off; the repaint puts it back** — the contract decision 1021's signal
/// count depends on, pinned here so it cannot quietly rot.
///
/// `PetActionButton_OnClick`'s first line is `this:SetChecked(0)` and it runs for every click, so
/// the ring always comes off. What puts it back is `PET_BAR_UPDATE` — and the repaint that answers
/// it must re-derive `SetChecked` from `isActive` per slot, **not** diff the views, or a press that
/// changed no state would have no way home.
///
/// The reference signals that repaint from the state writes (`0x4bc940`/`0x4bc960`) and **not**
/// from a `TogglePetAutocast` it refuses (`0x4bcbf7` — a token is not autocastable, i.e. every
/// right-click on Follow, Stay or a reaction). So on a right-click the ring genuinely stays off
/// until something else repaints the bar. That is the reference's own quirk, checked there by the
/// director; decision 1027 diverged from it and 1030 put it back.
#[test]
fn a_click_drops_the_ring_and_the_repaint_restores_it() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);

    let checked = |s: &UiScript| {
        s.eval::<bool>("return BenillaPetActionButton1:GetChecked()")
            .unwrap()
    };

    // Button 1 (Attack, lit) spans x[72,102] y[56,86] ⇒ centre (87,71). Both mouse buttons take
    // the ring off — the CheckButton toggles itself, then `SetChecked(0)` lands it at 0 either way.
    for button in ["RightButton", "LeftButton"] {
        s.set_pet_actions(true, true, true, hunter_slots());
        s.fire_event("PET_BAR_UPDATE", vec![]);
        s.resolve();
        assert!(checked(&s), "{button}: the Attack token starts lit");

        s.mouse_button(87.0, 71.0, button, true);
        s.mouse_button(87.0, 71.0, button, false);
        assert!(
            !checked(&s),
            "{button}: the ref's own SetChecked(0), reproduced"
        );

        // A repaint carrying the very SAME views restores it — the property the signal count buys.
        s.fire_event("PET_BAR_UPDATE", vec![]);
        assert!(
            checked(&s),
            "{button}: an unchanged repaint must re-light, not diff to a no-op"
        );
    }
    let _ = s.take_pet_actions();
    let _ = s.take_pet_stop_attacks();
    let _ = s.take_pet_autocast_toggles();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A DISABLED bar still draws — every icon desaturated, nothing hidden. That pair
/// (`PetHasActionBar` true, `GetPetActionsUsable` false) is what a feared or mind-controlled pet
/// looks like, and collapsing it to "hide the bar" would lose the state entirely.
#[test]
fn a_disabled_bar_greys_rather_than_hides() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);

    s.set_pet_actions(true, false, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();
    let quads = s.extract();
    assert_eq!(
        textures(&quads, "Interface\\PetActionBar\\UI-PetBar"),
        2,
        "the bar is still on screen"
    );
    // `SetDesaturation` has no shader path in this engine, so the grey is the reference's own
    // no-shader fallback: vertex colour 0.5 (UiPanels.xml's transcription).
    let tint = quads
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Texture {
                path: Some(p),
                color,
                ..
            } if p == "Interface\\Icons\\Ability_Druid_Rake" => Some(*color),
            _ => None,
        })
        .expect("Claw's icon");
    assert_eq!(
        tint.map(|c| [c[0], c[1], c[2]]),
        Some([0.5, 0.5, 0.5]),
        "a disabled bar greys every icon on it"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The pet bar as it lands with the bottom-left multibar loaded and shown — which is how benilla
/// actually runs (0270: the bottom bars are always on). `shelf` counts the two `UI-PetBar` strips;
/// `attack_top` is the top edge of Attack's icon, the bar's own row in one number.
fn pet_bar_row(with_multibar: bool) -> (usize, f32) {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "UiPanels.xml",
        "UIParent.xml",
        "Cooldown.xml",
        "ActionBar.xml",
    ] {
        load_xml(&s, file);
    }
    if with_multibar {
        load_xml(&s, "MultiBars.xml");
    }
    load_xml(&s, "PetActionBar.xml");
    declare_token_strings(&s);

    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s.tick(0.05);
    s.resolve();
    let quads = s.extract();
    let shelf = quads
        .iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                if p == "Interface\\PetActionBar\\UI-PetBar")
        })
        .count();
    let attack_top = texture_rect(&quads, "Interface\\Icons\\Ability_GhoulFrenzy")
        .expect("Attack's icon draws")
        .top;
    (shelf, attack_top)
}

/// **The row the pet bar shares with the bottom-left multibar** (decision 0988, director-caught:
/// the bar drew straight across a live row of spells, border and all).
///
/// Two rules, both the reference's, both applied by `UIParent_ManageFramePositions`: with that bar
/// up the pet bar rises by the ref's 43 px, and its shelf art — which is a border only while the
/// bar sits directly on the main bar — is hidden rather than drawn over the buttons below.
///
/// The regression this locks is specifically that the manage pass RUNS: the bar's own XML anchor
/// is the base position, so a pass that never fired would leave it exactly where the bug was.
#[test]
fn the_pet_bar_rises_and_sheds_its_shelf_over_the_bottom_left_bar() {
    let (low_shelf, low_top) = pet_bar_row(false);
    let (high_shelf, high_top) = pet_bar_row(true);

    assert_eq!(
        low_shelf, 2,
        "on the main bar, the shelf IS the bar's border"
    );
    assert_eq!(high_shelf, 0, "raised, it would draw across the row below");

    let risen = low_top - high_top;
    assert!(
        (risen.abs() - 43.0).abs() < 0.5,
        "the bar must rise by the ref's 43 px (moved {risen})"
    );
}

/// **The drag, through the shipped XML** (decision 1010). One verb serves both ends — the button's
/// `OnDragStart` and its `OnReceiveDrag` both call `PickupPetAction` — so what makes this a move
/// rather than two pick-ups is the binding's own fork on whether the cursor is already carrying.
///
/// Also locks the grid: while a pet action rides the cursor every slot shows, including the empty
/// ones, so there is somewhere visible to drop it. That is the whole reason `PET_BAR_SHOWGRID`
/// exists, and the reason an unnamed button's `Hide()` is conditional.
#[test]
fn dragging_a_pet_spell_between_slots_moves_it_through_the_shipped_handlers() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();

    // Slot 5 is empty, so its button is hidden — until the grid comes up.
    assert!(!s
        .eval::<bool>("return BenillaPetActionButton5:IsShown()")
        .unwrap());

    s.run("BenillaPetActionButton_OnDragStart(BenillaPetActionButton4)")
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.take_pet_set_actions(),
        vec![vec![(3, CLAW_WORD & 0xFFFF_0000)]],
        "the pickup blanks the slot it came from and tells the server"
    );
    s.tick(0.01);
    s.resolve();
    assert_eq!(
        s.eval::<i64>("return BenillaPetActionBarFrame.showgrid")
            .unwrap(),
        1
    );
    assert!(
        s.eval::<bool>("return BenillaPetActionButton5:IsShown()")
            .unwrap(),
        "the grid reveals the empty slots to drop onto"
    );

    s.run("BenillaPetActionButton_OnReceiveDrag(BenillaPetActionButton5)")
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.take_pet_set_actions(),
        vec![vec![(4, CLAW_WORD)]],
        "and the drop writes the word verbatim into slot 5 (0-based 4)"
    );
    s.tick(0.01);
    assert_eq!(
        s.eval::<i64>("return BenillaPetActionBarFrame.showgrid")
            .unwrap(),
        0,
        "the grid goes down with the payload"
    );
    assert!(s.eval::<bool>("return GetCursorInfo() == nil").unwrap());
}

/// A shift-click is the same pick-up, whichever mouse button carried it — the reference's own fork
/// puts shift above the left/right split, so it never toggles autocast by accident.
#[test]
fn shift_clicking_a_pet_button_picks_it_up_rather_than_casting() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();

    s.run(
        "IsShiftKeyDown = function() return 1 end \
         BenillaPetActionButton_OnClick(BenillaPetActionButton4, 'RightButton')",
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.take_pet_actions().is_empty() && s.take_pet_autocast_toggles().is_empty(),
        "shift outranks both click arms — no cast, no autocast flip"
    );
    assert_eq!(
        s.take_pet_set_actions(),
        vec![vec![(3, CLAW_WORD & 0xFFFF_0000)]]
    );
}
