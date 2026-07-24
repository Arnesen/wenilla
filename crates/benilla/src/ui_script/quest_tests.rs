//! The shipped **questgiver window** open/close sound, driven engine-only (no Bevy): the real
//! `assets/ui/QuestFrame.xml` loaded behind `UiPanels.xml` and shown/hidden through the wire events.
//! The window-sound convention's machine check for the quest arc (decision 0090), the sibling of the
//! merchant/gossip/bag/loot sound tests.

use benilla_ui::script::{QuestPanel, QuestState, ScriptValue, SoundRequest, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the panel tests'
/// loader, duplicated so this file is self-contained).
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

/// The questgiver window's open/close kits — the window-sound convention (decision 0090). The real
/// QuestFrame.lua plays igQuestListOpen in QuestFrame_OnShow (l.285) and igQuestListClose in
/// QuestFrame_OnHide (l.294), wired via the frame OnShow/OnHide (QuestFrame.xml l.1012/1015). A
/// questgiver event (QUEST_DETAIL here) → ShowUIPanel → Show() fires OnShow; QUEST_FINISHED →
/// HideUIPanel → Hide() fires OnHide. Nothing queues at load (the frame is authored hidden="true").
/// Same pair as gossip — questgiver and gossip are the same "list" surface to the client.
#[test]
fn questgiver_show_hide_plays_open_and_close_kits() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    // The BenillaMoney_* purse helpers the quest reward/progress panels repaint through live in
    // MerchantFrame.xml (the same documented cross-window dep the bag tests load).
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");

    // Hidden at load: no open sound (never transitions on startup).
    assert!(
        s.take_sounds().is_empty(),
        "no sound at load (never transitions)"
    );

    // A quest-details panel arrives (SMSG_QUESTGIVER_QUEST_DETAILS → QUEST_DETAIL) and shows the
    // window through ShowUIPanel.
    s.set_quest(Some(QuestState {
        panel: QuestPanel::Detail,
        title: "A Threat Within".into(),
        body: "Speak with the captain.".into(),
        objectives: "Report to the captain.".into(),
        ..QuestState::default()
    }));
    s.fire_event("QUEST_DETAIL", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return BenillaQuestFrame:IsVisible()")
            .unwrap(),
        "the questgiver window opened onto the left slot"
    );
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igQuestListOpen".into())],
        "opening the questgiver window plays igQuestListOpen"
    );

    // QUEST_FINISHED hides it through HideUIPanel → OnHide.
    s.fire_event("QUEST_FINISHED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return BenillaQuestFrame:IsVisible()")
            .unwrap(),
        "QUEST_FINISHED hid the questgiver window"
    );
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igQuestListClose".into())],
        "closing the questgiver window plays igQuestListClose"
    );
}

/// The four-child-panel restructure (this pass): each questgiver event shows exactly ONE of the four
/// real sub-panels and hides the other three — mirroring the ref's own `QuestFrame*Panel_OnShow`
/// (each hides its three siblings on show). No sound on a panel SWITCH (only the outer window's
/// OnShow/OnHide play kits, per the ref `QuestFrame_OnShow`/`OnHide` — a panel change while the window
/// stays open is silent).
#[test]
fn panel_events_show_exactly_one_child_panel_and_hide_the_others() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");

    let panels = [
        "BenillaQuestGreetingPanel",
        "BenillaQuestDetailPanel",
        "BenillaQuestProgressPanel",
        "BenillaQuestRewardPanel",
    ];
    let is_shown =
        |s: &UiScript, name: &str| s.eval::<bool>(&format!("return {name}:IsShown()")).unwrap();

    s.set_quest(Some(QuestState {
        panel: QuestPanel::Greeting,
        greeting: "What can I do for you?".into(),
        active_titles: vec!["Report to Goldshire".into()],
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_GREETING",
        vec![ScriptValue::Str("Deputy Willem".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    for name in panels {
        assert_eq!(
            is_shown(&s, name),
            name == "BenillaQuestGreetingPanel",
            "only the greeting panel is shown after QUEST_GREETING"
        );
    }
    assert_eq!(
        s.eval::<String>("return BenillaQuestNpcNameText:GetText()")
            .unwrap(),
        "Deputy Willem"
    );
    // No sound on the panel event itself — the window was already open (well, first open here DOES
    // play the open kit; drain it so the assertion below is about the SWITCH, not this first show).
    s.take_sounds();

    s.set_quest(Some(QuestState {
        panel: QuestPanel::Detail,
        title: "A Threat Within".into(),
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_DETAIL",
        vec![ScriptValue::Str("Deputy Willem".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    for name in panels {
        assert_eq!(
            is_shown(&s, name),
            name == "BenillaQuestDetailPanel",
            "only the detail panel is shown after QUEST_DETAIL"
        );
    }
    assert!(
        s.take_sounds().is_empty(),
        "a panel switch with the window already open plays no kit (only OnShow/OnHide of the window do)"
    );
}

/// The reward/choice grid, ported from the ref's shared `QuestFrameItems_Update` (the SAME idiom the
/// quest log's own reward rows already prove out) — now on REAL per-panel widgets
/// (`BenillaQuestDetailChoice*`/`BenillaQuestDetailReward*`), exercised on the Detail panel with the
/// fixture shape the task's capture drives: 2 choices + 1 fixed reward + money.
#[test]
fn detail_panel_reward_grid_follows_the_refs_two_per_row_layout() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");

    let choice = |name: &str, quality: u32| benilla_ui::script::QuestItemView {
        item_id: 0,
        name: Some(name.into()),
        texture: None,
        count: 1,
        quality,
        usable: true,
    };
    s.set_quest(Some(QuestState {
        panel: QuestPanel::Detail,
        title: "A Threat Within".into(),
        body: "Kill the kobolds infesting the mine.".into(),
        objectives: "Slay 10 Kobold Vermin.".into(),
        choices: vec![choice("Worn Sword", 1), choice("Worn Mace", 1)],
        rewards: vec![choice("Militia Hammer", 1)],
        reward_money: 150,
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_DETAIL",
        vec![ScriptValue::Str("Marshal McBride".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(s
        .eval::<bool>("return BenillaQuestDetailChoice1:IsShown()")
        .unwrap());
    assert!(s
        .eval::<bool>("return BenillaQuestDetailChoice2:IsShown()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BenillaQuestDetailChoice3:IsShown()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaQuestDetailChoice1Name:GetText()")
            .unwrap(),
        "Worn Sword"
    );
    assert_eq!(
        s.eval::<String>("return BenillaQuestDetailItemChooseText:GetText()")
            .unwrap(),
        "You may choose one of these rewards:"
    );
    assert_eq!(
        s.eval::<String>("return BenillaQuestDetailItemReceiveText:GetText()")
            .unwrap(),
        "You will also receive:",
        "choices present -> the ref's \"also\" wording (QuestFrame.lua:461-462)"
    );
    assert!(s
        .eval::<bool>("return BenillaQuestDetailReward1:IsShown()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BenillaQuestDetailReward2:IsShown()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaQuestDetailReward1Name:GetText()")
            .unwrap(),
        "Militia Hammer"
    );
    assert_eq!(
        s.eval::<String>("return BenillaQuestDetailRewardTitleText:GetText()")
            .unwrap(),
        "Rewards"
    );

    // The detail panel's choice rows are informational only (ref: only the REWARD panel's rows
    // select) — clicking one must not raise (no OnClick wired) and must not set an itemChoice.
    s.run("BenillaQuestDetailChoice1:Click()").ok();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The reward panel's choice rows ARE selectable (ref `QuestRewardItem_OnClick`) — clicking one moves
/// the highlight and arms `GetQuestReward`'s 1-based→0-based conversion.
#[test]
fn reward_panel_choice_click_selects_and_completes_with_zero_based_index() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");

    let choice = |name: &str| benilla_ui::script::QuestItemView {
        item_id: 0,
        name: Some(name.into()),
        texture: None,
        count: 1,
        quality: 1,
        usable: true,
    };
    s.set_quest(Some(QuestState {
        panel: QuestPanel::Reward,
        title: "A Threat Within".into(),
        body: "Well done.".into(),
        choices: vec![choice("Worn Sword"), choice("Worn Mace")],
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_COMPLETE",
        vec![ScriptValue::Str("Marshal McBride".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(
        !s.eval::<bool>("return BenillaQuestRewardChoiceHighlight:IsShown()")
            .unwrap(),
        "no row picked yet"
    );
    // Completing without a choice picked is a no-op (ref: QuestChooseRewardError guard).
    s.run("BenillaQuestRewardCompleteButton:Click()").unwrap();
    assert!(
        s.take_quest_actions().is_empty(),
        "must pick a choice first"
    );

    s.run("BenillaQuestRewardChoice2:Click()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return BenillaQuestRewardChoiceHighlight:IsShown()")
            .unwrap(),
        "picking a choice row shows the highlight"
    );

    s.run("BenillaQuestRewardCompleteButton:Click()").unwrap();
    assert_eq!(
        s.take_quest_actions(),
        vec![benilla_ui::script::QuestAction::Reward(1)],
        "row 2 (1-based) -> wire index 1 (0-based)"
    );
}

/// The greeting panel's own Goodbye button (ref `QuestFrameGreetingGoodbyeButton`, a widget the flat
/// v1 layout never carried) — a plain client-side close, no `DeclineQuest()` call (ref l.748-752).
#[test]
fn greeting_goodbye_button_closes_the_window() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");

    s.set_quest(Some(QuestState {
        panel: QuestPanel::Greeting,
        greeting: "Hello".into(),
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_GREETING",
        vec![ScriptValue::Str("Deputy Willem".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return BenillaQuestFrame:IsVisible()")
        .unwrap());

    s.run("BenillaQuestGreetingGoodbyeButton:Click()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return BenillaQuestFrame:IsVisible()")
            .unwrap(),
        "Goodbye closes the questgiver window"
    );
}

/// A rect-level regression guard for the exact bug the capture-loop caught: `BenillaQuestPanelButtonTemplate`
/// is a virtual template declared `hidden="true"` (never itself drawn) — an instance that doesn't
/// explicitly `:Show()` stays invisible even though it isn't declared `hidden="true"` itself. Every
/// panel's action buttons must show, extracted at real on-window rects (never dropped/hidden).
#[test]
fn detail_panel_action_buttons_resolve_to_real_onscreen_rects() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");
    s.set_quest(Some(QuestState {
        panel: QuestPanel::Detail,
        title: "A Threat Within".into(),
        body: "Kill kobolds.".into(),
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_DETAIL",
        vec![ScriptValue::Str("Deputy Willem".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return BenillaQuestAcceptButton:IsShown()")
        .unwrap());
    assert!(s
        .eval::<bool>("return BenillaQuestDeclineButton:IsShown()")
        .unwrap());

    s.resolve();
    let button_art = |s: &mut UiScript| {
        let mut v = Vec::new();
        for q in s.extract() {
            if let benilla_ui::script::QuadContent::Texture { path: Some(p), .. } = &q.content {
                if p.starts_with("Interface\\Buttons\\UI-Panel-Button-") {
                    v.push((
                        p.clone(),
                        q.rect.expect("button quad carries a resolved rect"),
                    ));
                }
            }
        }
        v
    };

    // The instant-text arm (QUEST_FADING_DISABLE pinned "1"): OnShow still disables Accept and
    // zeroes the block for the ref's one-frame window — the reveal edge just starts at 1024.
    let writing = button_art(&mut s);
    assert_eq!(
        writing.len(),
        2,
        "Accept + Decline both extract a real rect (not dropped for being unshown)"
    );
    assert!(
        writing
            .iter()
            .any(|(p, _)| p.ends_with("UI-Panel-Button-Disabled")),
        "Accept draws the Disabled art inside the one-frame arm window"
    );
    for (_, r) in &writing {
        assert!(
            r.top <= 512.0 && r.bottom >= 0.0 && r.left >= 0.0 && r.right <= 384.0,
            "button {r:?} must land inside the 384x512 window, not off-screen/at the origin"
        );
    }

    // First tick: instant text keeps the writing sound — exactly one quill scratch — then the
    // gradient runs off, the objectives/rewards block SNAPS to opaque (no fade) and Accept wakes.
    let scratches = |sounds: Vec<SoundRequest>| {
        sounds
            .iter()
            .filter(|r| **r == SoundRequest::KitName("WriteQuest".into()))
            .count()
    };
    s.tick(0.05);
    assert!(s.errors().is_empty(), "tick errors: {:?}", s.errors());
    assert_eq!(
        scratches(s.take_sounds()),
        1,
        "instant text keeps the writing sound: one scratch on the wake tick"
    );
    assert!(s
        .eval::<bool>("return BenillaQuestAcceptButton:IsEnabled()")
        .unwrap());
    assert_eq!(
        s.eval::<f32>("return BenillaQuestDetailTextAlphaFrame:GetAlpha()")
            .unwrap(),
        1.0,
        "the block snaps straight to opaque — no QUESTINFO_FADE_IN ramp in instant mode"
    );
    s.tick(0.5);
    assert_eq!(
        scratches(s.take_sounds()),
        0,
        "the quill stops after the single instant-mode scratch"
    );
    s.resolve();
    let written = button_art(&mut s);
    assert!(
        written
            .iter()
            .all(|(p, _)| p.ends_with("UI-Panel-Button-Up")),
        "both buttons draw live art once the text has written on: {written:?}"
    );
}

/// The ref write-on survives verbatim behind the flag: with QUEST_FADING_DISABLE = "0" (the
/// ref's own default, our pin is "1"), the description writes on at 40 chars/s scratching the
/// quill each tick, Accept stays dead mid-write, and the objectives/rewards block FADES in over
/// QUESTINFO_FADE_IN rather than snapping.
#[test]
fn write_on_still_fades_when_instant_text_is_off() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");
    s.eval::<()>(r#"QUEST_FADING_DISABLE = "0""#).unwrap();
    s.set_quest(Some(QuestState {
        panel: QuestPanel::Detail,
        title: "A Threat Within".into(),
        body: "Kill kobolds.".into(),
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_DETAIL",
        vec![ScriptValue::Str("Deputy Willem".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Mid-write (reveal edge at char 4 of 13): the quill scratches, Accept stays dead.
    s.tick(0.1);
    assert!(
        s.take_sounds()
            .iter()
            .any(|r| *r == SoundRequest::KitName("WriteQuest".into())),
        "the write-on scratches the WriteQuest quill each tick"
    );
    assert!(!s
        .eval::<bool>("return BenillaQuestAcceptButton:IsEnabled()")
        .unwrap());

    // Half a second more runs the 13-char gradient off (40 chars/s): Accept wakes and the block
    // is mid-FADE — armed at 0 and ramping over QUESTINFO_FADE_IN, not snapped to opaque.
    s.tick(0.5);
    assert!(s.errors().is_empty(), "tick errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return BenillaQuestAcceptButton:IsEnabled()")
        .unwrap());
    let alpha = s
        .eval::<f32>("return BenillaQuestDetailTextAlphaFrame:GetAlpha()")
        .unwrap();
    assert!(
        alpha < 1.0,
        "fading mode ramps the block in (got alpha {alpha})"
    );
    s.tick(1.1);
    assert_eq!(
        s.eval::<f32>("return BenillaQuestDetailTextAlphaFrame:GetAlpha()")
            .unwrap(),
        1.0,
        "the fade completes at opaque"
    );
}

/// The giver window's title bar shows the NPC name — the panel-open events carry it as arg1 and
/// the in-place QUEST_ITEM_UPDATE refresh (how a late ask-once name arrives in live play) must
/// update it too (the 0112-era capture showed a permanently blank bar).
#[test]
fn npc_name_reaches_the_title_bar_on_open_and_on_refresh() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestFrame.xml");

    s.set_quest(Some(QuestState {
        panel: QuestPanel::Detail,
        title: "A Threat Within".into(),
        body: "Kill things.".into(),
        ..QuestState::default()
    }));
    s.fire_event(
        "QUEST_DETAIL",
        vec![ScriptValue::Str("Marshal McBride".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<String>("return getglobal('BenillaQuestNpcNameText'):GetText() or ''")
            .unwrap(),
        "Marshal McBride",
        "the panel-open event's arg1 lands in the title bar"
    );
    // And it RENDERS: the quad extracts with a rect near the window top (the 0112-era capture
    // showed a blank bar — pin the whole path, not just the Lua-visible text).
    s.resolve();
    let name_quad = s
        .extract()
        .into_iter()
        .find(|q| {
            matches!(&q.content,
                benilla_ui::script::QuadContent::Text { text: Some(t), .. } if t == "Marshal McBride")
        })
        .expect("the NPC-name FontString extracts a quad");
    assert!(
        name_quad.rect.is_some(),
        "the NPC-name quad carries a resolved rect (got None — under-constrained)"
    );

    // The late-name path: open with an empty name, the refresh brings it.
    s.run("getglobal('BenillaQuestNpcNameText'):SetText('')")
        .unwrap();
    s.fire_event(
        "QUEST_ITEM_UPDATE",
        vec![ScriptValue::Str("Marshal McBride".into())],
    );
    assert_eq!(
        s.eval::<String>("return getglobal('BenillaQuestNpcNameText'):GetText() or ''")
            .unwrap(),
        "Marshal McBride",
        "the QUEST_ITEM_UPDATE refresh also carries the name"
    );
}
