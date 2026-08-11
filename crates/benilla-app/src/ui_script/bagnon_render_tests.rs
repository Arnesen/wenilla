//! **"Only the frame header and the gold text. No bag slots at all."** — the director's Bagnon
//! report, reproduced end to end and then closed.
//!
//! ## Why this file exists
//!
//! The addon survey scored Bagnon `loaded, missing=0, session=ok, probe=ok` — every column green —
//! while the window on the director's screen was empty. Every one of those columns asks whether
//! something *raised*; none of them asks whether anything was **drawn**. Two independent faults
//! were hiding in that gap, and only one of them raises at all:
//!
//! | | what it does | does it raise? |
//! |---|---|---|
//! | `UnitName("player")` is nil at addon file scope | Bagnon reads the live bags as another character's offline cache → **0 slots**, window shown | **no** |
//! | the `SetItemButton*` family is missing | the per-slot update dies on the first slot → loop aborts, window never shown | yes, once |
//!
//! The director saw the first (their `Bagnon_Core` loaded at startup, so `currentPlayer` was
//! captured nil). Fix only that and you get the second. So the tests below pin **both**, and the
//! headline is a quad count, not an error count: [`bagnon_draws_a_slot_for_every_bag_slot`] is
//! green only when 16 item-slot quads actually reach the render list.
//!
//! Nothing from the corpus is committed, and every test here skips cleanly on a machine without
//! it — the `ui_chat::ace_gate_tests` rule.

use std::path::{Path, PathBuf};

use benilla_ui::script::{AddOnInfo, ContainerSlot, ContainerState, QuadContent, UiScript};
use benilla_ui::toc::Toc;

/// Where the vanilla addon corpus might be — `$BENILLA_ADDON_CORPUS`, else a sibling checkout
/// resolved from this crate's manifest (a pool worktree's cwd is not stable across tool calls).
fn corpus_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(over) = std::env::var_os("BENILLA_ADDON_CORPUS") {
        out.push(PathBuf::from(over));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for up in [2usize, 3, 4] {
        if let Some(root) = manifest.ancestors().nth(up) {
            out.push(root.join("wow-addons-vanilla"));
        }
    }
    out
}

/// The corpus root, or `None` — **a skip, never a failure**: it is third-party content that is
/// deliberately not in this repo.
fn corpus() -> Option<PathBuf> {
    corpus_candidates().into_iter().find(|c| c.is_dir())
}

macro_rules! corpus_or_skip {
    () => {
        match corpus() {
            Some(root) => root,
            None => {
                eprintln!(
                    "skipping: no vanilla addon corpus — looked in {:?} (set $BENILLA_ADDON_CORPUS)",
                    corpus_candidates()
                );
                return;
            }
        }
    };
}

fn read_toc(root: &Path, name: &str) -> Toc {
    let path = root.join(name).join(format!("{name}.toc"));
    Toc::parse(&benilla_ui::source::decode(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    ))
}

/// One addon's `.toc` files through the same two arms the real loader uses — `.lua` as a chunk,
/// anything else as FrameXML — with the AddOns root as the provider's path space (decision 1186).
fn load_addon_files(script: &UiScript, root: &Path, name: &str) -> Vec<String> {
    let toc = read_toc(root, name);
    let provider = |req: &str| -> Option<Vec<u8>> { std::fs::read(root.join(req)).ok() };
    let mut errors = Vec::new();
    for file in &toc.files {
        let path = benilla_ui::loader::join_ref(name, file);
        let Ok(bytes) = std::fs::read(root.join(&path)) else {
            errors.push(format!("{file}: not found"));
            continue;
        };
        if file.to_ascii_lowercase().ends_with(".lua") {
            // Named as the client names it — the corpus parses its own tracebacks.
            if let Err(e) =
                script.run_chunk_named(&bytes, &benilla_ui::script::addon_chunk_name(name, file))
            {
                errors.push(format!("{file}: {e}"));
            }
            continue;
        }
        match benilla_ui::framexml::parse(&benilla_ui::source::decode(&bytes)) {
            Ok(doc) => {
                let report = benilla_ui::loader::load_in(script, &doc, &path, &provider);
                errors.extend(report.errors.into_iter().map(|e| format!("{file}: {e}")));
            }
            Err(e) => errors.push(format!("{file}: {e}")),
        }
    }
    errors
}

/// The director's exact installed set. Bagnon and Bagnon_Options are `## LoadOnDemand: 1`, so the
/// startup walk skips them and `LoadAddOn` brings them in — which is why the registry below
/// carries a real root.
const INSTALLED: &[&str] = &[
    "!OmniCC",
    "Bagnon",
    "Bagnon_Core",
    "Bagnon_Forever",
    "Bagnon_Options",
    "MapCoords",
];

/// Whether the VM gets its `"player"` before the addons load — i.e. whether
/// [`super::seat_from_roster`] ran.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seat {
    /// What the app does now: the roster row goes in ahead of `load_ingame_ui`.
    BeforeAddons,
    /// What it did before the fix: nothing writes `"player"` until `ui_unit::feed_units` sees the
    /// self descriptor, seconds later.
    AfterAddons,
}

/// The roster row the director's session would carry, and the same struct the app reads —
/// so the seat under test is built by [`super::seat_from_roster`] itself, not by a copy of it.
fn roster() -> crate::char_select::Roster {
    let row = benilla_protocol::Character {
        guid: 7,
        name: "Harness".into(),
        race: 1,  // Human → Alliance
        class: 1, // Warrior
        gender: 0,
        level: 60,
        skin: 0,
        face: 0,
        hair_style: 0,
        hair_color: 0,
        facial_hair: 0,
        zone: 0,
        map: 0,
        position: benilla_protocol::wire::Vector3d {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        flags: 0,
        equipment: [benilla_protocol::CharEnumItem::default(); 19],
    };
    crate::char_select::Roster::with_pending_pick(vec![row], 7)
}

/// A VM shaped like the director's session: our whole interface, their installed addons walked in
/// dependency order the way `addons::load_third_party` walks them, and a real backpack.
fn seat(root: &Path, seat: Seat) -> UiScript {
    let mut s = UiScript::new().expect("VM");
    s.set_screen_size(1024.0, 768.0);

    let mut registry: Vec<AddOnInfo> = INSTALLED
        .iter()
        .map(|n| super::addons::info_from_toc(n, &read_toc(root, n)))
        .collect();
    s.register_addons(registry.clone(), Some(root.to_path_buf()), None, None);
    s.set_realm_name("Harness");
    let player = super::seat_from_roster(&roster()).expect("a pending pick seats a player");
    if seat == Seat::BeforeAddons {
        s.set_unit("player", Some(player.clone()));
    }
    let failures = super::load_default_ui(&s);
    assert!(
        failures.is_empty(),
        "our own FrameXML failed to load: {failures:#?}"
    );

    // A real backpack — 16 slots, one occupied. `GetContainerNumSlots(0)` answering 16 is what
    // Bagnon's LIVE path reads; the whole bug is that it took a different path and never asked.
    //
    // The slot carries a real `item_id` and `link`, not just an icon: the render half only ever
    // needed the texture, but the INTERACTION half rides `GameTooltip:SetBagItem` (which requires
    // a resolved instance) and `PickupContainerItem` (which needs something to pick up), so a
    // texture-only slot would make every hover and every click a silent no-op — a fixture that
    // hides the very thing the test is for.
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Bag_08".into()),
            count: 1,
            item_id: 4496,
            quality: Some(1),
            link: Some("|cffffffff|Hitem:4496:0:0:0|h[Small Brown Pouch]|h|r".into()),
            ..Default::default()
        },
    );
    s.set_container(
        0,
        Some(ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots,
        }),
    );

    // The startup walk: dependencies first, LoadOnDemand skipped, one `ADDON_LOADED` each.
    let mut done: Vec<String> = Vec::new();
    for name in INSTALLED {
        walk(&mut s, root, name, &mut done, &mut registry);
    }
    // Re-registered with the startup set marked loaded, so `LoadAddOn` below meets satisfied
    // dependencies exactly as it does in a real session.
    s.register_addons(registry, Some(root.to_path_buf()), None, None);
    for event in ["VARIABLES_LOADED", "PLAYER_LOGIN"] {
        s.fire_event(event, Vec::new());
    }
    // The self descriptor's arrival: `ui_unit::feed_units` pushes the player and fires
    // PLAYER_ENTERING_WORLD off it. In the `AfterAddons` arm this is the FIRST time the VM has
    // ever had a `"player"` — which is the state this file exists to reproduce.
    if seat == Seat::AfterAddons {
        s.set_unit("player", Some(player));
    }
    s.fire_event("PLAYER_ENTERING_WORLD", Vec::new());
    for _ in 0..10 {
        s.tick(0.1);
    }
    s
}

fn walk(s: &mut UiScript, root: &Path, name: &str, done: &mut Vec<String>, reg: &mut [AddOnInfo]) {
    if done.iter().any(|d| d.eq_ignore_ascii_case(name)) {
        return;
    }
    let toc = read_toc(root, name);
    if toc.load_on_demand() {
        return; // the reference's `0x51f600` loads only records whose LoadOnDemand byte is 0
    }
    done.push(name.to_string());
    for dep in toc.dependencies() {
        if INSTALLED.iter().any(|n| n.eq_ignore_ascii_case(dep)) {
            walk(s, root, dep, done, reg);
        }
    }
    load_addon_files(s, root, name);
    if let Some(i) = reg.iter().position(|a| a.name.eq_ignore_ascii_case(name)) {
        reg[i].loaded = true;
    }
    s.fire_event(
        "ADDON_LOADED",
        vec![benilla_ui::script::ScriptValue::Str(name.to_string())],
    );
}

/// Every item-slot quad **Bagnon's own buttons** drew — the `UI-Quickslot2` ring, which is the one
/// texture an empty slot still paints (an occupied slot adds its icon on top).
///
/// **Attributed, not counted.** Our own `BenillaBagFrame` paints sixteen of exactly this texture,
/// so a bare total reads 16 whether Bagnon drew nothing or our window did — the "an addon replaces,
/// it does not add" trap. Every quad is charged to the nearest named frame above it
/// ([`UiScript::target_owner_name`]) and only `BagnonItem*` counts.
fn bagnon_slot_quads(s: &mut UiScript) -> usize {
    s.resolve();
    s.extract()
        .iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                     if p.contains("UI-Quickslot2"))
        })
        .filter(|q| {
            s.target_owner_name(q.target)
                .is_some_and(|n| n.starts_with("BagnonItem"))
        })
        .count()
}

/// Bagnon's window, open, with the fixture's backpack in it — the state every interaction test
/// below starts from. `ToggleBackpack` is Bagnon's own replacement, and its first call
/// demand-loads the `Bagnon` addon (the frame does not exist before that).
fn open_bagnon(root: &Path) -> UiScript {
    let mut s = seat(root, Seat::BeforeAddons);
    s.run("ToggleBackpack()").expect("ToggleBackpack");
    for _ in 0..3 {
        s.tick(0.1);
    }
    assert!(
        s.eval::<bool>("return Bagnon and Bagnon:IsVisible() or false")
            .unwrap(),
        "the window must be on screen before anything is clicked"
    );
    s.resolve();
    s
}

/// The centre of a named frame, in the y-up UI space [`UiScript::mouse_move`] /
/// [`UiScript::mouse_button`] take — the same space `resolve`/`extract` use, so a frame's own
/// `GetLeft`/`GetBottom` read straight back as a cursor position.
fn centre_of(s: &mut UiScript, name: &str) -> (f32, f32) {
    s.resolve();
    let r: Vec<f32> = s
        .eval(&format!(
            "local f = getglobal(\"{name}\") \
             return {{ f:GetLeft(), f:GetBottom(), f:GetWidth(), f:GetHeight() }}"
        ))
        .unwrap_or_else(|e| panic!("{name}: no resolved rect: {e}"));
    assert_eq!(r.len(), 4, "{name}: unresolved rect {r:?}");
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

/// The name of the Bagnon item button showing backpack slot `slot`. Bagnon lays its buttons out in
/// its own order and `SetID`s each to the game slot, so the button holding the fixture's one item
/// is found by asking, never by assuming an index.
fn bagnon_button_for_slot(s: &UiScript, bag: i64, slot: u32) -> String {
    s.eval::<String>(&format!(
        "for i = 1, 36 do \
           local b = getglobal(\"BagnonItem\"..i) \
           if b and b:GetID() == {slot} and b:GetParent():GetID() == {bag} then \
             return \"BagnonItem\"..i \
           end \
         end \
         return \"\""
    ))
    .inspect(|n| assert!(!n.is_empty(), "no BagnonItem* is bag {bag} slot {slot}"))
    .expect("the item-button scan")
}

/// Press and release the named button over `name`, the way a player's mouse does — through
/// `mouse_button`, so the whole engine path runs (hit test, `RegisterForClicks` gate, the frame's
/// own `OnClick`), never `s.run("Handler(...)")`.
fn click(s: &mut UiScript, name: &str, button: &str) {
    let (x, y) = centre_of(s, name);
    s.mouse_move(x, y);
    s.mouse_button(x, y, button, true);
    s.mouse_button(x, y, button, false);
}

/// **The headline, and the director's own symptom.** Press B with Bagnon installed and every one
/// of the backpack's 16 slots must reach the render list.
///
/// This is the test that was red before the two fixes and is green after — and the *number* is the
/// point, not the absence of an error: before, this VM raised nothing at all on the path the
/// director was on.
#[test]
fn bagnon_draws_a_slot_for_every_bag_slot() {
    let root = corpus_or_skip!();
    let mut s = seat(&root, Seat::BeforeAddons);

    // Bagnon's own entry point: it replaces `ToggleBackpack`, and its replacement demand-loads the
    // `Bagnon` addon on the first call (the frame does not exist before that).
    s.run("ToggleBackpack()").expect("ToggleBackpack");
    for _ in 0..3 {
        s.tick(0.1);
    }

    assert!(
        s.eval::<bool>("return Bagnon and Bagnon:IsVisible() or false")
            .unwrap(),
        "the window must be on screen"
    );
    assert_eq!(
        s.eval::<i64>("return Bagnon.size").unwrap(),
        16,
        "Bagnon must size itself off the LIVE backpack (GetContainerNumSlots(0) == 16), not off \
         Bagnon_Forever's empty offline cache — nil UnitName('player') is what sent it there"
    );
    assert_eq!(
        bagnon_slot_quads(&mut s),
        16,
        "sixteen bag slots must actually be DRAWN — the director saw a title and a gold line and \
         nothing else, while every column of the addon survey called this addon fine"
    );
    assert!(
        s.errors().is_empty(),
        "and nothing may raise on the way: {:#?}",
        s.errors()
    );
}

/// **The control that makes the headline mean something: reproduce the director's screen.**
///
/// With the player seated only *after* the addons load — benilla's own order until
/// [`super::seat_from_roster`] landed — Bagnon shows a window whose title and money frame are
/// right and which contains **not one slot**, and raises nothing doing it. If this ever starts
/// drawing slots, the headline above has stopped testing the thing it was written for.
#[test]
fn without_a_player_at_addon_load_bagnon_draws_an_empty_window() {
    let root = corpus_or_skip!();
    let mut s = seat(&root, Seat::AfterAddons);
    s.run("ToggleBackpack()").expect("ToggleBackpack");
    for _ in 0..3 {
        s.tick(0.1);
    }

    // The two things the director COULD see, exactly as reported.
    assert!(
        s.eval::<bool>("return Bagnon:IsVisible()").unwrap(),
        "the window is up"
    );
    assert_eq!(
        s.eval::<String>("return BagnonTitle:GetText()").unwrap(),
        "Harness's Inventory",
        "…with its header"
    );
    // And the thing they could not.
    assert_eq!(
        bagnon_slot_quads(&mut s),
        0,
        "no slot is drawn — this is the report, reproduced"
    );
    assert!(
        s.eval::<String>("return tostring(BagnonItem1)").unwrap() == "nil",
        "not one item button was even created"
    );
}

/// The **other** fault, pinned on its own so a later regression names itself: the reference's
/// `SetItemButton*` family (`assets/ui/ItemButtonTemplate.xml`).
///
/// Bagnon ends every slot update with `SetItemButtonDesaturated` / `SetItemButtonTexture` /
/// `SetItemButtonCount`. While those were nil the raise landed inside `BagnonFrame_AddBag`'s
/// per-slot loop, so the FIRST slot aborted it, `frame.size` never advanced past 0 and the window
/// never reached its own `frame:Show()`. That is a different picture from the one above — nothing
/// on screen at all — and it is what the identity fix alone would have produced.
#[test]
fn the_item_button_helpers_paint_a_slots_icon_and_count() {
    let mut s = UiScript::new().expect("VM");
    s.set_screen_size(1024.0, 768.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "our own FrameXML: {failures:#?}");

    // A button shaped the way the family's naming contract requires, built the way an addon does.
    s.run(
        r#"
        local b = CreateFrame("Button", "ProbeItemButton", UIParent)
        b:SetWidth(37) b:SetHeight(37)
        b:SetPoint("CENTER", UIParent, "CENTER", 0, 0)
        local icon = b:CreateTexture("ProbeItemButtonIconTexture", "BORDER")
        icon:SetAllPoints(b)
        local count = b:CreateFontString("ProbeItemButtonCount", "OVERLAY", "NumberFontNormal")
        count:SetPoint("BOTTOMRIGHT", b, "BOTTOMRIGHT", -5, 2)
        count:Hide()
    "#,
    )
    .expect("probe button");

    s.run(r#"SetItemButtonTexture(ProbeItemButton, "Interface\\Icons\\INV_Misc_Bag_08")"#)
        .expect("SetItemButtonTexture");
    s.run("SetItemButtonCount(ProbeItemButton, 5)")
        .expect("SetItemButtonCount");
    s.run("SetItemButtonDesaturated(ProbeItemButton, 1, 0.5, 0.5, 0.5)")
        .expect("SetItemButtonDesaturated");

    // Attributed to the probe button, never to the whole screen: the default UI paints its own
    // "1".."0" action-bar hotkeys, so a bare "is there a quad reading 1 anywhere" would answer yes
    // whatever these functions did. The `!` case below is the one that would have been fooled.
    let count_text = |s: &UiScript, want: &str| {
        s.extract().iter().any(|q| {
            matches!(&q.content, QuadContent::Text { text, .. } if text.as_deref() == Some(want))
                && s.target_owner_name(q.target).as_deref() == Some("ProbeItemButton")
        })
    };

    s.resolve();
    assert!(
        s.extract().iter().any(|q| matches!(&q.content,
            QuadContent::Texture { path: Some(p), .. } if p.contains("INV_Misc_Bag_08"))),
        "the icon the addon asked for must be on the region"
    );
    assert!(
        count_text(&s, "5"),
        "a stack of 5 shows its count (>1 is the reference's own gate)"
    );
    // The no-shader branch: benilla has no `Texture:SetDesaturated`, so a locked slot takes the
    // reference's own fixed-function fallback — a flat 0.5 grey tint on the icon.
    let tint = s
        .eval::<Vec<f32>>("return {ProbeItemButtonIconTexture:GetVertexColor()}")
        .expect("GetVertexColor");
    assert_eq!(
        &tint[..3],
        &[0.5, 0.5, 0.5],
        "a locked item greys out (ref: `elseif not r or not shaderSupported`)"
    );

    // A count of 1 hides the label again — the reference's gate, not a one-way switch.
    s.run("SetItemButtonCount(ProbeItemButton, 1)").unwrap();
    s.resolve();
    assert!(
        !count_text(&s, "1") && !count_text(&s, "5"),
        "a single item shows no count"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The seat itself, in isolation: what the roster row actually puts in the VM, and the `None` that
/// keeps a capture (no pick, no roster) unchanged.
#[test]
fn the_roster_seat_names_the_character_the_addons_will_meet() {
    let seat = super::seat_from_roster(&roster()).expect("a pending pick seats a player");
    assert_eq!(seat.name.as_deref(), Some("Harness"));
    assert_eq!(seat.level, 60);
    assert_eq!(seat.race_file.as_deref(), Some("Human"));
    assert_eq!(seat.class_file.as_deref(), Some("WARRIOR"));
    assert_eq!(seat.sex, 2, "the wire's 0 is UnitSex's 2");
    assert_eq!(
        seat.faction_group.as_deref(),
        Some("Alliance"),
        "nil here is 24 corpus addons stopping on AceDB-2.0's file-scope concatenation"
    );
    assert!(seat.exists && seat.is_player);
    // Deliberately NOT invented — the descriptor says these, within the second.
    assert_eq!((seat.health, seat.max_health), (0, 0));

    assert!(
        super::seat_from_roster(&crate::char_select::Roster::default()).is_none(),
        "no pick, no seat — a capture must load exactly as it did before"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The INTERACTION half — the layer the render half could not see
// ─────────────────────────────────────────────────────────────────────────────────────────────
//
// The tests above prove Bagnon's window is *drawn*. The director's next report was that it is
// **dead to the touch**: hover raises, click raises, and the console fills up. Every column of the
// addon survey — load, session, probe, and 1230's render column — is blind to that, for one
// structural reason: **none of them clicks**. So these do, through
// [`UiScript::mouse_move`]/[`UiScript::mouse_button`], which is the whole engine path a player's
// mouse takes (hit test → `RegisterForClicks` gate → the frame's own `OnClick`), never a direct
// `s.run("Handler(...)")` that would prove only that a function exists.
//
// `the_whole_bagnon_window_survives_being_used` below is the *enumerator*: it drives every
// gesture in one VM and asserts the error list is empty. A scan can only find what it knows to
// look for; this finds whatever is actually missing.

/// **Bug 1, hover.** `Bagnon_Core/core/Item.lua:136` ends its normal-bag branch in
/// `ContainerFrameItemButton_OnEnter(item)` — the reference's own global. It was nil, so every
/// hover over every slot raised and no tooltip ever appeared.
#[test]
fn hovering_a_bagnon_slot_shows_the_items_tooltip() {
    let root = corpus_or_skip!();
    let mut s = open_bagnon(&root);
    let occupied = bagnon_button_for_slot(&s, 0, 1);

    let (x, y) = centre_of(&mut s, &occupied);
    s.mouse_move(x, y);

    assert!(
        s.errors().is_empty(),
        "a hover must not raise: {:#?}",
        s.errors()
    );
    assert!(
        s.eval::<bool>(&format!("return GameTooltip:IsOwned({occupied})"))
            .unwrap(),
        "the tooltip must belong to the hovered slot ({occupied})"
    );
    assert!(
        s.eval::<bool>("return GameTooltip:IsShown()").unwrap(),
        "…and be on screen"
    );

    // …and it drops again on the way out, the template's own OnLeave.
    let (ex, ey) = (x, y + 400.0);
    s.mouse_move(ex, ey);
    assert!(
        !s.eval::<bool>("return GameTooltip:IsShown()").unwrap(),
        "moving off the slot hides the tooltip"
    );
    assert!(s.errors().is_empty(), "…without raising: {:#?}", s.errors());
}

/// **Bug 1, left click.** `Item.lua:105` routes a plain click to
/// `ContainerFrameItemButton_OnClick(mouseButton, ignoreModifiers)`, whose unmodified left arm is
/// `PickupContainerItem(this:GetParent():GetID(), this:GetID())`. The observable is the cursor.
#[test]
fn left_clicking_a_bagnon_slot_picks_the_item_up() {
    let root = corpus_or_skip!();
    let mut s = open_bagnon(&root);
    let occupied = bagnon_button_for_slot(&s, 0, 1);

    assert!(
        !s.eval::<bool>("return CursorHasItem()").unwrap(),
        "nothing is held before the click"
    );
    click(&mut s, &occupied, "LeftButton");

    assert!(
        s.errors().is_empty(),
        "a left click must not raise: {:#?}",
        s.errors()
    );
    assert!(
        s.eval::<bool>("return CursorHasItem()").unwrap(),
        "the item is now on the cursor"
    );

    // …and clicking an empty slot puts it back down, through the same one entry point.
    let empty = bagnon_button_for_slot(&s, 0, 2);
    click(&mut s, &empty, "LeftButton");
    assert_eq!(
        s.take_container_moves(),
        vec![benilla_ui::script::ContainerMove {
            src_bag: 0,
            src_slot: 1,
            dst_bag: 0,
            dst_slot: 2,
            count: None,
        }],
        "the place half of the same gesture"
    );
    assert!(s.errors().is_empty(), "…nor the place: {:#?}", s.errors());
}

/// **Bug 1, right click.** The reference's right arm with no merchant open is
/// `UseContainerItem(bag, slot)` — use/equip the item.
#[test]
fn right_clicking_a_bagnon_slot_uses_the_item() {
    let root = corpus_or_skip!();
    let mut s = open_bagnon(&root);
    let occupied = bagnon_button_for_slot(&s, 0, 1);

    click(&mut s, &occupied, "RightButton");

    assert!(
        s.errors().is_empty(),
        "a right click must not raise: {:#?}",
        s.errors()
    );
    assert_eq!(
        s.take_container_uses(),
        vec![(0, 1)],
        "right-click uses the slot"
    );
}

/// **Bug 1, drag.** Bagnon registers `OnDragStart`/`OnReceiveDrag` on every item button and routes
/// both to `BagnonItem_OnClick("LeftButton", 1)` → `ContainerFrameItemButton_OnClick`'s
/// `ignoreModifiers` arm. Driven as a real gesture: press, move past the threshold, release.
#[test]
fn dragging_a_bagnon_slot_picks_the_item_up() {
    let root = corpus_or_skip!();
    let mut s = open_bagnon(&root);
    let occupied = bagnon_button_for_slot(&s, 0, 1);
    let empty = bagnon_button_for_slot(&s, 0, 2);

    let (sx, sy) = centre_of(&mut s, &occupied);
    let (dx, dy) = centre_of(&mut s, &empty);
    s.mouse_move(sx, sy);
    s.mouse_button(sx, sy, "LeftButton", true);
    s.mouse_move(sx + 12.0, sy + 12.0); // past DRAG_START_THRESHOLD ⇒ OnDragStart
    s.mouse_move(dx, dy);
    s.mouse_button(dx, dy, "LeftButton", false);

    assert!(
        s.errors().is_empty(),
        "a drag must not raise: {:#?}",
        s.errors()
    );
    assert_eq!(
        s.take_container_moves(),
        vec![benilla_ui::script::ContainerMove {
            src_bag: 0,
            src_slot: 1,
            dst_bag: 0,
            dst_slot: 2,
            count: None,
        }],
        "drag-and-drop moved the item"
    );
}

/// **Bug 2 — our own handler, called the reference's way.** Bagnon takes
/// `MainMenuBarBackpackButton:GetScript("OnEnter")` and calls it back with **no arguments**
/// (`Bagnon.lua:87`), because that is the reference's contract: an XML script body takes nothing
/// and reads the frame off the `this` global. Ours compiled to `function(self, ...)` and its body
/// passed `self` straight into `GameTooltip:SetOwner`, so the hook handed it nil:
///
/// ```text
/// bad argument #2: error converting Lua nil to table
///   BagFrame.xml:789: in function 'BenillaBagToggle_OnEnter'
///   [string "MainMenuBarBackpackButton:OnEnter"]:2: in upvalue 'bMainBag_OnEnter'
/// ```
#[test]
fn the_backpack_button_still_hovers_with_bagnon_holding_its_script() {
    let root = corpus_or_skip!();
    let mut s = open_bagnon(&root);

    // Bagnon really did take the script over — otherwise this test proves nothing.
    assert!(
        s.eval::<bool>(
            "return MainMenuBarBackpackButton:GetScript(\"OnEnter\") == BagnonBlizMainBag_OnEnter"
        )
        .unwrap(),
        "Bagnon_AddBagHooks must have replaced the backpack button's OnEnter"
    );

    let (x, y) = centre_of(&mut s, "MainMenuBarBackpackButton");
    s.mouse_move(x, y);

    assert!(
        s.errors().is_empty(),
        "the hooked hover must not raise: {:#?}",
        s.errors()
    );
    assert!(
        s.eval::<bool>("return GameTooltip:IsOwned(MainMenuBarBackpackButton)")
            .unwrap(),
        "our own tooltip still opens, owned by the button"
    );
}

/// **Bug 3.** `Bagnon_Forever/database/ui.lua:61` sizes its character dropdown from
/// `button:GetTextWidth()` on a **CheckButton** — a real 1.12 Button method (wow-re
/// `widget-api-batch-benilla.md` Q8 lists it present, and `GetStringWidth` absent, on Button).
#[test]
fn a_button_reports_its_own_label_width() {
    let root = corpus_or_skip!();
    let mut s = open_bagnon(&root);

    // Bagnon_Forever's exact shape: a CheckButton built from its own template, with a label.
    s.run(
        r#"
        local b = CreateFrame("CheckButton", "ProbeWidthButton", UIParent, "BagnonDBUINameBox")
        b:SetPoint("CENTER", UIParent, "CENTER", 0, 0)
        b:SetText("ProbeLabelText")
    "#,
    )
    .expect("the probe button");
    s.resolve();

    // Answer the host measure for the button's OWN label, and nothing else — the assertion below
    // is then "the button forwarded to the right region and read the right metric", not merely
    // "some number came back". A headless VM has no font atlas, so an unanswered measure is 0 and
    // a `> 0` assertion would be untestable here (and unfalsifiable there).
    let req = s
        .fontstrings_needing_measure()
        .into_iter()
        .find(|r| r.text == "ProbeLabelText")
        .expect("the label asks to be measured");
    s.set_measured_text_unwrapped(&[(req.id, 61.0, 12.0, req.key)]);

    assert_eq!(
        s.eval::<f64>("return ProbeWidthButton:GetTextWidth()")
            .expect("Button:GetTextWidth"),
        61.0,
        "the Button reports its LABEL's natural extent"
    );
    assert_eq!(
        s.eval::<f64>("return ProbeWidthButton:GetTextHeight()")
            .expect("Button:GetTextHeight"),
        12.0
    );
    // A Button with no label at all answers 0 rather than raising — the reference dereferences a
    // FontString pointer here that a bare `CreateFrame("Button")` leaves null, and what it does
    // then is not byte-read, so this takes the harmless number.
    s.run(r#"CreateFrame("Button", "ProbeLabellessButton", UIParent)"#)
        .unwrap();
    assert_eq!(
        s.eval::<f64>("return ProbeLabellessButton:GetTextWidth()")
            .unwrap(),
        0.0
    );

    // The real consumer, end to end: Bagnon_Forever's own character list.
    s.run("BagnonDBUI_ShowCharacterList(Bagnon)")
        .expect("BagnonDBUI_ShowCharacterList");
    assert!(
        s.errors().is_empty(),
        "the character list must not raise: {:#?}",
        s.errors()
    );
}

/// **Bug 4.** `Bagnon_Core/core/Item.xml:34` hangs `<Model name="$parentCooldown"
/// inherits="CooldownFrameTemplate"/>` off every item button — the reference's own template, which
/// we had never declared, so every one of the 36 buttons warned and the child came out bare.
#[test]
fn the_reference_cooldown_template_resolves() {
    let root = corpus_or_skip!();
    let s = open_bagnon(&root);
    let occupied = bagnon_button_for_slot(&s, 0, 1);

    // The child exists and answers as a frame, whatever Bagnon does to it.
    assert!(
        s.eval::<bool>(&format!("return getglobal(\"{occupied}Cooldown\") ~= nil"))
            .unwrap(),
        "the template's $parentCooldown child must exist"
    );
    // …and the one entry point every consumer drives it through does not raise on it.
    s.run(&format!(
        "CooldownFrame_SetTimer(getglobal(\"{occupied}Cooldown\"), GetTime(), 30, 1)"
    ))
    .expect("CooldownFrame_SetTimer on an addon's cooldown child");
    assert!(s.errors().is_empty(), "{:#?}", s.errors());
}

/// **The enumerator.** Every gesture a player makes in Bagnon's window, in one VM, with the error
/// list asserted empty at the end.
///
/// This is the test the arc was missing. Four landings in a row fixed the errors that happened to
/// fire and left the next layer to be found by the director playing, because every instrument we
/// had asks whether something *raised at load* and none of them touches the window. A scan finds
/// what it knows to look for; this finds what is actually missing, because it does what a player
/// does.
#[test]
fn the_whole_bagnon_window_survives_being_used() {
    let root = corpus_or_skip!();
    let mut s = open_bagnon(&root);

    // 1 · Every slot, hovered and left again — the OnEnter/OnUpdate/OnLeave loop on both an
    //     occupied and an empty slot.
    for i in 1..=16 {
        let name = format!("BagnonItem{i}");
        if s.eval::<bool>(&format!("return getglobal(\"{name}\") ~= nil"))
            .unwrap_or(false)
        {
            let (x, y) = centre_of(&mut s, &name);
            s.mouse_move(x, y);
            s.tick(0.4); // Bagnon's own 0.3s OnUpdate tooltip refresh
            s.mouse_move(x, y + 400.0);
        }
    }

    // 2 · The window's own furniture: the title (a tooltip), the bag row toggle, the close button.
    for name in ["BagnonTitle", "BagnonShowBags", "BagnonCloseButton"] {
        if s.eval::<bool>(&format!("return getglobal(\"{name}\") ~= nil"))
            .unwrap_or(false)
        {
            let (x, y) = centre_of(&mut s, name);
            s.mouse_move(x, y);
        }
    }
    click(&mut s, "BagnonShowBags", "LeftButton"); // show the bag row…

    // 3 · Bagnon's OWN bag buttons (the row it just revealed — `BagnonBags0..4` plus the
    //     keyring's `BagnonBags-2`, from `Bagnon.xml`'s `$parent<id>` naming): hover, click and
    //     drag each. `BagnonBag_OnClick` is `PutItemInBackpack`/`PutItemInBag`, `BagnonBag_OnDrag`
    //     is `PickupBagFromSlot` — three engine verbs, none of which existed here.
    for id in ["0", "1", "2", "3", "4", "-2"] {
        let name = format!("BagnonBags{id}");
        if !s
            .eval::<bool>(&format!(
                "local b = getglobal(\"{name}\") return b ~= nil and b:IsVisible()"
            ))
            .unwrap_or(false)
        {
            continue;
        }
        let (x, y) = centre_of(&mut s, &name);
        s.mouse_move(x, y);
        s.mouse_button(x, y, "LeftButton", true);
        s.mouse_button(x, y, "LeftButton", false);
        s.mouse_button(x, y, "LeftButton", true);
        s.mouse_move(x + 12.0, y + 12.0);
        s.mouse_button(x + 12.0, y + 12.0, "LeftButton", false);
    }

    // 4 · The money frame's three coin buttons — `OpenCoinPickupFrame`, guarded in our own
    //     MoneyFrame.xml and *not* guarded in Bagnon's copy of the same handler.
    for coin in ["Gold", "Silver", "Copper"] {
        let name = format!("BagnonMoneyFrame{coin}Button");
        if s.eval::<bool>(&format!(
            "local b = getglobal(\"{name}\") return b ~= nil and b:IsVisible()"
        ))
        .unwrap_or(false)
        {
            click(&mut s, &name, "LeftButton");
        }
    }

    // 5 · The bag-bar buttons Bagnon hooked on OUR bar — hover, click, and the keyring.
    for name in [
        "MainMenuBarBackpackButton",
        "CharacterBag0Slot",
        "CharacterBag1Slot",
        "KeyRingButton",
    ] {
        if !s
            .eval::<bool>(&format!(
                "local b = getglobal(\"{name}\") return b ~= nil and b:IsVisible()"
            ))
            .unwrap_or(false)
        {
            continue;
        }
        let (x, y) = centre_of(&mut s, name);
        s.mouse_move(x, y);
        s.mouse_button(x, y, "LeftButton", true);
        s.mouse_button(x, y, "LeftButton", false);
        s.mouse_move(x, y + 400.0);
    }

    // **A CLOSED LIST, not "is it empty".** Everything Bagnon reaches that benilla does not have
    // is named here, so this test does two jobs at once: it holds the residual honest (each entry
    // is a gap somebody decided, with a reason, not one nobody noticed), and it fails LOUDLY on
    // any error class that is not on it. "Assert the list is empty" would have had to be deleted
    // or `#[ignore]`d while a known gap stands, and then it would be catching nothing at all.
    //
    // To retire an entry: implement it and delete the line. To add one: you had better be able to
    // say why, in the same sentence.
    const KNOWN_GAPS: &[(&str, &str)] = &[
        // The bag-BUTTON cursor trio (`Bag.lua:199-215` — Bagnon's own bag row, click and drag).
        // Three 1.12 engine bindings benilla does not have: `PutItemInBackpack 0x4c8f70`,
        // `PutItemInBag 0x4c8f00`, `PickupBagFromSlot 0x4c8fa0` (wow-re `system/ui/ledger.tsv`
        // l.1962-1964 has the registrations verified; the DECOMPILATION on disk is partial —
        // Ghidra removed unreachable blocks from two of the three — so what each one actually
        // DOES is not read).
        //
        // **Deliberately not guessed.** The open question is whether `PutItemInBag(inv)`
        // auto-stores the held item into that bag's CONTENTS or places it onto the equipment slot,
        // and those are different opcodes. Writing a cursor transition on the wrong one moves a
        // player's items to the wrong place, which is the most expensive kind of wrong here
        // (the contract §4), so an RE cross-check is dispatched into wow-5875-re and these land on
        // its verdict, not before.
        ("PutItemInBackpack", "Bagnon_Core\\core\\Bag.lua:202"),
        ("PutItemInBag", "Bagnon_Core\\core\\Bag.lua:204"),
        ("PickupBagFromSlot", "Bagnon_Core\\core\\Bag.lua:214"),
        // Money on the cursor. `Frame.lua:537-543` — Bagnon's money frame calls this UNGUARDED
        // where benilla's own `MoneyFrame.xml:411` guards its copy of the same reference handler.
        // It is the head of a whole feature, not a missing line: the reference's
        // `CoinPickupFrame.lua`/`.xml` need `GetCursorMoney`, `PickupPlayerMoney`, a MONEY cursor
        // payload (wow-re `cursor-dragdrop-payload.md` row 2 — benilla's cursor has Item/Spell/
        // Action only) and a keyboard-focused frame with `OnChar`/`OnKeyDown`, which this engine
        // deliberately does not have (see `EnableKeyboard` in the same landing's report).
        ("OpenCoinPickupFrame", "Bagnon_Core\\core\\Frame.lua:543"),
    ];

    let raised = s.errors();
    let unexplained: Vec<&String> = raised
        .iter()
        .filter(|e| !KNOWN_GAPS.iter().any(|(name, _)| e.contains(name)))
        .collect();
    assert!(
        unexplained.is_empty(),
        "using Bagnon raised {} error(s) that are NOT on the known-gap list:\n{:#?}",
        unexplained.len(),
        unexplained
    );
    // …and the other direction, so a gap that quietly closes does not leave a stale entry behind.
    for (name, site) in KNOWN_GAPS {
        assert!(
            raised.iter().any(|e| e.contains(name)),
            "`{name}` is listed as a known gap ({site}) but nothing raised on it — if it is \
             built now, delete the entry"
        );
    }
}
