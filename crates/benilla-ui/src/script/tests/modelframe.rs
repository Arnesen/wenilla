//! **The `Model` widget's Lua surface** — the pane's scene, read back through the API that wrote it.
//!
//! The property under test throughout is that this widget is *state the app renders*, not state the
//! engine interprets: every setter's value must come back out unchanged, and the two places where
//! the widget does have an opinion (the two names sharing one yaw slot, and content being an
//! either/or) must hold.

use super::common::script;
use crate::script::UiScript;

/// The whole scene, set and read back — plus the widget's three actual behaviours: `SetRotation`
/// and `SetFacing` are one slot, `SetModel` and `SetUnit` displace each other, and `ClearModel`
/// empties both.
#[test]
fn the_model_pane_holds_the_scene_it_was_given() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(r#"m = CreateFrame("Model", "MPane", UIParent)"#)
        .unwrap();

    // A fresh pane: no content, unit SCALE (not zero — a model at scale 0 is invisible, which is
    // why ModelState hand-writes its Default), no yaw, at the origin.
    assert_eq!(
        s.eval::<(Option<String>, f64, f64)>(
            "return MPane:GetModel(), MPane:GetModelScale(), MPane:GetFacing()"
        )
        .unwrap(),
        (None, 1.0, 0.0),
        "a fresh pane has no model and unit scale"
    );

    // The path round-trips verbatim — the client's own path space, backslashes and `.mdx` intact.
    // pfUI's autocast shine is exactly this call.
    s.run(r#"MPane:SetModel("Interface\\Buttons\\UI-AutoCastButton.mdx")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>("return MPane:GetModel()").unwrap(),
        r"Interface\Buttons\UI-AutoCastButton.mdx"
    );

    // ── ONE YAW SLOT, TWO NAMES. Both are in the binary; the shipped FrameXML drives the tabard
    //    and character panes with SetRotation and never calls SetFacing, while addons reach for
    //    either. Two independent yaws would be a bug invisible until one frame used both names.
    s.run("MPane:SetRotation(1.5)").unwrap();
    assert_eq!(s.eval::<f64>("return MPane:GetFacing()").unwrap(), 1.5);
    s.run("MPane:SetFacing(-0.25)").unwrap();
    assert_eq!(
        s.eval::<f64>("return MPane:GetFacing()").unwrap(),
        -0.25,
        "SetFacing writes the same slot SetRotation does"
    );

    // Scale, camera, position — plain storage, read back through their own getters.
    s.run("MPane:SetModelScale(0.4) MPane:SetCamera(2) MPane:SetPosition(0.1, -0.2, 3)")
        .unwrap();
    assert_eq!(
        s.eval::<f64>("return MPane:GetModelScale()").unwrap(),
        0.4_f32 as f64
    );
    let (x, y, z): (f64, f64, f64) = s.eval("return MPane:GetPosition()").unwrap();
    assert_eq!(
        (x as f32, y as f32, z as f32),
        (0.1, -0.2, 3.0),
        "GetPosition returns the three numbers SetPosition took"
    );

    // ── CONTENT IS AN EITHER/OR. SetUnit and SetModel are two ways to fill one pane, not layers:
    //    after a SetUnit, GetModel must not answer a path from three frames ago.
    s.run(r#"MPane:SetUnit("player")"#).unwrap();
    assert_eq!(
        s.eval::<Option<String>>("return MPane:GetModel()").unwrap(),
        None,
        "SetUnit displaces the model path"
    );
    s.run(r#"MPane:SetModel("Interface\\Buttons\\Other.mdx")"#)
        .unwrap();
    // ...and back the other way, which is the direction an addon reskinning the dress-up pane takes.
    assert_eq!(
        s.eval::<String>("return MPane:GetModel()").unwrap(),
        r"Interface\Buttons\Other.mdx"
    );

    // RefreshUnit is a live no-op: our pane stores the unit TOKEN and resolves it at render, so
    // there is no cached appearance to invalidate. It must still exist — the reference's own
    // DressUp/PaperDoll frames call it, so an addon hooking them will too.
    assert!(s.run("MPane:RefreshUnit()").is_ok());

    // ClearModel empties BOTH content slots, not just the one that was last set.
    s.run("MPane:ClearModel()").unwrap();
    assert_eq!(
        s.eval::<Option<String>>("return MPane:GetModel()").unwrap(),
        None
    );
}

/// **`SetSequenceTime` is a scrub INTO the current sequence**, so changing the sequence drops it.
///
/// The cooldown indicator drives this pair every frame — `SetSequence(n)` then
/// `SetSequenceTime(n, ms)` — and carrying a stale scrub across a sequence change would park the
/// new animation at a time belonging to the previous one.
#[test]
fn a_sequence_change_drops_the_scrub() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        m = CreateFrame("Model", "MSeq", UIParent)
        m:SetModel("Interface\\Buttons\\UI-AutoCastButton.mdx")
        m:SetSequence(0)
        m:SetSequenceTime(0, 250)
    "#,
    )
    .unwrap();

    // 1.12 has no `GetSequence`, so the scrub is read off the model the way `simplehtml`'s tests
    // read their blocks — through the arena, because there is no Lua getter to read it through.
    let scrub = |s: &UiScript| {
        let lua = s.lua();
        let model = lua.app_data_ref::<crate::script::Model>().expect("model");
        let fh = model.arena.lookup("MSeq").expect("MSeq frame");
        match &model.arena.frame(fh).expect("live frame").kind_state {
            crate::widget::KindState::Model(m) => (m.sequence, m.sequence_time),
            _ => panic!("MSeq is not a Model"),
        }
    };
    assert_eq!(scrub(&s), (0, Some((0, 250))));

    s.run("MSeq:SetSequence(3)").unwrap();
    assert_eq!(
        scrub(&s),
        (3, None),
        "a new sequence starts unscrubbed — the old (sequence, ms) pair is not carried across"
    );

    // ...and ClearModel drops the scrub with the content.
    s.run("MSeq:SetSequenceTime(3, 40) MSeq:ClearModel()")
        .unwrap();
    assert_eq!(scrub(&s).1, None);
}

/// `SetLight`'s numbers are stored and returned **verbatim**, however many there are.
///
/// The engine core has no lighting model; typing this tuple would assert a scene semantics nobody
/// has verified, and a wrong typing is worse than an opaque one because it reads as knowledge. So
/// the contract is exactly "what went in comes out".
#[test]
fn the_light_tuple_is_opaque_and_survives_the_round_trip() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        m = CreateFrame("Model", "MLight", UIParent)
        m:SetLight(1, 0, 0, -1, -1, 0.7, 1, 1, 1, 0.8, 1, 1, 1)
    "#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<usize>("return table.getn({ MLight:GetLight() })")
            .unwrap(),
        13,
        "every number handed to SetLight comes back"
    );
    assert_eq!(
        s.eval::<(f64, f64)>("local t = { MLight:GetLight() } return t[1], t[6]")
            .unwrap(),
        (1.0, 0.7_f32 as f64)
    );

    // Fog is the same shape, and unset fog returns NOTHING rather than three zeros — a pane with no
    // fog and a pane fogged to black are different states.
    assert_eq!(
        s.eval::<usize>("return table.getn({ MLight:GetFogColor() })")
            .unwrap(),
        0
    );
    s.run("MLight:SetFogColor(0.1, 0.2, 0.3)").unwrap();
    let (r, g, b): (f64, f64, f64) = s.eval("return MLight:GetFogColor()").unwrap();
    assert_eq!((r as f32, g as f32, b as f32), (0.1, 0.2, 0.3));
}
