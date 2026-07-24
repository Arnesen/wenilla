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
