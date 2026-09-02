//! The social window (decision 0668, `FriendsFrame.xml`): the tab strip, the two list tabs and
//! their toggle pair, the who list's columns, and the verbs each button queues — driven exactly
//! as `ui_social`'s feed drives it (a pushed [`SocialState`] snapshot, then the list event).
//!
//! What these guard that the Rust-side unit tests structurally cannot: the *window* is Lua over a
//! snapshot, so a getter whose returns are in the wrong order, a row template whose FontString is
//! misnamed, or a button wired to the wrong verb are all invisible to `ui_social`'s tests and
//! green in the parse sweep. Each test here fails on exactly one of those.

use benilla_ui::script::{FriendInfo, SocialRequest, SocialState, UiScript, WhoInfo};

use super::test_ui::load_ui_strict as load_xml;

/// The window's own manifest slice, in `load_default_ui` order.
fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Interface\\FrameXML\\UIDropDownMenu.xml");
    // The row right-click menu is the shared UnitPopup "FRIEND" menu, so the window's slice of
    // the manifest includes it (it loads well before FriendsFrame.xml in the real order).
    load_xml(&s, "UnitPopup.xml");
    load_xml(&s, "ScrollTemplates.xml");
    // The guild pane's frames inherit the reference's shared UIPanelButtonTemplate /
    // UIPanelCloseButton / UIPanelScrollFrameTemplate rather than a private copy (decision 1257),
    // so this file's slice needs the shared kit — and the strict `load_xml` above is what would
    // otherwise let those frames load art-less and silent.
    load_xml(&s, "UIPanelTemplates.xml");
    load_xml(&s, "FriendsFrame.xml");
    // The social window's fourth tab lives in its own file, and it is part of THIS window's
    // manifest slice now: `BENILLA_FRIENDS_SUBFRAMES` names "RaidFrame", and both
    // `FriendsFrame_ShowSubFrame` and `FriendsFrame_OnHide` resolve every name in that list
    // through `getglobal` and call `:Hide()` on it. The reference's list names it too and never
    // guards, because there RaidFrame.xml is FrameXML and always loaded — so the guard belongs in
    // the harness's load order, not in shipped Lua defending against a state the client cannot be
    // in (decision 1549).
    load_xml(&s, "RaidFrame.xml");
    s
}

fn friend(name: &str, level: u32, class: &str, area: &str, connected: bool) -> FriendInfo {
    FriendInfo {
        name: name.to_string(),
        level,
        class: class.to_string(),
        area: area.to_string(),
        connected,
        status: String::new(),
    }
}

/// Push a snapshot and fire the list event that follows it, exactly as `feed_social` does.
fn push(s: &mut UiScript, state: SocialState, event: &str) {
    s.set_social(state);
    s.fire_event(event, Vec::new());
}

/// The window loads, opens on the Friends tab, and the GUILD tab comes up disabled — the
/// GUILDLESS case of `InGuildCheck`.
///
/// **This assertion changed meaning with decision 1257 and was kept rather than deleted.** It used
/// to pin a hardcoded `PanelTemplates_DisableTab(this, 3)`, because there was no guild arc and the
/// tab was permanently dead. The tab is live now, and this is the other half of its law: with no
/// guild seated in the engine, `IsInGuild()` answers nil, `FriendsFrame_OnShow` runs
/// `InGuildCheck()`, and the tab greys — which is exactly what the reference does for a character
/// in no guild. The IN-guild half is `guild_tests`. Note that this file seats no guild fixture at
/// all: it is asserting against the REAL `script::guild` bindings' empty state, which is the state
/// every character starts a session in.
#[test]
fn the_window_opens_on_friends_with_the_guild_tab_disabled() {
    let s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    assert!(s.eval::<bool>("return FriendsFrame:IsVisible()").unwrap());
    assert!(s
        .eval::<bool>("return FriendsListFrame:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return FriendsFrameTitleText:GetText()")
            .unwrap(),
        "Friends List"
    );
    assert_eq!(
        s.eval::<i64>("return IsInGuild() and 1 or 0").unwrap(),
        0,
        "no guild is seated, which is what makes the next assertion mean something"
    );
    assert_eq!(
        s.eval::<i64>("return FriendsFrameTab3.isDisabled or 0")
            .unwrap(),
        1,
        "the guild tab is disabled, not absent"
    );
    // …and the tab refuses to be reached by NUMBER either, which is the second of the two locks
    // (`ToggleFriendsFrame`'s own `tab == 3 and not IsInGuild()` early-out).
    s.run("ToggleFriendsFrame(3)").unwrap();
    assert!(
        !s.eval::<bool>("return GuildFrame:IsVisible()").unwrap(),
        "a guildless character cannot open the guild pane at all"
    );
    assert!(s
        .eval::<bool>("return FriendsListFrame:IsVisible()")
        .unwrap());
    // The two tab kinds are different templates and must not be swapped: the strip along the
    // bottom is the big window tab (20px end slices), the Friends/Ignore pair inside tab 1 is the
    // ref's compact TabButtonTemplate (16px). Getting this wrong is invisible to every behaviour
    // assertion — it only shows up as the wrong-looking window.
    assert_eq!(
        s.eval::<f64>("return FriendsFrameTab1Left:GetWidth()")
            .unwrap(),
        20.0,
        "the window tab strip keeps the big tab art"
    );
    assert_eq!(
        s.eval::<f64>("return FriendsFrameToggleTab1Left:GetWidth()")
            .unwrap(),
        16.0,
        "the in-panel toggle pair is the compact tab art"
    );
    // …and toggling the same tab again closes the window (the ref's own second branch).
    s.run("ToggleFriendsFrame(1)").unwrap();
    assert!(!s.eval::<bool>("return FriendsFrame:IsVisible()").unwrap());
}

/// A friend row shows name/zone/status on its top line and "Level N Class" underneath; an
/// OFFLINE friend takes the greyed offline template instead. This is the test that fails if
/// `GetFriendInfo`'s six returns ever come back in the wrong order.
#[test]
fn friend_rows_render_the_online_and_offline_templates() {
    let mut s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    push(
        &mut s,
        SocialState {
            friends: vec![
                FriendInfo {
                    status: "<AFK>".to_string(),
                    ..friend("Onerogue", 60, "Rogue", "Elwynn Forest", true)
                },
                friend("Twomage", 0, "", "", false),
            ],
            selected_friend: 1,
            ..Default::default()
        },
        "FRIENDLIST_UPDATE",
    );

    assert_eq!(
        s.eval::<String>("return FriendsFrameFriendButton1NameLocation:GetText()")
            .unwrap(),
        "Onerogue |cffffffff- Elwynn Forest|r <AFK>"
    );
    assert_eq!(
        s.eval::<String>("return FriendsFrameFriendButton1Info:GetText()")
            .unwrap(),
        "Level 60 Rogue"
    );
    assert_eq!(
        s.eval::<String>("return FriendsFrameFriendButton2NameLocation:GetText()")
            .unwrap(),
        "|cff999999Twomage - Offline|r"
    );
    assert!(
        s.eval::<bool>("return FriendsFrameFriendButton2:IsVisible()")
            .unwrap(),
        "both rows shown"
    );
    assert!(
        !s.eval::<bool>("return FriendsFrameFriendButton3:IsVisible()")
            .unwrap(),
        "rows past the list are hidden"
    );
}

/// The three friend buttons follow the SELECTED friend: an offline one can be removed but not
/// whispered or invited, and an empty list disables all three.
#[test]
fn the_friend_buttons_follow_the_selection() {
    let mut s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();

    push(&mut s, SocialState::default(), "FRIENDLIST_UPDATE");
    for button in ["SendMessage", "GroupInvite", "RemoveFriend"] {
        assert!(
            !s.eval::<bool>(&format!(
                "return FriendsFrame{button}Button:IsEnabled() ~= 0"
            ))
            .unwrap(),
            "{button} is disabled with no friends"
        );
    }

    push(
        &mut s,
        SocialState {
            friends: vec![friend("Twomage", 0, "", "", false)],
            selected_friend: 1,
            ..Default::default()
        },
        "FRIENDLIST_UPDATE",
    );
    assert!(
        s.eval::<bool>("return FriendsFrameRemoveFriendButton:IsEnabled() ~= 0")
            .unwrap(),
        "an offline friend can still be removed"
    );
    assert!(
        !s.eval::<bool>("return FriendsFrameSendMessageButton:IsEnabled() ~= 0")
            .unwrap(),
        "…but not whispered"
    );
}

/// Remove Friend addresses the selected ROW (the wire removes by guid, which the app resolves
/// from this index), and Group Invite addresses the NAME.
#[test]
fn the_friend_buttons_queue_their_verbs() {
    let mut s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    push(
        &mut s,
        SocialState {
            friends: vec![
                friend("Onerogue", 60, "Rogue", "Elwynn Forest", true),
                friend("Twomage", 40, "Mage", "Westfall", true),
            ],
            selected_friend: 2,
            ..Default::default()
        },
        "FRIENDLIST_UPDATE",
    );
    let _ = s.take_social_requests();

    s.run("FriendsFrame_RemoveFriend()").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::RemoveFriendIndex(2)]
    );

    s.run("FriendsFrame_GroupInvite()").unwrap();
    assert!(s.take_party_requests().iter().any(|r| matches!(
        r,
        benilla_ui::script::PartyRequest::InviteName(n) if n == "Twomage"
    )));

    s.run("FriendsFrame_SendMessage()").unwrap();
    assert_eq!(s.take_tell_requests(), vec!["Twomage".to_string()]);
}

/// The Ignore toggle-tab swaps tab 1's list without leaving the tab, and the ignore rows render
/// their names. `ShowIgnorePanel()` (what `/ignore` with no name runs) lands on the same list.
#[test]
fn the_ignore_list_is_the_other_half_of_tab_one() {
    let mut s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    push(
        &mut s,
        SocialState {
            ignores: vec!["Spammer".to_string(), "Ninja".to_string()],
            selected_ignore: 1,
            ..Default::default()
        },
        "IGNORELIST_UPDATE",
    );

    s.run("FriendsFrameToggleTab2:Click()").unwrap();
    assert!(s
        .eval::<bool>("return IgnoreListFrame:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return FriendsListFrame:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return FriendsFrameTitleText:GetText()")
            .unwrap(),
        "Ignore List"
    );
    assert_eq!(
        s.eval::<String>("return FriendsFrameIgnoreButton1Name:GetText()")
            .unwrap(),
        "Spammer"
    );
    assert_eq!(
        s.eval::<String>("return FriendsFrameIgnoreButton2Name:GetText()")
            .unwrap(),
        "Ninja"
    );

    // Remove Player un-ignores the selected name.
    let _ = s.take_social_requests();
    s.run("FriendsFrame_UnIgnore()").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::DelIgnore("Spammer".to_string())]
    );

    // Back to the friends half — through the IGNORE frame's own Friends tab, since the friends
    // frame's copy is hidden with it (the ref ships the pair twice for exactly this reason).
    s.run("IgnoreFrameToggleTab1:Click()").unwrap();
    assert!(s
        .eval::<bool>("return FriendsListFrame:IsVisible()")
        .unwrap());
    s.run("ShowIgnorePanel()").unwrap();
    assert!(s
        .eval::<bool>("return IgnoreListFrame:IsVisible()")
        .unwrap());
}

/// The who list fills its four columns, the variable column follows the dropdown, and the totals
/// line uses the singular/plural template. The class/race swap between the wire and the Lua API
/// makes the column order worth pinning.
#[test]
fn who_rows_fill_their_columns() {
    let mut s = setup();
    s.run("ShowWhoPanel()").unwrap();
    push(
        &mut s,
        SocialState {
            who: vec![WhoInfo {
                name: "Tigole".to_string(),
                guild: "Legacy of Steel".to_string(),
                level: 40,
                race: "Human".to_string(),
                class: "Rogue".to_string(),
                zone: "Westfall".to_string(),
            }],
            who_total: 1,
            ..Default::default()
        },
        "WHO_LIST_UPDATE",
    );

    assert!(s.eval::<bool>("return WhoFrame:IsVisible()").unwrap());
    assert_eq!(
        s.eval::<String>("return WhoFrameButton1Name:GetText()")
            .unwrap(),
        "Tigole"
    );
    assert_eq!(
        s.eval::<String>("return WhoFrameButton1Level:GetText()")
            .unwrap(),
        "40"
    );
    assert_eq!(
        s.eval::<String>("return WhoFrameButton1Class:GetText()")
            .unwrap(),
        "Rogue",
        "class, not race — the API returns race first but the column is class"
    );
    assert_eq!(
        s.eval::<String>("return WhoFrameButton1Variable:GetText()")
            .unwrap(),
        "Westfall",
        "the variable column defaults to Zone (dropdown entry 1)"
    );
    assert_eq!(
        s.eval::<String>("return WhoFrameTotals:GetText()").unwrap(),
        "1 Person Found  ",
        "singular template for one hit"
    );
}

/// The variable column really is variable: picking Guild re-paints it and sorts by that key.
#[test]
fn the_who_dropdown_switches_the_variable_column() {
    let mut s = setup();
    s.run("ShowWhoPanel()").unwrap();
    push(
        &mut s,
        SocialState {
            who: vec![WhoInfo {
                name: "Tigole".to_string(),
                guild: "Legacy of Steel".to_string(),
                level: 40,
                race: "Human".to_string(),
                class: "Rogue".to_string(),
                zone: "Westfall".to_string(),
            }],
            who_total: 1,
            ..Default::default()
        },
        "WHO_LIST_UPDATE",
    );
    let _ = s.take_social_requests();

    s.run("UIDropDownMenu_SetSelectedID(WhoFrameDropDown, 2); WhoList_Update()")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return WhoFrameButton1Variable:GetText()")
            .unwrap(),
        "Legacy of Steel"
    );

    // A column header sorts by its own key.
    s.run("WhoFrameColumnHeader3:Click()").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::SortWho("level".to_string())]
    );
}

/// The who buttons need a selected row, and selecting one enables both. A fresh answer clears
/// the selection — row 3 of the last query is not row 3 of this one.
#[test]
fn the_who_buttons_need_a_selected_row() {
    let mut s = setup();
    s.run("ShowWhoPanel()").unwrap();
    let rows = vec![
        WhoInfo {
            name: "Tigole".to_string(),
            level: 40,
            ..Default::default()
        },
        WhoInfo {
            name: "Furor".to_string(),
            level: 41,
            ..Default::default()
        },
    ];
    push(
        &mut s,
        SocialState {
            who: rows.clone(),
            who_total: 2,
            ..Default::default()
        },
        "WHO_LIST_UPDATE",
    );
    assert!(
        !s.eval::<bool>("return WhoFrameAddFriendButton:IsEnabled() ~= 0")
            .unwrap(),
        "nothing selected yet"
    );

    s.run("WhoFrameButton2:Click()").unwrap();
    assert!(s
        .eval::<bool>("return WhoFrameAddFriendButton:IsEnabled() ~= 0")
        .unwrap());
    let _ = s.take_social_requests();
    s.run("WhoFrameAddFriendButton:Click()").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::AddFriend("Furor".to_string())]
    );

    push(
        &mut s,
        SocialState {
            who: rows,
            who_total: 2,
            ..Default::default()
        },
        "WHO_LIST_UPDATE",
    );
    assert!(
        !s.eval::<bool>("return WhoFrameAddFriendButton:IsEnabled() ~= 0")
            .unwrap(),
        "a fresh answer drops the old selection"
    );
}

/// The Who frame tells the engine where the NEXT answer goes: showing it claims the results,
/// hiding it hands them back to the chat frame. Without this, a `/who` typed with the window
/// closed would print nothing at all.
#[test]
fn showing_the_who_frame_claims_the_next_answer() {
    let mut s = setup();
    s.run("ShowWhoPanel()").unwrap();
    assert!(
        s.take_social_requests()
            .contains(&SocialRequest::SetWhoToUi(true)),
        "showing the frame routes results to it"
    );
    s.run("HideUIPanel(FriendsFrame)").unwrap();
    assert!(
        s.take_social_requests()
            .contains(&SocialRequest::SetWhoToUi(false)),
        "hiding it routes them back to chat"
    );
}

/// The `/who` edit box sends its filter verbatim — the string the app parses into wire fields.
#[test]
fn the_who_edit_box_sends_its_filter() {
    let mut s = setup();
    s.run("ShowWhoPanel()").unwrap();
    let _ = s.take_social_requests();
    s.run("WhoFrameEditBox:SetText(\"z-\\\"Elwynn Forest\\\" 1-10\")")
        .unwrap();
    s.run("WhoFrameEditBox_OnEnterPressed()").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::Who("z-\"Elwynn Forest\" 1-10".to_string())]
    );
}

/// The slash bodies are the reference's: a NAMED `/friends` befriends, a bare one refreshes; a
/// bare `/who` opens the panel and fills the edit box with the default filter.
#[test]
fn the_slash_bodies_match_the_reference() {
    let mut s = setup();
    // The WhoFrame's OnLoad routes results to chat until it is shown; drain that so each
    // assertion below sees only what its own call queued.
    let _ = s.take_social_requests();

    s.run("BenillaSlashFriends(\"Onerogue\")").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::AddFriend("Onerogue".to_string())]
    );
    s.run("BenillaSlashFriends(\"\")").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::RefreshFriends]
    );

    // `/ignore <name>` toggles rather than adds — the ref's AddOrDelIgnore.
    s.run("BenillaSlashIgnore(\"Spammer\")").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::ToggleIgnore("Spammer".to_string())]
    );

    // A bare `/who` opens the Who panel and sends the default filter, which it also shows.
    s.run("BenillaSlashWho(\"\")").unwrap();
    assert!(s.eval::<bool>("return WhoFrame:IsVisible()").unwrap());
    let requests = s.take_social_requests();
    let sent = requests
        .iter()
        .find_map(|r| match r {
            SocialRequest::Who(filter) => Some(filter.clone()),
            _ => None,
        })
        .expect("a bare /who still sends a query");
    assert!(
        sent.starts_with("z-\""),
        "the default filter is zone-scoped: {sent}"
    );
    assert_eq!(
        s.eval::<String>("return WhoFrameEditBox:GetText()")
            .unwrap(),
        sent,
        "and the edit box shows what was sent"
    );
}

/// The Add Friend button with no friendly target opens the name-entry dialog — the first
/// customer of the popup engine's `hasEditBox` capability. Accepting sends what was typed.
#[test]
fn add_friend_without_a_target_opens_the_name_dialog() {
    let mut s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    s.run("FriendsFrameAddFriendButton:Click()").unwrap();

    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "no cooperable target → the dialog"
    );
    assert!(
        s.eval::<bool>("return StaticPopup1EditBox:IsVisible()")
            .unwrap(),
        "and it has an edit box"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "Enter name of friend to add:"
    );

    let _ = s.take_social_requests();
    s.run("StaticPopup1EditBox:SetText(\"Onerogue\")").unwrap();
    s.run("StaticPopup_OnClick(StaticPopup1, 1)").unwrap();
    assert_eq!(
        s.take_social_requests(),
        vec![SocialRequest::AddFriend("Onerogue".to_string())]
    );
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "accepting closes it"
    );
}

/// A re-opened dialog starts EMPTY. Left filled, a reflexive Enter would befriend whoever was
/// typed last time.
#[test]
fn the_name_dialog_reopens_empty() {
    let s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    s.run("StaticPopup_Show(\"ADD_FRIEND\")").unwrap();
    s.run("StaticPopup1EditBox:SetText(\"Onerogue\")").unwrap();
    s.run("StaticPopup_Hide(\"ADD_FRIEND\")").unwrap();
    s.run("StaticPopup_Show(\"ADD_FRIEND\")").unwrap();
    assert_eq!(
        s.eval::<String>("return StaticPopup1EditBox:GetText()")
            .unwrap(),
        ""
    );
}

/// Right-clicking a `/who` row opens the shared FRIEND menu addressed by NAME — the point of
/// that menu, since a who hit has no unit token behind it. Whisper and Invite are the rows that
/// survive UnitPopup's gating, and each one acts on the name.
#[test]
fn right_clicking_a_who_row_opens_the_friend_menu() {
    let mut s = setup();
    s.run("ShowWhoPanel()").unwrap();
    push(
        &mut s,
        SocialState {
            who: vec![WhoInfo {
                name: "Tigole".to_string(),
                level: 40,
                ..Default::default()
            }],
            who_total: 1,
            ..Default::default()
        },
        "WHO_LIST_UPDATE",
    );

    s.run("WhoFrameButton1:Click(\"RightButton\")").unwrap();
    assert!(
        s.eval::<bool>("return DropDownList1:IsVisible()").unwrap(),
        "the menu opens"
    );
    assert_eq!(
        s.eval::<String>("return FriendsDropDown.name").unwrap(),
        "Tigole",
        "addressed by name, not by a unit token"
    );
    // …and a right-click must NOT also select the row (the ref's two branches are exclusive).
    assert!(
        !s.eval::<bool>("return WhoFrameAddFriendButton:IsEnabled() ~= 0")
            .unwrap(),
        "right-click does not select"
    );

    // Whisper acts on the name.
    let whisper = r#"
        for i = 1, UIDROPDOWNMENU_MAXBUTTONS do
            local b = getglobal("DropDownList1Button" .. i)
            if b and b:IsVisible() and b.value == "WHISPER" then b:Click() return 1 end
        end
        return nil"#;
    assert_eq!(
        s.eval::<Option<i64>>(whisper).unwrap(),
        Some(1),
        "the menu has a Whisper row"
    );
    assert_eq!(s.take_tell_requests(), vec!["Tigole".to_string()]);
}

/// An OFFLINE friend's right-click opens nothing — there is no verb to offer them, which is the
/// ref's own `connected` gate on the whole menu.
#[test]
fn right_clicking_an_offline_friend_opens_nothing() {
    let mut s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    push(
        &mut s,
        SocialState {
            friends: vec![friend("Twomage", 0, "", "", false)],
            selected_friend: 1,
            ..Default::default()
        },
        "FRIENDLIST_UPDATE",
    );
    s.run("FriendsFrameFriendButton1:Click(\"RightButton\")")
        .unwrap();
    assert!(
        !s.eval::<bool>("return DropDownList1:IsVisible()").unwrap(),
        "nothing to offer an offline friend"
    );
}

/// Selecting a row queues the selection to the app AND reads back immediately — the same-tick
/// read `FriendsList_Update` does right after `SetSelectedFriend`.
#[test]
fn selecting_a_row_reads_back_in_the_same_tick() {
    let mut s = setup();
    s.run("ToggleFriendsFrame(1)").unwrap();
    push(
        &mut s,
        SocialState {
            friends: vec![
                friend("Onerogue", 60, "Rogue", "Elwynn Forest", true),
                friend("Twomage", 40, "Mage", "Westfall", true),
            ],
            selected_friend: 1,
            ..Default::default()
        },
        "FRIENDLIST_UPDATE",
    );
    let _ = s.take_social_requests();

    s.run("FriendsFrameFriendButton2:Click()").unwrap();
    assert_eq!(
        s.eval::<i64>("return GetSelectedFriend()").unwrap(),
        2,
        "the getter answers this tick, not after the app's next push"
    );
    assert!(s
        .take_social_requests()
        .contains(&SocialRequest::SelectFriend(2)));
}

/// **The window's geometry, diffed against the reference FrameXML itself.**
///
/// Every number in `FriendsFrame.xml` is a transcription of one in the reference's own file, and a
/// wrong one is invisible to every other test here: the frame loads, the clicks work, only the
/// *look* is wrong — so it surfaces as a screenshot from the director, one tab at a time. Two
/// rounds of that is what this replaces. Four of the five it caught on its first run were the
/// FriendsListFrame's values that had leaked into the IgnoreListFrame subtree, which is exactly
/// the failure mode of building a sibling pane by copy-adapting instead of transcribing.
///
/// It scrapes both files for `<AbsDimension>` pairs per named element and compares the elements
/// that exist in both (ours carry a `Benilla` prefix). Deliberately narrow — it does not try to
/// understand the XML, only to notice that a number moved. Known-benign differences are listed
/// explicitly rather than filtered by a pattern, so a NEW difference can never hide inside an
/// exemption. Skips without the extracted reference.
#[test]
fn the_window_geometry_matches_the_reference_framexml() {
    let Some(reference) = super::framexml_diff::reference("FriendsFrame.xml") else {
        eprintln!("skipping: no extracted FrameXML");
        return;
    };

    // Differences that are ours on purpose. Each is a *deliberate* deviation with a reason, not a
    // tolerance: the list is short and every entry names why.
    const EXPECTED: &[&str] = &[
        // The ref omits an all-zero <Offset>; we write none at all. Same anchor, fewer bytes.
        "FriendsFrameFriendButton2",
        "FriendsFrameFriendButton3",
        "FriendsFrameFriendButton4",
        "FriendsFrameFriendButton5",
        "FriendsFrameFriendButton6",
        "FriendsFrameFriendButton7",
        "FriendsFrameFriendButton8",
        "FriendsFrameFriendButton9",
        "FriendsFrameFriendButton10",
        "FriendsFrameIgnoreButton2",
        "FriendsFrameIgnoreButton3",
        "FriendsFrameIgnoreButton4",
        "FriendsFrameIgnoreButton5",
        "FriendsFrameIgnoreButton6",
        "FriendsFrameIgnoreButton7",
        "FriendsFrameIgnoreButton8",
        "FriendsFrameIgnoreButton9",
        "FriendsFrameIgnoreButton10",
        "FriendsFrameIgnoreButton11",
        "FriendsFrameIgnoreButton12",
        "FriendsFrameIgnoreButton13",
        "FriendsFrameIgnoreButton14",
        "FriendsFrameIgnoreButton15",
        "FriendsFrameIgnoreButton16",
        "FriendsFrameIgnoreButton17",
        "FriendsFrameIgnoreButton18",
        "FriendsFrameIgnoreButton19",
        "FriendsFrameIgnoreButton20",
        "WhoFrameButton2",
        "WhoFrameButton3",
        "WhoFrameButton4",
        "WhoFrameButton5",
        "WhoFrameButton6",
        "WhoFrameButton7",
        "WhoFrameButton8",
        "WhoFrameButton9",
        "WhoFrameButton10",
        "WhoFrameButton11",
        "WhoFrameButton12",
        "WhoFrameButton13",
        "WhoFrameButton14",
        "WhoFrameButton15",
        "WhoFrameButton16",
        "WhoFrameButton17",
        "FriendsFrameToggleTab2",
        "IgnoreFrameToggleTab2",
        "FriendsFrameTopLeft",
        "FriendsFrameTopRight",
        "FriendsFrameBottomLeft",
        "FriendsFrameBottomRight",
        "WhoFrameAddFriendButton",
        "WhoFrameWhoButton",
        // The ref writes an all-zero <Offset> on these two row columns; we write none.
        "FriendsFrameButtonTemplate/Info",
        "FriendsFrameWhoButtonTemplate/Variable",
        // Our column header carries the highlight's two anchors inline (the ref's sit in a
        // separate <HighlightTexture> block the scrape attributes to the same element).
        "WhoFrameColumnHeaderTemplate/Right",
        // The three faux-scroll frames: the ref decorates each with two UI-Character-ScrollBar
        // trough textures. benilla's FauxScrollFrameTemplate draws its own complete bar
        // (decisions 0247/0250/0251), so the ref's loose art would double it.
        "FriendsFrameFriendsScrollFrame",
        "FriendsFrameIgnoreScrollFrame",
        "WhoListScrollFrame",
        // …and the guild pane's, the fourth of the same kind (decision 1257).
        "GuildListScrollFrame",
        // The window's own close button predates UIPanelTemplates.xml shipping the reference's
        // `UIPanelCloseButton`, so it states inline the 32x32 that template confers. The guild
        // pane's two close buttons inherit it and therefore need no entry here.
        "FriendsFrameCloseButton",
    ];

    super::framexml_diff::assert_geometry_matches("FriendsFrame.xml", &reference, EXPECTED, 210);
}
