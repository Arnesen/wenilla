//! Regression: a BLP2 texture with an unknown `alpha_type` byte must still decode. Real 1.12-era
//! particle/effect art (e.g. `particles/dust1.blp` — palettized, `alpha_bits == 0`, but a stale
//! `alpha_type == 2`) used to abort the whole decode in `wow_blp`; our fork falls back to no-alpha.
//! Skips (passes) when the client isn't present at `<repo>/WoW/Data`.

use benilla_formats::{blp_to_rgba, open_chain};

#[test]
fn decodes_blp2_unknown_alpha_type() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let bytes = chain
        .read_file("particles\\dust1.blp")
        .expect("read particles/dust1.blp");

    let (w, h, rgba) = blp_to_rgba(&bytes).expect("dust1.blp must decode despite alpha_type == 2");
    assert!(w > 0 && h > 0, "decoded dimensions {w}x{h}");
    assert_eq!(
        rgba.len(),
        (w * h * 4) as usize,
        "RGBA8 buffer matches dimensions"
    );
}
