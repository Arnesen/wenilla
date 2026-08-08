//! The Minimap widget kind (decision 0203): zoom API + the extracted content hole.

use super::common::script;
use crate::script::*;
use crate::widget::{MINIMAP_DEFAULT_ZOOM, MINIMAP_ZOOM_LEVELS};

/// A `<Minimap>`-kind frame carries its zoom out through extraction as [`QuadContent::Minimap`]
/// at the frame's own draw slot, and the zoom API clamps like the client's `set_zoom` (0..=5).
#[test]
fn minimap_zoom_api_and_extract() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        m = CreateFrame("Minimap", "TestMinimap")
        m:SetWidth(140); m:SetHeight(140); m:SetPoint("TOPRIGHT")
    "#,
    )
    .unwrap();

    // Defaults + the clamp law. Both indices seed from the CVar default "3", not 0.
    assert_eq!(
        s.eval::<u8>("return m:GetZoom()").unwrap(),
        MINIMAP_DEFAULT_ZOOM
    );
    assert_eq!(
        s.eval::<u8>("return m:GetZoomLevels()").unwrap(),
        MINIMAP_ZOOM_LEVELS
    );
    s.run("m:SetZoom(3)").unwrap();
    assert_eq!(s.eval::<u8>("return m:GetZoom()").unwrap(), 3);
    s.run("m:SetZoom(99)").unwrap();
    assert_eq!(
        s.eval::<u8>("return m:GetZoom()").unwrap(),
        MINIMAP_ZOOM_LEVELS - 1,
        "SetZoom clamps at levels-1 like the client's 0x6daa10"
    );
    s.run("m:SetZoom(-2)").unwrap();
    assert_eq!(s.eval::<u8>("return m:GetZoom()").unwrap(), 0);

    // The widget's own slot extracts as the Minimap content hole, carrying the zoom.
    s.resolve();
    let mm = s
        .extract()
        .into_iter()
        .find(|q| matches!(&q.content, QuadContent::Minimap { .. }))
        .expect("the Minimap content quad");
    assert!(
        matches!(
            &mm.content,
            QuadContent::Minimap {
                zoom: 0,
                inside_zoom: 3
            }
        ),
        "extract carries both live indices: the outdoor one we drove to 0, the indoor one still at \
         its untouched default, got {:?}",
        mm.content
    );
    assert!(
        mm.rect.is_some(),
        "a sized+anchored Minimap resolves a rect"
    );

    // Duck-typing: the zoom API must NOT leak onto other kinds (per-kind method registries).
    s.run(r#"plain = CreateFrame("Frame", "PlainF")"#).unwrap();
    assert!(
        s.eval::<bool>("return plain.SetZoom == nil").unwrap(),
        "SetZoom must resolve nil on a plain Frame"
    );
}

/// The client keeps **two** zoom indices and routes `GetZoom`/`SetZoom` on WMO containment (the
/// inside flag `0xceaa60` → outdoor `0x86f698` / indoor `0x86f69c`). Each persists across the
/// transition: zooming the inn's map right in must not disturb the zoom you left outside.
#[test]
fn minimap_indoor_and_outdoor_zoom_indices_are_independent() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        m = CreateFrame("Minimap", "TestMinimap")
        m:SetWidth(140); m:SetHeight(140); m:SetPoint("TOPRIGHT")
    "#,
    )
    .unwrap();

    // Outside: the zoom API drives the outdoor index.
    s.run("m:SetZoom(2)").unwrap();
    assert_eq!(s.eval::<u8>("return m:GetZoom()").unwrap(), 2);

    // Step inside: the API now reads/writes the indoor index, still at its own untouched default.
    s.set_minimap_inside(true);
    assert_eq!(
        s.eval::<u8>("return m:GetZoom()").unwrap(),
        MINIMAP_DEFAULT_ZOOM,
        "indoors reads the separate indoor index, not the outdoor 2"
    );
    s.run("m:SetZoom(5)").unwrap();
    assert_eq!(s.eval::<u8>("return m:GetZoom()").unwrap(), 5);

    // Both indices ride out through extraction, whatever the flag says.
    s.resolve();
    let mm = s
        .extract()
        .into_iter()
        .find(|q| matches!(&q.content, QuadContent::Minimap { .. }))
        .expect("the Minimap content quad");
    assert!(
        matches!(
            &mm.content,
            QuadContent::Minimap {
                zoom: 2,
                inside_zoom: 5
            }
        ),
        "extract carries both indices independently, got {:?}",
        mm.content
    );

    // Step back outside: the outdoor zoom is exactly where we left it.
    s.set_minimap_inside(false);
    assert_eq!(
        s.eval::<u8>("return m:GetZoom()").unwrap(),
        2,
        "the outdoor index survived an indoor zoom"
    );
}

/// **The level persists** (decision 1131). `SetZoom` writes the live index *and* the matching CVar
/// — `minimapInsideZoom` while inside a WMO, `minimapZoom` outside — the client's own `set_zoom` →
/// `CVar::Set` pair, which is the whole reason a zoom survives a restart. The host push in the
/// other direction (the seed) does *not* echo back as a change.
#[test]
fn setzoom_persists_the_level_through_the_cvar_it_belongs_to() {
    let mut s = script();
    s.register_cvars([("minimapZoom", "3"), ("minimapInsideZoom", "3")]);
    s.run(r#"m = CreateFrame("Minimap", "TestMinimap")"#)
        .unwrap();

    // The seed is a HOST write: it moves both live indices and queues nothing (the host is the
    // one that just read them off disk — an echo would re-dirty the file it loaded).
    s.set_minimap_zoom(1, 4);
    assert_eq!(s.eval::<u8>("return m:GetZoom()").unwrap(), 1);
    assert!(s.take_cvar_changes().is_empty(), "the seed must not echo");

    // Outdoors, a zoom writes the outdoor CVar and only that one.
    s.run("m:SetZoom(5)").unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("minimapZoom".to_string(), "5".to_string())]
    );
    assert_eq!(s.cvar("minimapInsideZoom").as_deref(), Some("3"));

    // Indoors it writes the indoor one — the two levels persist separately, like the indices.
    s.set_minimap_inside(true);
    s.run("m:SetZoom(0)").unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("minimapInsideZoom".to_string(), "0".to_string())]
    );
    assert_eq!(s.cvar("minimapZoom").as_deref(), Some("5"));

    // A no-op zoom queues nothing: quiet frames stay quiet, and the config stays clean.
    s.run("m:SetZoom(0)").unwrap();
    assert!(s.take_cvar_changes().is_empty());

    // The seed clamps like `set_zoom` does, so a hand-edited config.toml cannot seed out of range.
    s.set_minimap_zoom(99, 99);
    s.set_minimap_inside(false);
    assert_eq!(
        s.eval::<u8>("return m:GetZoom()").unwrap(),
        MINIMAP_ZOOM_LEVELS - 1
    );
}

/// A VM whose host registered nothing (a bare test harness, a glue-only run) still zooms — the
/// engine-side CVar write is a silent no-op there, not a warning: engine writes are code, not UI
/// content, so a miss means this build's host does not back the var.
#[test]
fn zooming_without_a_registered_cvar_table_is_silent() {
    let mut s = script();
    s.run(r#"m = CreateFrame("Minimap", "TestMinimap") m:SetZoom(4)"#)
        .unwrap();
    assert_eq!(s.eval::<u8>("return m:GetZoom()").unwrap(), 4);
    assert!(s.take_cvar_changes().is_empty());
    assert!(
        s.take_warnings().is_empty(),
        "no warning for an engine write"
    );
}
