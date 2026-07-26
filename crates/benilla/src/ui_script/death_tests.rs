//! The death-arc UI (decision 0308, DeathFrame.xml): the DEATH release popup's show/countdown/
//! release flow, the resurrect-offer popup pick, and the spirit-healer XP_LOSS two-step — the Lua
//! wiring between the engine's death events and the StaticPopup engine, driven exactly as
//! `death.rs`'s feed does it (set_death → fire_event).

use benilla_ui::script::{DeathAction, DeathUiState, ScriptValue, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the delete-item
/// tests' loader, duplicated so this file is self-contained).
fn load_xml(s: &UiScript, file: &str) -> usize {
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
    report.frames
}

fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "DeathFrame.xml");
    s
}

/// PLAYER_DEAD with a running release timer: the DEATH popup shows, its OnShow seeds the countdown
/// from GetReleaseTimeRemaining, the engine's per-tick DEATH text renders, Release Spirit queues
/// the Repop intent, and PLAYER_ALIVE (the release landing) hides it.
#[test]
fn death_popup_counts_down_and_release_queues_repop() {
    let mut s = setup();
    s.set_death(DeathUiState {
        release_remaining: Some(300.0),
        ..Default::default()
    });
    s.fire_event("PLAYER_DEAD", vec![]);
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "PLAYER_DEAD shows the DEATH popup"
    );
    s.tick(0.05);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "5 minutes until release",
        "the countdown renders through the engine's DEATH per-tick text"
    );
    // ESC must NOT close it (no hideOnEscape on DEATH — the ref's law).
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "the DEATH popup ignores ESC"
    );
    // Button2 (soulstone) is hidden — HasSoulstone() is nil.
    assert!(
        !s.eval::<bool>("return StaticPopup1Button2:IsShown()")
            .unwrap(),
        "no soulstone ⇒ single-button release dialog"
    );
    s.run("StaticPopup_OnClick(StaticPopup1, 1)").unwrap();
    assert_eq!(s.take_death_actions(), vec![DeathAction::Repop]);
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "Release Spirit hides the dialog"
    );

    // Re-shown (still dead), then the release lands: PLAYER_ALIVE hides it.
    s.fire_event("PLAYER_DEAD", vec![]);
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    s.fire_event("PLAYER_ALIVE", vec![]);
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "PLAYER_ALIVE (the release) hides the DEATH popup"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The no-timer variant: GetReleaseTimeRemaining() == −1 (an instanceable map) swaps in the
/// DEATH_RELEASE_NOTIMER text and never counts down.
#[test]
fn death_popup_no_timer_shows_the_static_text() {
    let mut s = setup();
    s.set_death(DeathUiState {
        release_remaining: None,
        ..Default::default()
    });
    s.fire_event("PLAYER_DEAD", vec![]);
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    s.tick(0.05);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "You have died. Release to the nearest graveyard?",
        "−1 picks the no-timer text, untouched by ticks"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// RESURRECT_REQUEST picks its popup by the offer bits (the ref's three-way), formats the offerer
/// name in, gates Accept behind the recovery delay, and Accept/Decline queue their intents.
#[test]
fn resurrect_request_picks_variant_and_answers() {
    let mut s = setup();
    // No sickness + timer, delay already elapsed: RESURRECT_NO_SICKNESS with an armed Accept.
    s.set_death(DeathUiState {
        resurrect_sickness: false,
        resurrect_has_timer: true,
        recovery_delay: 0.0,
        ..Default::default()
    });
    s.fire_event("RESURRECT_REQUEST", vec![ScriptValue::Str("Pone".into())]);
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "RESURRECT_NO_SICKNESS"
    );
    s.tick(0.05); // the zero StartDelay expires: real text + enabled button
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "Pone wants to resurrect you",
        "the offerer name formats into the no-sickness text"
    );
    assert!(s
        .eval::<bool>("return StaticPopup1Button1:IsEnabled()")
        .unwrap());
    s.run("StaticPopup_OnClick(StaticPopup1, 1)").unwrap();
    assert_eq!(s.take_death_actions(), vec![DeathAction::AcceptResurrect]);

    // Sickness variant declines: the intent queues and (still dead) DEATH re-shows.
    s.set_death(DeathUiState {
        resurrect_sickness: true,
        resurrect_has_timer: true,
        release_remaining: Some(200.0),
        ..Default::default()
    });
    s.set_unit(
        "player",
        Some(benilla_ui::script::UnitState {
            exists: true,
            max_health: 100,
            health: 0,
            dead: true,
            ..Default::default()
        }),
    );
    s.fire_event("RESURRECT_REQUEST", vec![ScriptValue::Str("Pone".into())]);
    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "RESURRECT"
    );
    s.run("StaticPopup_OnClick(StaticPopup1, 2)").unwrap();
    assert_eq!(s.take_death_actions(), vec![DeathAction::DeclineResurrect]);
    // The re-shown DEATH lands on the free SECOND instance (Show runs inside the decline's
    // OnCancel, before the click's own hide frees instance 1) — the exact case that sized
    // STATICPOPUP_NUMDIALOGS at 2.
    assert_eq!(
        s.eval::<String>("return StaticPopup_Visible(\"DEATH\") or \"none\"")
            .unwrap(),
        "StaticPopup2",
        "declining while dead re-shows the DEATH popup (the ref's OnCancel)"
    );
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "the answered RESURRECT dialog itself closed"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The spirit healer's CONFIRM_XP_LOSS two-step: first Accept rewrites to the "Are you sure" text
/// and keeps the dialog, the second queues AcceptXPLoss; walking out of range auto-hides.
#[test]
fn xp_loss_two_step_confirm_then_range_hide() {
    let mut s = setup();
    s.set_death(DeathUiState {
        sickness_duration: Some("8 minutes".into()),
        spirit_healer_in_range: true,
        ..Default::default()
    });
    s.fire_event("CONFIRM_XP_LOSS", vec![]);
    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "XP_LOSS"
    );
    let text = s
        .eval::<String>("return StaticPopup1Text:GetText()")
        .unwrap();
    assert!(
        text.contains("afflicted by 8 minutes of Resurrection Sickness"),
        "the sickness duration formats into CONFIRM_XP_LOSS: {text}"
    );
    // First Accept: the AGAIN text swaps in, the dialog stays, nothing queues. XP_LOSS's verbatim
    // OnAccept reads `this:GetParent()` (the ref's button-handler context), so the click is driven
    // as the XML OnClick does — with `this` bound to the button.
    s.run("this = StaticPopup1Button1 StaticPopup_OnClick(StaticPopup1, 1) this = nil")
        .unwrap();
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    assert!(s.take_death_actions().is_empty());
    let text = s
        .eval::<String>("return StaticPopup1Text:GetText()")
        .unwrap();
    assert!(
        text.starts_with("Remember, if you find your corpse"),
        "the second-ask text: {text}"
    );
    // Second Accept: the activate intent queues and the dialog closes.
    s.run("this = StaticPopup1Button1 StaticPopup_OnClick(StaticPopup1, 1) this = nil")
        .unwrap();
    assert_eq!(s.take_death_actions(), vec![DeathAction::AcceptXpLoss]);
    assert!(!s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());

    // Below the sickness level: the NO_SICKNESS variant. Out of range: the OnUpdate poll hides.
    s.set_death(DeathUiState {
        sickness_duration: None,
        spirit_healer_in_range: true,
        ..Default::default()
    });
    s.fire_event("CONFIRM_XP_LOSS", vec![]);
    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "XP_LOSS_NO_SICKNESS"
    );
    s.set_death(DeathUiState {
        sickness_duration: None,
        spirit_healer_in_range: false,
        ..Default::default()
    });
    s.tick(0.05);
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "leaving the spirit healer's range auto-hides the confirm"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// UnitIsGhost/UnitIsDeadOrGhost — the trio's ghost legs (a ghost has health 1, so UnitIsDead is
/// false for it; decision 0308 §1).
#[test]
fn the_ghost_predicates() {
    let mut s = setup();
    s.set_unit(
        "player",
        Some(benilla_ui::script::UnitState {
            exists: true,
            max_health: 100,
            health: 1,
            dead: false,
            ghost: true,
            ..Default::default()
        }),
    );
    assert!(!s.eval::<bool>("return UnitIsDead(\"player\")").unwrap());
    assert!(s.eval::<bool>("return UnitIsGhost(\"player\")").unwrap());
    assert!(s
        .eval::<bool>("return UnitIsDeadOrGhost(\"player\")")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The corpse-run range events (decision 0308 §5): CORPSE_IN_RANGE shows RECOVER_CORPSE with its
/// StartDelay countdown gating Accept, Accept queues the reclaim intent (and keeps the dialog —
/// the server's descriptor deltas close the loop), CORPSE_OUT_OF_RANGE hides it; the instance
/// variant is the buttonless notice.
#[test]
fn corpse_range_events_drive_recover_corpse() {
    let mut s = setup();
    s.set_death(DeathUiState {
        recovery_delay: 2.0,
        ..Default::default()
    });
    s.fire_event("CORPSE_IN_RANGE", vec![]);
    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "RECOVER_CORPSE"
    );
    assert!(
        !s.eval::<bool>("return StaticPopup1Button1:IsEnabled()")
            .unwrap(),
        "Accept is delay-gated"
    );
    s.tick(0.5);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "2 seconds until resurrection"
    );
    s.tick(1.6);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "Resurrect now?"
    );
    assert!(s
        .eval::<bool>("return StaticPopup1Button1:IsEnabled()")
        .unwrap());
    s.run("StaticPopup_OnClick(StaticPopup1, 1)").unwrap();
    assert_eq!(s.take_death_actions(), vec![DeathAction::RetrieveCorpse]);
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "RetrieveCorpse returns 1 — the dialog stays until the server answers"
    );
    s.fire_event("CORPSE_OUT_OF_RANGE", vec![]);
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "leaving range hides the recover dialog"
    );

    // The dungeon-corpse notice: buttonless (the ref entry has no button1), whileDead.
    s.fire_event("CORPSE_IN_INSTANCE", vec![]);
    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "RECOVER_CORPSE_INSTANCE"
    );
    assert!(!s
        .eval::<bool>("return StaticPopup1Button1:IsShown()")
        .unwrap());
    s.fire_event("CORPSE_OUT_OF_RANGE", vec![]);
    assert!(!s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// GetCorpseMapPosition: the feed's corpse UV surfaces (0,0) = hidden when absent — the reference
/// WorldMapFrame.lua:443-452 law the map's update block branches on.
#[test]
fn corpse_map_position_binding() {
    let mut s = setup();
    let (x, y) = s
        .eval::<(f64, f64)>("return GetCorpseMapPosition()")
        .unwrap();
    assert_eq!((x, y), (0.0, 0.0), "no corpse ⇒ the (0,0) hide sentinel");
    s.set_world_map_feed(None, None, 0.0, Some((0.25, 0.75)));
    let (x, y) = s
        .eval::<(f64, f64)>("return GetCorpseMapPosition()")
        .unwrap();
    assert!((x - 0.25).abs() < 1e-6 && (y - 0.75).abs() < 1e-6);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
