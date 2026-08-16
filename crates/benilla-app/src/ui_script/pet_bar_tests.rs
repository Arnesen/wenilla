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
/// (`CooldownFrame_SetTimer`), ActionBar (the `MainMenuBar` anchor target) — then the bar.
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
    // 4 emitters x the DERIVED trail length, on the one autocasting slot — and on that slot only,
    // which is the lazy pool's own pin: the other nine buttons have no sparks at all, where they
    // used to carry a full hidden set each.
    let trail: usize = s.eval("return BENILLA_PET_AUTOCAST_TRAIL").unwrap();
    assert_eq!(
        trail, 23,
        "the size ramp's own spacing (see PetActionBar.xml's header) comes out at 23 samples"
    );
    assert_eq!(
        textures(&quads, "Interface\\Buttons\\GlowStar"),
        4 * trail,
        "4 emitters x the trail, on the one autocasting slot"
    );
    assert!(
        s.eval::<bool>("return PetActionButton1.sparks == nil")
            .unwrap(),
        "a button that never autocasts never pools a spark"
    );

    // Geometry, quoted from the ref: the bar's TOPLEFT is MainMenuBar's BOTTOMLEFT +(36,97),
    // and MainMenuBar is the 1024x53 frame at screen BOTTOM ⇒ its BOTTOMLEFT is (0,0), so the
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
        s.eval::<bool>("return PetActionButton1:GetChecked()")
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
    // Since decision 1327 `SetDesaturation` takes its SHADER arm — the icon carries the greyscale
    // flag to the renderer and keeps its own tint, rather than the reference's no-shader 0.5
    // vertex-colour fallback (which is what this asserted while nothing greyed).
    let grey = quads
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Texture {
                path: Some(p),
                desaturated,
                ..
            } if p == "Interface\\Icons\\Ability_Druid_Rake" => Some(*desaturated),
            _ => None,
        })
        .expect("Claw's icon");
    assert!(grey, "a disabled bar greys every icon on it");
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
    assert!(!s.eval::<bool>("return PetActionButton5:IsShown()").unwrap());

    s.run("this = PetActionButton4; PetActionButton_OnDragStart()")
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
        s.eval::<i64>("return PetActionBarFrame.showgrid").unwrap(),
        1
    );
    assert!(
        s.eval::<bool>("return PetActionButton5:IsShown()").unwrap(),
        "the grid reveals the empty slots to drop onto"
    );

    s.run("this = PetActionButton5; PetActionButton_OnReceiveDrag()")
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.take_pet_set_actions(),
        vec![vec![(4, CLAW_WORD)]],
        "and the drop writes the word verbatim into slot 5 (0-based 4)"
    );
    s.tick(0.01);
    assert_eq!(
        s.eval::<i64>("return PetActionBarFrame.showgrid").unwrap(),
        0,
        "the grid goes down with the payload"
    );
    assert!(s.eval::<bool>("return GetCursorInfo() == nil").unwrap());
}

/// The pet bar honours **Lock ActionBars** too (decision 1136) — the reference's second consumer of
/// the same uvar (`PetActionBarFrame.lua:270/278`), reading the global `ActionBar.xml` declares.
/// Both drag ends refuse while locked; the shift-click pick-up above stays live, exactly as the
/// reference leaves it (l.253-255).
#[test]
fn the_lock_stops_the_pet_bar_drag_but_not_its_shift_click() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();

    s.run(r#"LOCK_ACTIONBAR = "1""#).unwrap();
    s.run("this = PetActionButton4; PetActionButton_OnDragStart()")
        .unwrap();
    assert!(
        s.take_pet_set_actions().is_empty(),
        "a locked bar sends nothing — the slot was never picked up"
    );
    assert!(s.eval::<bool>("return GetCursorInfo() == nil").unwrap());

    // Shift-click is still the way through, and then the drop is refused too while locked.
    s.set_modifiers(true, false, false);
    s.run("this = PetActionButton4; PetActionButton_OnClick(\"LeftButton\")")
        .unwrap();
    s.set_modifiers(false, false, false);
    assert_eq!(
        s.take_pet_set_actions(),
        vec![vec![(3, CLAW_WORD & 0xFFFF_0000)]],
        "shift-click picked it up despite the lock"
    );
    s.run("this = PetActionButton5; PetActionButton_OnReceiveDrag()")
        .unwrap();
    assert!(
        s.take_pet_set_actions().is_empty(),
        "…and a locked slot will not take the drop"
    );

    s.run(r#"LOCK_ACTIONBAR = "0""#).unwrap();
    s.run("this = PetActionButton5; PetActionButton_OnReceiveDrag()")
        .unwrap();
    assert_eq!(
        s.take_pet_set_actions(),
        vec![vec![(4, CLAW_WORD)]],
        "unlocked, the same drop lands"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
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
         this = PetActionButton4 \
         PetActionButton_OnClick('RightButton')",
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

/// **The keybind pair** (decision 1052) — what a bound `BONUSACTIONBUTTONn` runs. The ref's
/// binding body calls `BonusActionButtonDown/Up`, one-liners onto `PetActionButtonDown/Up`
/// (l.218-231), so the whole lane is these two functions: press shows the pushed art, release
/// fires the slot.
///
/// The load-bearing half is what the key path does NOT do: it is a bare `CastPetAction`, with none
/// of `PetActionButton_OnClick`'s forks. Pinned below on an Attack token fed as already-active — a
/// left click on that same view calls the attack off (the test above) while the key re-issues it.
///
/// **What that fixture is and is not.** `attack_active` is pushed straight onto the view here, and
/// on a real *pet* bar it can never be true: `ui_pet`'s `possessing` carve (`0x4bd420`) raises the
/// latch only for a unit you are POSSESSING, so on a hunter's bar the click fork is dead code and
/// key and click behave identically. The state below is a **possess** bar's — Mind Control, Eye of
/// Kilrogg — which is the one case where the two paths genuinely diverge, and therefore the only
/// one worth pinning. Read as "ordinary pets differ from clicks", this test would be lying.
#[test]
fn the_keybind_pair_pushes_and_casts_without_the_clicks_forks() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();

    let state = |s: &UiScript| {
        s.eval::<String>("return PetActionButton1:GetButtonState()")
            .unwrap()
    };

    // Down shows the pushed art and fires nothing (the ref's runOnUp shape).
    s.run("PetActionButtonDown(1)").unwrap();
    assert_eq!(state(&s), "PUSHED");
    assert!(
        s.take_pet_actions().is_empty(),
        "the press is visual only — the ref fires on the release"
    );

    // Up releases the art and casts — slot 1 is Attack and the view says the order is live, so a
    // LEFT CLICK here would call it off instead (a possess bar's case; see the header). The key
    // has no such fork.
    s.run("PetActionButtonUp(1)").unwrap();
    assert_eq!(state(&s), "NORMAL");
    assert_eq!(s.take_pet_actions(), vec![1]);
    assert_eq!(
        s.take_pet_stop_attacks(),
        0,
        "IsPetAttackActive lives in OnClick, which a key press never reaches"
    );

    // The state guard is the whole re-entrancy story: an up with nothing pushed does nothing
    // (a focus-stolen release, a stuck-latch sweep), and a second down does not re-fire.
    s.run("PetActionButtonUp(1)").unwrap();
    assert!(
        s.take_pet_actions().is_empty(),
        "an unmatched release fires nothing"
    );
    s.run("PetActionButtonDown(1) PetActionButtonDown(1)")
        .unwrap();
    assert_eq!(state(&s), "PUSHED");
    s.run("PetActionButtonUp(1)").unwrap();
    assert_eq!(s.take_pet_actions(), vec![1], "one press, one cast");

    // An EMPTY slot is inert through the same path (CastPetAction's own guard): slot 2 of the
    // hunter bar carries no name, and its button is hidden.
    s.run("PetActionButtonDown(2) PetActionButtonUp(2)")
        .unwrap();
    assert!(
        s.take_pet_actions().is_empty(),
        "an unnamed slot queues nothing"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **B228 — the autocast trail's motion, MEASURED off the shipped handler.**
///
/// nazriel reads the shine as "a bit too fast", and the answer turned out to be in the widget's
/// CLOCK, which no constants read could have reached. `CSimpleModel`'s per-frame OnUpdate advances
/// its private `CM2Scene` by `__ftol(elapsed * 1000.0)` — **truncated, no fractional carry**, where
/// the world's driver adds 0.5 first (wow-re `modelframe-animation-clock.md`, §5 cross-checked).
/// So the reference's 2000 ms band actually takes `2000 * T / floor(T)` ms at frame time T, and is
/// **never faster than 2.000 s**. An exact 2.000 s emulation — which is what a measurement of our
/// old wiring confirmed we had — was therefore faster than any real client at any frame rate.
///
/// This drives the real `<OnUpdate>` with real seconds at real frame times and reads the head
/// spark's own SetPoint back. What is pinned: the lap comes out SLOW by exactly the truncation's
/// amount at 60 and 144 fps; the geometry is the 30.72 px square the widget's ortho projection puts
/// on a 30 px button (0.02 units × 1280 × the ref Model's `SetModelScale` 1.2), origin at the
/// button's BOTTOM-LEFT because that is the model's own origin; a corner every quarter lap, LINEAR
/// between them (`interp == 1` at the bone record's translation M2Track, so the half-edge sample
/// sits at the half-time); and the four emitters one quarter-lap apart.
#[test]
fn the_autocast_trail_laps_the_rim_the_reference_widgets_slow_way() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_pet_bar(&s);
    declare_token_strings(&s);
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();

    // Slot 4 is Claw, the one slot in the fixture with autocast RUNNING. `sparks[n]` is the pool
    // `BenillaPetAutocast_Create` built: emitter-major, so emitter e's HEAD (age 0, the bone's own
    // position) is index e·TRAIL + 1.
    let head = |s: &UiScript, emitter: usize| -> (f32, f32) {
        let expr = |axis: usize| {
            format!(
                "local n = {emitter} * BENILLA_PET_AUTOCAST_TRAIL + 1 \
                 local a, b, c, x, y = PetActionButton4.sparks[n]:GetPoint() \
                 return ({axis} == 0) and x or y"
            )
        };
        (
            s.eval::<f32>(&expr(0)).unwrap(),
            s.eval::<f32>(&expr(1)).unwrap(),
        )
    };
    let near = |got: (f32, f32), want: (f32, f32), what: &str| {
        assert!(
            (got.0 - want.0).abs() < 0.01 && (got.1 - want.1).abs() < 0.01,
            "{what}: at {got:?}, expected {want:?}"
        );
    };

    // 0.02 units × 1280 px/unit × 1.2 = 30.72 px across, laid from the button's BOTTOMLEFT — so
    // the walk runs 0..30.72 in both axes, not ±half from the centre.
    const S: f32 = 30.72;
    let (bl, tl, tr, br) = ((0.0, 0.0), (0.0, S), (S, S), (S, 0.0));

    // ── The CLOCK, at a real frame time. 60 fps ⇒ dt = 16.667 ms ⇒ only **16** banked per frame,
    // so the 2000 ms band needs 125 frames = **2.083 s of wall clock**. At 2.000 s (120 frames)
    // the clock has banked 1920 ms = 0.96 of a lap: the head is still on the last edge, running
    // right-to-left toward home and 0.04 × 122.88 = 4.92 px short of it. That shortfall IS the
    // bug — it is how much slower than our old exact-2.000 s clock the reference has always been.
    let frames = |s: &mut UiScript, n: usize, dt: f32| {
        for _ in 0..n {
            s.tick(dt);
        }
    };
    const DT60: f32 = 1.0 / 60.0;
    frames(&mut s, 120, DT60); // 2.000 s of wall clock
    near(
        head(&s, 0),
        (4.9152, 0.0),
        "2.000 s at 60 fps: NOT home yet — the reference is slower than the authored band",
    );
    frames(&mut s, 5, DT60); // 2.083 s
    near(
        head(&s, 0),
        bl,
        "2.083 s at 60 fps: one lap, the reference's own 2000*T/floor(T)",
    );

    // 144 fps truncates harder — 6.944 ms banked as 6 — so the same lap takes 2.315 s there. A
    // frame-rate-independent clock would land both of these at 2.000 s and fail here.
    let mut fast = UiScript::new().unwrap();
    fast.set_screen_size(1024.0, 768.0);
    load_pet_bar(&fast);
    declare_token_strings(&fast);
    fast.set_pet_actions(true, true, true, hunter_slots());
    fast.fire_event("PET_BAR_UPDATE", vec![]);
    fast.resolve();
    frames(&mut fast, 333, 1.0 / 144.0); // 2.313 s of wall clock ⇒ 1998 ms banked
    let (fx, fy) = head(&fast, 0);
    assert!(
        fy == 0.0 && fx > 0.0 && fx < 1.0,
        "2.313 s at 144 fps: still 2 ms short of the lap, at {fx:?},{fy:?}"
    );

    // ── The position LAW, driven at exact MILLISECOND clocks so no tick accumulation can blur it.
    // The mid-edge sample is the `interp == 1` pin: linear means the half-time is the half-edge,
    // and a spline-eased track would sit short of it.
    let at = |s: &UiScript, ms: u32| -> (f32, f32) {
        s.run(&format!(
            "BenillaPetAutocast_Update(PetActionButton4, {ms})"
        ))
        .unwrap();
        head(s, 0)
    };
    near(at(&s, 0), bl, "clock 0");
    near(
        at(&s, 250),
        (0.0, S / 2.0),
        "quarter edge in: the LEFT edge's midpoint",
    );
    near(at(&s, 500), tl, "clock 500 ms");
    near(at(&s, 1000), tr, "clock 1000 ms");
    near(at(&s, 1500), br, "clock 1500 ms");
    near(at(&s, 2000), bl, "clock 2000 ms wraps onto clock 0");

    // ── The trail is CONTINUOUS, which is what deriving the sample count buys (B228). No two
    // neighbours in a trail sit further apart than the smaller of their two stars is wide, so the
    // streak has no seam at any age. The 8-even-samples version failed this from mid-life back —
    // 7.7 px apart with 4.6 px stars — and a row of separated dots is what reads as marching.
    let worst_gap: f32 = s
        .eval(
            "local worst = 0 \
             for k = 1, BENILLA_PET_AUTOCAST_TRAIL - 1 do \
                 local a, b = PetActionButton4.sparks[k], PetActionButton4.sparks[k + 1] \
                 local _, _, _, ax, ay = a:GetPoint() \
                 local _, _, _, bx, by = b:GetPoint() \
                 local d = math.sqrt((ax - bx) * (ax - bx) + (ay - by) * (ay - by)) \
                 local w = math.min(a:GetWidth(), b:GetWidth()) \
                 if d / w > worst then worst = d / w end \
             end \
             return worst",
        )
        .unwrap();
    assert!(
        worst_gap <= 1.0,
        "the widest neighbour gap is {worst_gap:.2} star-widths — the streak has a seam"
    );

    // ── The four emitters are ONE LAP, a quarter apart — the file's four bones at the four
    // corners, chasing each other (m2bones: pivots at (-0.001,0), (0.019,0), (0.019,0.02),
    // (-0.001,0.02), same track, one corner of phase between neighbours).
    s.run("BenillaPetAutocast_Update(PetActionButton4, 0)")
        .unwrap();
    for (e, want, name) in [
        (0, bl, "emitter 0"),
        (1, tl, "emitter 1"),
        (2, tr, "emitter 2"),
        (3, br, "emitter 3"),
    ] {
        near(head(&s, e), want, name);
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The pet bar plus the three files its TOOLTIP needs, in the TOC's own order: Fonts.xml (the
/// colour codes that wrap the binding), GameTooltip.xml (the plate), and UIParent.xml — already a
/// prerequisite for the managed stack — for `GetBindingText`, which renders the key.
fn load_pet_bar_with_tooltip(s: &UiScript) {
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "PetActionBar.xml",
    ] {
        load_xml(s, file);
    }
}

/// A hunter bar hovered through the REAL gesture, with the real binding registry and the real CVar
/// table behind it — `mouse_move` runs the shipped `<OnEnter>` with `this` bound, which is the only
/// way this fork gets exercised the way nazriel exercised it.
///
/// Buttons are 30 px chained +8 from the bar's own origin: button 1 spans x[72,102] y[56,86] ⇒
/// centre (87,71), button 4's left = 72 + 3·38 ⇒ centre (201,71) — the geometry
/// `clicks_route_through_the_attack_toggle_fork` already leans on.
const ATTACK_BUTTON: (f32, f32) = (87.0, 71.0);
const CLAW_BUTTON: (f32, f32) = (201.0, 71.0);

fn hovered_pet_bar() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // The real command set and the real registered defaults, the way the app's own seed installs
    // them — so `GetBindingKey("BONUSACTIONBUTTON1")` answers CTRL-1 (its byte-real default) and
    // `GetCVar("UberTooltips")` answers "1" (its byte-read registrar value) rather than nil.
    s.register_bindings(&crate::bindings::registry_commands());
    s.register_cvars(crate::cvars::REGISTERED.iter().copied());
    load_pet_bar_with_tooltip(&s);
    declare_token_strings(&s);
    s.set_pet_actions(true, true, true, hunter_slots());
    s.fire_event("PET_BAR_UPDATE", vec![]);
    s.resolve();
    s
}

fn tooltip_line1(s: &UiScript) -> String {
    s.eval::<String>("return GameTooltipTextLeft1:GetText() or \"\"")
        .unwrap()
}

/// **B230 — Attack/Follow/Stay name their keybinding; the pet's own spells do not.**
///
/// nazriel: *"On 1.12 when you hover over Attack, Follow, Stay pet action buttons they show the
/// keybinding - which is not the case with Benilla"*. The reference agrees with him and is
/// narrower than "the pet bar": `PetActionButton_OnEnter` (PetActionBarFrame.lua l.285-305) forks
/// on `isToken or UberTooltips == "0"`, and only THAT branch concatenates
/// `NORMAL_FONT_COLOR_CODE.." ("..GetBindingText(GetBindingKey("BONUSACTIONBUTTON"..id), "KEY_")..")"`.
/// The other branch is a bare `SetPetAction` and appends nothing — so a fix that suffixed every
/// slot would be wrong, and the control below is what says ours doesn't.
///
/// The exact string is pinned, colour codes and all, because the ref's concatenation puts the
/// colour BEFORE the space (`Attack|cffffd200 (CTRL-1)|r`, not `Attack |cffffd200(CTRL-1)|r` —
/// which is what MicroMenu.xml's different-shaped version of the same idea produces).
#[test]
fn token_tooltips_name_their_binding_and_pet_spells_do_not() {
    let mut s = hovered_pet_bar();

    s.mouse_move(ATTACK_BUTTON.0, ATTACK_BUTTON.1);
    s.resolve();
    assert_eq!(
        tooltip_line1(&s),
        "Attack|cffffd200 (CTRL-1)|r",
        "a command token carries BONUSACTIONBUTTON1's key in the normal-font colour"
    );
    // Uber on + a token ⇒ still the default corner; only the CVar-off leg owner-anchors.
    assert!(
        s.eval::<bool>("return GameTooltip.default ~= nil").unwrap(),
        "a token's plate takes the default corner while UberTooltips is on"
    );

    // The CONTROL: slot 4 is Claw, a real pet spell, and it goes through the engine channel —
    // its name, and nothing in parentheses.
    s.mouse_move(CLAW_BUTTON.0, CLAW_BUTTON.1);
    s.resolve();
    let claw = tooltip_line1(&s);
    assert_eq!(claw, "Claw", "a pet SPELL renders through SetPetAction");
    assert!(
        !claw.contains('('),
        "…and appends no binding: {claw:?} (the ref's other branch)"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The append is UNCONDITIONAL in that branch, so an unbound row really does render empty
/// parentheses — `GetBindingText` answers `""` for a nil key (UIParent.xml, transcribed from the
/// ref's own l.1819-1821), and nothing guards the `"("`. That is the reference's behaviour, not a
/// slip of ours: MicroButtonTooltipText guards its own suffix and this one does not.
///
/// Stock it is unreachable — BONUSACTIONBUTTON1-10 default to CTRL-1..CTRL-0 — so it takes a
/// deliberate unbind to see, which is exactly what this does.
#[test]
fn an_unbound_token_row_renders_the_references_empty_parentheses() {
    let mut s = hovered_pet_bar();
    s.run(r#"SetBinding("CTRL-1")"#).unwrap();
    assert!(
        s.eval::<Option<String>>(r#"return GetBindingKey("BONUSACTIONBUTTON1")"#)
            .unwrap()
            .is_none(),
        "the row really is unbound now"
    );

    s.mouse_move(ATTACK_BUTTON.0, ATTACK_BUTTON.1);
    s.resolve();
    assert_eq!(
        tooltip_line1(&s),
        "Attack|cffffd200 ()|r",
        "unbound: the parentheses stay, empty — the ref's own unguarded concatenation"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The CVar is the fork's OTHER half, and it is not decoration: with `UberTooltips` off, a real pet
/// SPELL stops going through the engine channel and takes the token branch too — same built text,
/// same binding suffix — and the plate moves from the screen corner to beside the button
/// (`SetOwner(this, "ANCHOR_RIGHT")`). Both are the reference's, in the same five lines.
///
/// This is also the row's reason to exist in `cvars::REGISTERED`: benilla registers `UberTooltips`
/// at all because these Lua sites read it (the honest-tree rule's live-consumer half).
#[test]
fn ubertooltips_off_takes_the_spells_through_the_token_branch_and_moves_the_plate() {
    let mut s = hovered_pet_bar();
    s.set_cvar_engine("UberTooltips", "0");

    s.mouse_move(CLAW_BUTTON.0, CLAW_BUTTON.1);
    s.resolve();
    assert_eq!(
        tooltip_line1(&s),
        "Claw|cffffd200 (CTRL-4)|r",
        "with the CVar off a pet spell is built here too, binding and all"
    );
    assert!(
        s.eval::<bool>("return GameTooltip:IsOwned(PetActionButton4)")
            .unwrap(),
        "…and the plate sits beside the button, not at the screen corner"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
