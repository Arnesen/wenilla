//! The Era atlas seam (decision 0950): `atlas=` in XML, `SetAtlas`/`GetAtlas` at runtime, and the
//! warn-once miss drain — against a hand-pushed table. Disk and manifest.json belong to the app;
//! these tests prove the script/loader side alone.

use crate::framexml;
use crate::loader;
use crate::script::{EraAtlasEntry, UiScript};

/// The real `options_innerframe` numbers (fdid 1318750, rect [1,150,887,768] in 1024×1024) so a
/// regression here reads like the manifest it mirrors.
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.set_era_atlases([
        (
            "options_innerframe".to_string(),
            EraAtlasEntry {
                file: "era:textures/1318750.blp".to_string(),
                uv: [1.0 / 1024.0, 887.0 / 1024.0, 150.0 / 1024.0, 768.0 / 1024.0],
                size: [886.0, 618.0],
            },
        ),
        (
            "checkbox-minimal".to_string(),
            EraAtlasEntry {
                file: "era:textures/4614134.blp".to_string(),
                uv: [1.0 / 64.0, 31.0 / 64.0, 1.0 / 64.0, 30.0 / 64.0],
                size: [30.0, 29.0],
            },
        ),
    ]);
    s
}

#[test]
fn xml_atlas_attribute_resolves_texture_uv_and_size() {
    let mut s = harness();
    let doc = framexml::parse(
        r#"<Ui>
            <Frame name="T">
                <Size><AbsDimension x="920" y="724"/></Size>
                <Anchors><Anchor point="CENTER"/></Anchors>
                <Layers><Layer level="OVERLAY">
                    <Texture name="$parentInner" atlas="Options_InnerFrame" useAtlasSize="true"/>
                </Layer></Layers>
            </Frame>
        </Ui>"#,
    )
    .unwrap();
    let report = loader::load(&s, &doc, &|_| None);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

    // Case-insensitive resolve; the canonical (lowercase) name is what GetAtlas reports.
    assert_eq!(
        s.eval::<String>("return TInner:GetAtlas()").unwrap(),
        "options_innerframe"
    );
    let (l, r, t, b): (f32, f32, f32, f32) = s.eval("return TInner:GetTexCoord()").unwrap();
    assert!((l - 1.0 / 1024.0).abs() < 1e-6 && (r - 887.0 / 1024.0).abs() < 1e-6);
    assert!((t - 150.0 / 1024.0).abs() < 1e-6 && (b - 768.0 / 1024.0).abs() < 1e-6);
    // useAtlasSize applied the member's nominal size.
    assert_eq!(s.eval::<f32>("return TInner:GetWidth()").unwrap(), 886.0);
    assert_eq!(s.eval::<f32>("return TInner:GetHeight()").unwrap(), 618.0);
    // Nothing missed.
    assert!(s.take_era_atlas_misses().is_empty());
}

#[test]
fn runtime_setatlas_swaps_and_settexture_clears() {
    let s = harness();
    let doc = framexml::parse(
        r#"<Ui>
            <Frame name="T">
                <Size><AbsDimension x="100" y="100"/></Size>
                <Anchors><Anchor point="CENTER"/></Anchors>
                <Layers><Layer>
                    <Texture name="$parentTex" atlas="options_innerframe"/>
                </Layer></Layers>
            </Frame>
        </Ui>"#,
    )
    .unwrap();
    let report = loader::load(&s, &doc, &|_| None);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

    // The category-row idiom: swap atlas at runtime (hover/selected), size riding along.
    s.run("TTex:SetAtlas(\"checkbox-minimal\", true)").unwrap();
    assert_eq!(
        s.eval::<String>("return TTex:GetAtlas()").unwrap(),
        "checkbox-minimal"
    );
    assert_eq!(s.eval::<f32>("return TTex:GetWidth()").unwrap(), 30.0);

    // A plain SetTexture makes the region ordinary again — GetAtlas goes nil.
    s.run("TTex:SetTexture(\"Interface\\\\Buttons\\\\UI-Panel-Button-Up\")")
        .unwrap();
    assert!(s.eval::<bool>("return TTex:GetAtlas() == nil").unwrap());
}

#[test]
fn unknown_atlas_is_a_warn_once_miss_not_an_error() {
    let mut s = harness();
    let doc = framexml::parse(
        r#"<Ui>
            <Frame name="T">
                <Size><AbsDimension x="100" y="100"/></Size>
                <Anchors><Anchor point="CENTER"/></Anchors>
                <Layers><Layer>
                    <Texture name="$parentTex" atlas="not-extracted-yet"/>
                </Layer></Layers>
            </Frame>
        </Ui>"#,
    )
    .unwrap();
    // The load survives (a stale extraction must not kill the whole XML pass) …
    let report = loader::load(&s, &doc, &|_| None);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    // … the region draws nothing …
    assert!(s.eval::<bool>("return TTex:GetAtlas() == nil").unwrap());
    // … and the miss names itself exactly once, then the drain clears.
    s.run("TTex:SetAtlas(\"not-extracted-yet\")").unwrap();
    assert_eq!(
        s.take_era_atlas_misses(),
        vec!["not-extracted-yet".to_string()]
    );
    assert!(s.take_era_atlas_misses().is_empty());
}
