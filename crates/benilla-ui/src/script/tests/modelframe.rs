//! **The model-pane family's Lua surface** — `Model` and `PlayerModel`, read back through the API
//! that wrote them.
//!
//! The property under test throughout is that these widgets are *state the app renders*, not state
//! the engine interprets: every setter's value must come back out unchanged, and the places where
//! the widget does have an opinion (one yaw slot written by two verbs on two classes; content being
//! an either/or) must hold.
//!
//! Plus the one structural property, guarded as a whole block:
//! [`the_two_model_tables_are_the_references_own`] asserts our surfaces against the reference's
//! enumerated tables in **both** directions, so neither a missing verb nor an invented one can slip
//! past — and so `Model` can never re-acquire the three that are `PlayerModel`'s.

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

    // The yaw. `SetFacing` is the `Model` verb for it (`0x878948[4]`); `SetRotation` writes the
    // same field but belongs to `PlayerModel` and is tested there.
    s.run("MPane:SetFacing(-0.25)").unwrap();
    assert_eq!(s.eval::<f64>("return MPane:GetFacing()").unwrap(), -0.25);

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

    // `SetModel(nil)` is the documented clear and reaches the same place `ClearModel` does — not
    // everything in the corpus calls the dedicated verb.
    s.run("MPane:ClearModel()").unwrap();
    assert_eq!(
        s.eval::<Option<String>>("return MPane:GetModel()").unwrap(),
        None
    );
}

/// **A `<PlayerModel>` is a `<Model>` plus exactly three verbs — and the inheritance runs one way.**
///
/// The client registers four model-pane types and each has its own Lua method table that never
/// repeats its base's; a derived pane reaches its base through the miss leg of `vtable+0x8`
/// (wow-re `ui/scratch/model-pane-method-tables.md` §3). So `PlayerModel`'s three-entry table
/// `0x84f1fc` must sit *over* `Model`'s 23-entry `0x878948`, and nothing may chain the other way:
/// `CSimpleModel`'s lookup `0x76f870` has no leg into `CGCharacterModelBase`'s `0x506260`.
///
/// This is the shape pfUI's unit frames need — `CreateFrame("PlayerModel", ...)` driven by
/// `SetUnit` + `SetCamera`, one verb from each table on the same frame.
#[test]
fn a_player_model_is_a_model_plus_three_and_the_chain_runs_one_way() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        pm = CreateFrame("PlayerModel", "PMPane", UIParent)
        m  = CreateFrame("Model", "MOnly", UIParent)
    "#,
    )
    .unwrap();

    // Its own three resolve on the PlayerModel...
    for verb in ["SetUnit", "RefreshUnit", "SetRotation"] {
        assert_eq!(
            s.eval::<String>(&format!("return type(PMPane.{verb})"))
                .unwrap(),
            "function",
            "PlayerModel must answer its own {verb}"
        );
        // ...and on the plain Model they are ABSENT. This is the assertion that would have caught
        // three years of `SetUnit` published on the wrong class.
        assert_eq!(
            s.eval::<String>(&format!("return type(MOnly.{verb})"))
                .unwrap(),
            "nil",
            "a plain Model must NOT answer {verb} — the chain runs derived -> base only"
        );
    }

    // ...and the base's verbs resolve through the chain, unrepeated. pfUI's portrait line.
    s.run(r#"PMPane:SetUnit("player") PMPane:SetCamera(0)"#)
        .unwrap();
    assert_eq!(
        s.eval::<Option<String>>("return PMPane:GetModel()")
            .unwrap(),
        None,
        "SetUnit displaces the model path — content is an either/or, not layers"
    );
    s.run(r#"PMPane:SetModel("Interface\\Buttons\\Other.mdx")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>("return PMPane:GetModel()").unwrap(),
        r"Interface\Buttons\Other.mdx",
        "...and back the other way, the direction an addon reskinning a paper doll takes"
    );

    // `SetRotation` writes THE SAME yaw field `SetFacing` does — `0x505bb0`'s last instruction is
    // `mov [esi+0x39c], eax`, and `+0x39c` is what `0x76dce0` writes. `GetFacing` reads either.
    s.run("PMPane:SetRotation(1.5)").unwrap();
    assert_eq!(s.eval::<f64>("return PMPane:GetFacing()").unwrap(), 1.5);
    s.run("PMPane:SetFacing(-0.25)").unwrap();
    assert_eq!(
        s.eval::<f64>("return PMPane:GetFacing()").unwrap(),
        -0.25,
        "one slot: SetFacing overwrites what SetRotation wrote"
    );

    // RefreshUnit is a live no-op: the pane stores the unit TOKEN and resolves it at render, so
    // there is no cached appearance to invalidate. It must still exist — the reference's own
    // DressUp/PaperDoll frames call it, so an addon hooking them will too.
    assert!(s.run("PMPane:RefreshUnit()").is_ok());

    // ClearModel — a `Model` verb reached through the chain — empties BOTH content slots.
    s.run(r#"PMPane:SetUnit("player") PMPane:ClearModel()"#)
        .unwrap();
    assert_eq!(
        s.eval::<Option<String>>("return PMPane:GetModel()")
            .unwrap(),
        None
    );
}

/// **`SetModel(nil)` raises; `ClearModel()` clears. They are not the same verb.**
///
/// This binding asserted the opposite until 2026-08-30 — "the documented clear, and how the corpus
/// writes 'no model'" — which was invented, not read. `0x76d950` is shape A (decision 1717's
/// taxonomy): `lua_isstring` gates the argument and `Usage: %s:SetModel("file")` is raised on
/// anything that is not a string or a number. `ClearModel 0x76db20` is a separate table entry.
///
/// The same gate on `ReplaceIconTexture 0x76ed70`, whose *whole* observable behaviour it is: the
/// swap itself lands on a `CM2Model` we do not have, and with no CM2Model the reference drops the
/// call and never replays it.
///
/// A NUMBER is accepted by both — `lua_isstring` takes tags 3|4 and the client renders it to
/// decimal text — which is the half a `Value::String`-only match would get wrong.
#[test]
fn the_string_setters_gate_their_argument_and_a_number_is_a_string() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(r#"g = CreateFrame("Model", "GateM", UIParent)"#)
        .unwrap();

    s.run(r#"GateM:SetModel("Interface\\Buttons\\A.mdx")"#)
        .unwrap();
    for bad in ["nil", "{}", "true", "print", ""] {
        assert!(
            s.run(&format!("GateM:SetModel({bad})")).is_err(),
            "SetModel({bad}) must raise — it is not the clear"
        );
    }
    // ...and none of those raises disturbed the path that was already set.
    assert_eq!(
        s.eval::<String>("return GateM:GetModel()").unwrap(),
        r"Interface\Buttons\A.mdx",
        "a raised setter leaves the pane alone"
    );
    // A number IS a string to `lua_isstring`, and the client renders it to decimal text.
    s.run("GateM:SetModel(42)").unwrap();
    assert_eq!(s.eval::<String>("return GateM:GetModel()").unwrap(), "42");

    // ClearModel is the clear, takes no argument, and empties the pane.
    s.run("GateM:ClearModel()").unwrap();
    assert_eq!(
        s.eval::<Option<String>>("return GateM:GetModel()").unwrap(),
        None
    );

    // `ReplaceIconTexture` — same gate, and that gate is all of it. A good argument is accepted
    // and changes NOTHING observable: the swap targets the CM2Model's type-14 textures, and a
    // pane with no CM2Model drops the call (the reference's own `[widget+0x318] == 0` leg).
    for bad in ["nil", "{}", "true", ""] {
        assert!(
            s.run(&format!("GateM:ReplaceIconTexture({bad})")).is_err(),
            "ReplaceIconTexture({bad}) must raise"
        );
    }
    s.run(r#"GateM:SetModel("Interface\\Buttons\\A.mdx")"#)
        .unwrap();
    s.run(r#"GateM:ReplaceIconTexture("Interface\\Icons\\INV_Misc_QuestionMark")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GateM:GetModel()").unwrap(),
        r"Interface\Buttons\A.mdx",
        "it is a MATERIAL swap, not a content setter — the pane's model is untouched"
    );

    // `AdvanceTime` takes nothing, returns nothing, and does nothing — verified, not stubbed.
    assert_eq!(
        s.eval::<usize>("return table.getn({ GateM:AdvanceTime() })")
            .unwrap(),
        0,
        "AdvanceTime pushes no return value"
    );
}

/// **The whole block, both directions** — decision 1718's rule applied to the two model tables.
///
/// The lists below are the reference's OWN enumerations, transcribed entry-for-entry from
/// wow-re's `ui/scratch/model-pane-method-tables.md` §2.1 and §2.2 (each table's count fixed twice:
/// the registering `mov edx, imm32`, and the dword at `base + 8*count` being the start of the
/// string pool). They are **not** the names anyone noticed were missing — that is exactly the
/// mistake 1718 records, and it was re-made inside the test written to enforce it.
///
/// Guarding both directions is the point:
///
/// - **no gap** — every reference name we claim to publish must resolve;
/// - **no superset** — a name the reference does not have on a table must not resolve there
///   (1189: a name we have and the reference lacks routes an addon down a path the real client
///   never takes);
/// - **the seven we deliberately do not build must stay absent**, so "unbuilt" cannot quietly
///   become "stubbed" without this test being edited to say so.
#[test]
fn the_two_model_tables_are_the_references_own() {
    /// `Model` — `CSimpleModel`, table `0x878948`, 23 entries, in table order.
    const MODEL_23: [&str; 23] = [
        "SetModel",
        "GetModel",
        "ClearModel",
        "SetPosition",
        "SetFacing",
        "SetModelScale",
        "SetSequence",
        "SetSequenceTime",
        "SetCamera",
        "SetLight",
        "GetLight",
        "GetPosition",
        "GetFacing",
        "GetModelScale",
        "AdvanceTime",
        "ReplaceIconTexture",
        "SetFogColor",
        "GetFogColor",
        "SetFogNear",
        "GetFogNear",
        "SetFogFar",
        "GetFogFar",
        "ClearFog",
    ];
    /// `PlayerModel` — `CGCharacterModelBase`, table `0x84f1fc`, 3 entries, in table order.
    const PLAYERMODEL_3: [&str; 3] = ["SetUnit", "RefreshUnit", "SetRotation"];
    /// The subset of [`MODEL_23`] this client does not build. Named, not stubbed (1134 §4) — their
    /// bodies are uncarved and they have no corpus caller. Moving a name OUT of here means the
    /// verb was actually implemented — as `AdvanceTime` and `ReplaceIconTexture` were, once
    /// wow-re carved them (`ui/scratch/model-advancetime-replaceicontexture.md`). The five left
    /// are the fog near/far set, whose `ClearFog` semantics over the colour/near/far triple nobody
    /// has read; guessing that clear would read as knowledge.
    const UNBUILT: [&str; 5] = [
        "SetFogNear",
        "GetFogNear",
        "SetFogFar",
        "GetFogFar",
        "ClearFog",
    ];

    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        CreateFrame("Model", "GuardM", UIParent)
        CreateFrame("PlayerModel", "GuardPM", UIParent)
    "#,
    )
    .unwrap();
    let is_fn = |s: &UiScript, frame: &str, name: &str| {
        s.eval::<String>(&format!("return type({frame}.{name})"))
            .unwrap()
            == "function"
    };

    for name in MODEL_23 {
        let want = !UNBUILT.contains(&name);
        // A `Model` verb resolves on BOTH panes — on the PlayerModel through the chain.
        for frame in ["GuardM", "GuardPM"] {
            assert_eq!(
                is_fn(&s, frame, name),
                want,
                "{frame}.{name}: table 0x878948 has it; built = {want}"
            );
        }
    }
    for name in PLAYERMODEL_3 {
        assert!(
            is_fn(&s, "GuardPM", name),
            "GuardPM.{name}: table 0x84f1fc entry, all three are built"
        );
        assert!(
            !is_fn(&s, "GuardM", name),
            "GuardM.{name}: 0x84f1fc is NOT reachable from CSimpleModel's lookup"
        );
    }
    // The two names a `strings` scan of the model band would tempt anyone into, which do not exist
    // in 5875 in ANY form (substring scan of the mapped image returns 0, positive control 27 hits
    // for `Creature`). They are later-expansion verbs; publishing one is decision 1189's error.
    for name in ["SetCreature", "SetCustomRace"] {
        for frame in ["GuardM", "GuardPM"] {
            assert!(
                !is_fn(&s, frame, name),
                "{frame}.{name} does not exist in 1.12.1.5875"
            );
        }
    }
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

    // **Fog is NOT the same shape, and this used to assert the opposite.** It read "unset fog
    // returns NOTHING rather than three zeros — a pane with no fog and a pane fogged to black are
    // different states", which is a good argument about a model the client does not have: there is
    // no unset state. The fog colour is one packed `0xAARRGGBB` dword whose ctor writes
    // `0xffffffff`, so a fresh pane reads **four** values, `1, 1, 1, 1` (decision 1845).
    assert_eq!(
        s.eval::<usize>("return table.getn({ MLight:GetFogColor() })")
            .unwrap(),
        4
    );
    assert_eq!(
        s.eval::<(f64, f64, f64, f64)>("return MLight:GetFogColor()")
            .unwrap(),
        (1.0, 1.0, 1.0, 1.0),
        "never set is white and opaque, not four zeros"
    );

    // Three arguments set alpha to **1.0**, not 0 — the fifth is guarded with that default where
    // r/g/b are read unconditionally. The round trip is LOSSY by 8 bits a channel, because the
    // store is that packed dword: 0.1 does not survive, 0.2 does.
    s.run("MLight:SetFogColor(0.1, 0.2, 0.3)").unwrap();
    let (r, g, b, a): (f64, f64, f64, f64) = s.eval("return MLight:GetFogColor()").unwrap();
    assert_eq!(a, 1.0, "the omitted alpha defaults to 1.0");
    for (got, want) in [(r, 0.1), (g, 0.2), (b, 0.3)] {
        assert!(
            (got - want).abs() <= 1.0 / 255.0,
            "within one 8-bit step of {want}, got {got}"
        );
    }

    // …and the alpha really is the fifth argument, on the same clamp as the rest.
    s.run("MLight:SetFogColor(1, 1, 1, 0)").unwrap();
    assert_eq!(
        s.eval::<f64>("local _, _, _, a = MLight:GetFogColor() return a")
            .unwrap(),
        0.0
    );
}
