//! TexCoords + Font objects (decision 0084).

use super::common::script;
use crate::script::*;

/// `SetTexCoord(l, r, t, b)` writes the region's UV sub-rect; it surfaces on the extracted quad,
/// `GetTexCoord` reads it back; the 8-arg affine form carries per-corner UVs (the route-line
/// draw's rotated quads).
#[test]
fn set_tex_coord_changes_extracted_uv() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        f = CreateFrame("Frame", "TcF")
        f:SetWidth(100); f:SetHeight(100); f:SetPoint("CENTER")
        t = f:CreateTexture("TcTex", "ARTWORK")
        t:SetTexture("Interface\\Foo")
        t:SetTexCoord(0.1, 0.6, 0.2, 0.8)
    "#,
    )
    .unwrap();
    s.resolve();
    let tex = s
        .extract()
        .into_iter()
        .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("Foo")))
        .expect("the Foo texture quad");
    assert!(
        matches!(&tex.content, QuadContent::Texture { tex_coords: Some(TexCoords::Rect(tc)), .. } if *tc == [0.1, 0.6, 0.2, 0.8]),
        "SetTexCoord surfaces on the extracted quad, got {:?}",
        tex.content
    );

    let (l, r, t, b): (f32, f32, f32, f32) = s.eval("return t:GetTexCoord()").unwrap();
    assert_eq!((l, r, t, b), (0.1, 0.6, 0.2, 0.8));

    // The 8-arg affine form (arg order UL, LL, UR, LR) lands as per-corner UVs in screen order
    // [TL, TR, BR, BL] — here a 90° rotation of the full texture.
    s.run("t:SetTexCoord(0,1, 1,1, 0,0, 1,0)").unwrap();
    s.resolve();
    let tex = s
        .extract()
        .into_iter()
        .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("Foo")))
        .unwrap();
    assert!(
        matches!(
            &tex.content,
            QuadContent::Texture { tex_coords: Some(TexCoords::Corners(c)), .. }
                if *c == [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]
        ),
        "the affine form carries corners in screen winding, got {:?}",
        tex.content
    );
    // A no-arg reset returns to the full texture.
    s.run("t:SetTexCoord()").unwrap();
    s.resolve();
    let tex = s
        .extract()
        .into_iter()
        .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("Foo")))
        .unwrap();
    assert!(matches!(
        &tex.content,
        QuadContent::Texture {
            tex_coords: None,
            ..
        }
    ));
}

/// `SetFontObject("Name")` copies a registered [`FontObject`]'s face/height/color onto the
/// FontString; `GetFontObject` reads the name back; a later call re-points it (the resolved paint on
/// the extracted quad follows). An unknown name errors.
#[test]
fn set_font_object_repoints_fontstring() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.register_font_object(
        "Big",
        FontObject {
            font: Some("Fonts\\MORPHEUS.TTF".into()),
            height: Some(18.0),
            color: Some([0.0, 0.0, 0.0, 1.0]),
            outline: Outline::None,
            justify_h: None,
            justify_v: None,
            shadow: None,
        },
    );
    s.register_font_object(
        "Small",
        FontObject {
            font: Some("Fonts\\FRIZQT__.TTF".into()),
            height: Some(10.0),
            color: Some([1.0, 1.0, 1.0, 1.0]),
            outline: Outline::None,
            justify_h: None,
            justify_v: None,
            shadow: None,
        },
    );
    s.run(
        r#"
        f = CreateFrame("Frame", "FoF")
        f:SetWidth(120); f:SetHeight(30); f:SetPoint("CENTER")
        fs = f:CreateFontString("FoText", "ARTWORK")
        fs:SetText("Hi")
        fs:SetFontObject("Big")
    "#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<String>("return fs:GetFontObject()").unwrap(),
        "Big"
    );

    let resolved = |s: &UiScript| -> (Option<String>, Option<f32>, Option<[f32; 4]>) {
        s.extract()
            .into_iter()
            .find_map(|q| match q.content {
                QuadContent::Text {
                    font,
                    font_height,
                    color,
                    ..
                } => Some((font, font_height, color)),
                _ => None,
            })
            .expect("a text quad")
    };
    s.resolve();
    assert_eq!(
        resolved(&s),
        (
            Some("Fonts\\MORPHEUS.TTF".into()),
            Some(18.0),
            Some([0.0, 0.0, 0.0, 1.0])
        )
    );

    // Re-point.
    s.run("fs:SetFontObject('Small')").unwrap();
    s.resolve();
    assert_eq!(
        resolved(&s),
        (
            Some("Fonts\\FRIZQT__.TTF".into()),
            Some(10.0),
            Some([1.0, 1.0, 1.0, 1.0])
        )
    );

    // An unknown font object errors (never a silent no-op).
    assert!(s.run("fs:SetFontObject('Nope')").is_err());
}

/// `justifyV`: MIDDLE is the default (the client's own FontString default — what seats the money
/// numbers on their coins' centerline), `SetJustifyV` overrides it, and a font object carrying one
/// applies it on `SetFontObject`.
#[test]
fn justify_v_defaults_middle_and_overrides() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        f = CreateFrame("Frame", "JvF")
        f:SetWidth(100); f:SetHeight(30); f:SetPoint("CENTER")
        fs = f:CreateFontString("JvText", "ARTWORK")
        fs:SetText("Hi")
    "#,
    )
    .unwrap();
    let justify_v = |s: &UiScript| {
        s.extract()
            .into_iter()
            .find_map(|q| match q.content {
                QuadContent::Text { justify_v, .. } => Some(justify_v),
                _ => None,
            })
            .expect("a text quad")
    };
    s.resolve();
    assert_eq!(justify_v(&s), JustifyV::Middle, "the client default");

    s.run("fs:SetJustifyV('BOTTOM')").unwrap();
    s.resolve();
    assert_eq!(justify_v(&s), JustifyV::Bottom);

    // A font object carrying justify_v re-points it.
    s.register_font_object(
        "TopFont",
        FontObject {
            font: Some("Fonts\\FRIZQT__.TTF".into()),
            height: Some(12.0),
            color: None,
            outline: Outline::None,
            justify_h: None,
            justify_v: Some(JustifyV::Top),
            shadow: None,
        },
    );
    s.run("fs:SetFontObject('TopFont')").unwrap();
    s.resolve();
    assert_eq!(justify_v(&s), JustifyV::Top);
}
