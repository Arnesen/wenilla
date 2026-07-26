//! The per-instance `MeshTag` channel: **one home for its bit conventions and ownership protocol**
//! (decisions 0066, 0173 — the typed field layout).
//!
//! Every `WowModelMaterial` submesh carries a Bevy `MeshTag` (a raw `u32` the shader reads per
//! instance). **Bits 31 and 30 are standalone flags; bits 0..=29 are the payload**, whose meaning
//! switches on **material state** — so per entity the payload has exactly one meaning at a time:
//!
//! - **Highlight** ([`HIGHLIGHT_BIT`], bit 31): the hover/target model-brighten flag — the shader
//!   adds the client's emissive lift to the lighting sum when set. Orthogonal by construction:
//!   both payload modes below never set bit 31, and the shader masks it off before reading the
//!   payload — so the untagged-⇒-opaque `0` sentinel still works on the masked value.
//! - **Interior fog** ([`INTERIOR_FOG_BIT`], bit 30): this instance's model stands in a WMO
//!   interior, so the shader fogs it with the INTERIOR triple (shared-light rows 18-19) instead of
//!   the scene fog — the reference stages a unit's fog by the unit's OWN interior classification
//!   (`0x71c110` → collector `+0x184..`, lane by `[node+0xc]`; wow-re `m2-unit-interior-fog.md`),
//!   so an indoor character never carries the storm's near veil while the camera is inside.
//!   Carried by BOTH indoor laws: [`probe_bits`] bakes it in, and the entity classifier ORs it
//!   onto the Matte (day/night) payload. Masked off with bit 31 before the payload decode.
//! - **Exterior payload** (the default): two typed fields (decision 0173's redesign of the old
//!   whole-payload f32 alpha, triggered by the ground-shade writer 0066 anticipated):
//!   - bits 0..=15 — the **fade alpha** as u16 (`0xffff` = opaque; 16 bits is far past perceptual
//!     for a sub-second ramp), multiplying the cutout alpha. A whole payload of `0` is the shader's
//!     *untagged ⇒ opaque* sentinel, so the alpha field is never legitimately `0` — a true zero
//!     alpha writes `1` (≈0, invisible) instead ([`alpha_bits`]).
//!   - bits 16..=23 — the **ground-shade byte** (`0` = lit, `255` = fully MCSH-shadowed): the
//!     per-instance mix from the batch's lit sun level toward the shaded one (`wow_model.wgsl`).
//!     Entities (units/players/GameObjects) ramp it per frame ([`crate::entity_shade`]); statics
//!     (doodads/props) leave it `0` — their shade is the per-material selector (`sun_scale.x`).
//!   - bits 24..=30 — reserved (0).
//! - **Interior probe slot** (interior M2 props/entities — material in interior mode,
//!   `model_flags.z` set, not a WMO): bits 16..=29 carry the SH-probe TABLE SLOT (see
//!   [`crate::lighting::PropProbes`]; 8192 slots < 2¹⁴), and bits 0..=15 stay the **fade alpha**,
//!   exactly like the exterior payload — so a fade (the self-avatar zoom feather, a despawn ramp)
//!   composes with the slot through [`with_alpha`] instead of clobbering it (the pre-0355 layout
//!   put the slot in the alpha bits: a feathering indoor character lost its probe AND read shade
//!   byte 0 = the lit exterior intensity — the director's "light jumps outdoor when I zoom in").
//!   Slot 0 is a valid payload: [`probe_bits`] always carries a non-zero alpha field, so the
//!   shader's untagged-⇒-opaque `0` sentinel never fires for these.
//!
//! **Who writes it** — five systems share the channel, deconflicted *by design*, not by luck:
//!
//! 1. `model_fade::apply_render_fade` (appear/despawn ramps) owns an entity's **alpha field** (and
//!    material) while a `RenderFade` lives on it; the other alpha writers filter those entities out
//!    (`Without<RenderFade>`, `Without<PendingAppearFade>`). It writes through [`with_alpha`], so
//!    the shade field survives a fade.
//! 2. `interior::classify_entity_interior` is the steady-state owner for interior-capable parts
//!    (`InteriorLit` holders) — and of [`INTERIOR_FOG_BIT`]: probe slot + fog bit on the Bake law,
//!    fog bit alone on the Matte law, plain `alpha_bits(1.0)` outdoors (dropping the shade byte —
//!    the shade writer below re-asserts it the same frame, ordered after).
//! 3. `debug_panel::apply_model_visibility` drives the small-prop distance fade (`DoodadFade`
//!    holders) + glow-card dimming; `DoodadFade` is **never attached** to a lit interior prop
//!    (spawn-time exclusion in `terrain_stream`), so 2 and 3 are disjoint.
//! 4. `player::apply_self_model_fade` (the zoom-to-first-person feather) runs **after** 1–3 and wins
//!    on the self body submeshes' alpha while feathering; at α ≥ 1 it yields the channel back to 2.
//! 5. `entity_shade::update_ground_shade` owns the **shade field** on entity M2 parts (decision
//!    0173): read-modify-write via [`with_shade`], never touching alpha — so it composes with 1–4
//!    instead of racing them. It skips interior-classified parts (their payload is a colour) and
//!    runs after 2 to re-assert the byte over 2's exterior reclaim.
//!
//! A further *payload* writer (stealth, ghost form, …) should claim reserved bits through a typed
//! accessor here — never a new ad-hoc whole-payload convention (decision 0066's rule, upheld by
//! 0173's layout).
//!
//! **Bit 31 has its own single writer**, deconflicted by *schedule*, not by slot:
//! `target::highlight::apply_highlight` (PostUpdate) ORs/clears it on the hovered/targeted roots'
//! parts every frame. The payload writers above all run in Update and re-derive the payload bits
//! (dropping the flag); running after them re-asserts it the same frame, so they never need to know
//! it exists.

/// Bit 31 of the `MeshTag`: the hover/target **model-brighten** flag (the real client's
/// per-model highlight emissive — `SetHighlight 0x614550` writing the config RGB into the CM2;
/// wow-re `object-layer/scratch/selection-circle.md` PART 2). The shader adds the emissive lift
/// to the lighting sum when set and masks the bit off before decoding the payload.
pub(crate) const HIGHLIGHT_BIT: u32 = 0x8000_0000;

/// Bit 30 of the `MeshTag`: the **interior-fog** flag — fog this instance with the interior
/// triple (shared-light rows 18-19, the camera-crossfaded MFOG) instead of the scene fog. The
/// entity classifier (`crate::interior`) owns it: set on both indoor laws (Bake via
/// [`probe_bits`], Matte by OR), cleared on the exterior law's payload reset. At camera-out the
/// interior triple EQUALS the scene triple (the reference's t=0 lerp), so the bit only ever
/// diverges the fog while the camera is inside a fogged WMO — exactly the byte semantics
/// (wow-re `m2-unit-interior-fog.md`).
pub(crate) const INTERIOR_FOG_BIT: u32 = 0x4000_0000;

/// Bits 0..=15 of BOTH payload modes: the fade alpha as u16.
const ALPHA_MASK: u32 = 0x0000_ffff;
/// Bits 16..=23 of the exterior payload: the ground-shade byte.
const SHADE_MASK: u32 = 0x00ff_0000;
const SHADE_SHIFT: u32 = 16;

/// An interior-probe payload: the slot in bits 16..=29 + an opaque alpha field + the
/// [`INTERIOR_FOG_BIT`] (a probe payload always means the model stands indoors). A fade writer
/// composes through [`with_alpha`], which preserves bits 16..=31 — the slot rides through a
/// feather. `wow_model.wgsl` reads the slot as `(tag >> 16) & 0x3fff` on interior-mode materials.
/// The ONE constructor for BOTH probe-payload writers — the static-prop spawner
/// (`terrain_stream::spawn`) and the entity classifier (`interior`): the 0355 re-lane moved the
/// slot and updated only the classifier; the spawner kept the old bits-0..=15 write, so every
/// static interior prop decoded slot 0 and lit from whichever probe won the streaming race.
pub(crate) fn probe_bits(slot: u16) -> u32 {
    INTERIOR_FOG_BIT | (u32::from(slot) << SHADE_SHIFT) | alpha_bits(1.0)
}

/// A fade alpha as `MeshTag` bits: u16 in the low half, shade byte zero. Handles the shader's
/// `0`-sentinel: `MeshTag == 0` means *untagged ⇒ opaque 1.0* in `wow_model.wgsl`, so a true zero
/// (or negative) alpha returns `1` (≈1/65535, invisible) instead of an accidentally-opaque `0`.
pub(crate) fn alpha_bits(alpha: f32) -> u32 {
    if alpha <= 0.0 {
        1u32
    } else {
        ((alpha.min(1.0) * 65535.0).round() as u32).max(1)
    }
}

/// Write the alpha field of an exterior-payload tag, preserving the shade byte and the highlight
/// bit — the fade writers' read-modify-write (an appear-fade on a unit standing in MCSH shadow must
/// not flash it lit).
pub(crate) fn with_alpha(tag: u32, alpha: f32) -> u32 {
    (tag & !ALPHA_MASK) | alpha_bits(alpha)
}

/// Write the shade byte of an exterior-payload tag (`0` = lit … `255` = fully MCSH-shadowed),
/// preserving the alpha field and the highlight bit. If the alpha field reads `0`, the tag was the
/// whole-payload-`0` *untagged ⇒ opaque* sentinel (the field is never legitimately `0` —
/// [`alpha_bits`] floors at `1`), so materialize it as opaque — otherwise a non-zero shade byte
/// would defeat the sentinel and the instance would decode alpha 0 (invisible).
pub(crate) fn with_shade(tag: u32, shade: u8) -> u32 {
    let alpha = match tag & ALPHA_MASK {
        0 => ALPHA_MASK,
        a => a,
    };
    (tag & !(ALPHA_MASK | SHADE_MASK)) | alpha | (u32::from(shade) << SHADE_SHIFT)
}

/// Read back the shade byte of an exterior-payload tag (the shade writer's change gate).
pub(crate) fn shade_of(tag: u32) -> u8 {
    ((tag & SHADE_MASK) >> SHADE_SHIFT) as u8
}

/// Human-readable decode of a `MeshTag`, for the probes (`WOW_PICK`'s per-frame shading dump).
///
/// It lives **here** because this module owns the bit conventions: a probe that re-derived the masks
/// would silently drift the day a field moves — which is precisely how 0355 broke (the slot moved and
/// one of its two writers kept the old bits). The masks stay private; this is the read-out.
///
/// The payload's meaning switches on **material state**, which a tag alone cannot know, so BOTH
/// readings of bits 16..=29 are printed side by side: `shade` is the exterior law's byte, `slot` the
/// interior law's probe index. A writer using the wrong law for its material is invisible in either
/// reading alone and obvious in the pair — a shade byte of 255 and a slot of 255 are the same bits.
pub(crate) fn describe(tag: u32) -> String {
    if tag == 0 {
        return "0 (untagged ⇒ opaque)".to_string();
    }
    let flags = match (tag & HIGHLIGHT_BIT != 0, tag & INTERIOR_FOG_BIT != 0) {
        (true, true) => " hi+fog",
        (true, false) => " hi",
        (false, true) => " fog",
        (false, false) => "",
    };
    format!(
        "{tag:#010x}{flags} α {:.4} shade {} / slot {}",
        (tag & ALPHA_MASK) as f32 / 65535.0,
        shade_of(tag),
        (tag >> SHADE_SHIFT) & 0x3fff,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_prints_both_readings_of_the_shared_bits() {
        // The 0-sentinel is called out by name rather than decoded as "α 0.0000" (invisible), which
        // is the one thing it never means.
        assert!(describe(0).contains("untagged"));
        // Exterior law: the shade byte. Interior law: the same bits as a slot. Both, always.
        let t = with_shade(alpha_bits(1.0), 255);
        assert!(describe(t).contains("shade 255"), "{}", describe(t));
        assert!(describe(t).contains("slot 255"), "{}", describe(t));
        // A probe payload reads back its slot, and carries the fog flag it bakes in.
        let t = probe_bits(6660);
        assert!(describe(t).contains("slot 6660"), "{}", describe(t));
        assert!(describe(t).contains("fog"), "{}", describe(t));
        // Both standalone flags are named, and neither is mistaken for payload.
        assert!(describe(HIGHLIGHT_BIT | alpha_bits(0.5)).contains("hi"));
        assert!(describe(HIGHLIGHT_BIT | INTERIOR_FOG_BIT | 1).contains("hi+fog"));
    }

    #[test]
    fn highlight_bit_is_orthogonal_to_both_payloads() {
        // Neither payload mode can ever set bit 31, so OR-ing/masking the flag is lossless.
        for a in [0.0, f32::MIN_POSITIVE, 0.25, 0.5, 1.0] {
            assert_eq!(alpha_bits(a) & HIGHLIGHT_BIT, 0);
        }
        assert_eq!(with_shade(alpha_bits(1.0), 255) & HIGHLIGHT_BIT, 0);
        // The interior payload is a u16 probe slot — structurally below bit 31.
        // Both field writers preserve an already-set flag.
        assert_eq!(
            with_alpha(HIGHLIGHT_BIT | 0x00ff_ffff, 0.5) & HIGHLIGHT_BIT,
            HIGHLIGHT_BIT
        );
        assert_eq!(
            with_shade(HIGHLIGHT_BIT | 0xffff, 7) & HIGHLIGHT_BIT,
            HIGHLIGHT_BIT
        );
    }

    #[test]
    fn alpha_bits_never_hits_the_opaque_sentinel() {
        assert_eq!(alpha_bits(0.0), 1);
        assert_eq!(alpha_bits(-0.5), 1);
        assert_ne!(alpha_bits(f32::MIN_POSITIVE), 0);
        assert_eq!(alpha_bits(1.0), 0xffff);
        // Full alpha and the shade byte occupy disjoint fields.
        assert_eq!(alpha_bits(1.0) & SHADE_MASK, 0);
    }

    #[test]
    fn alpha_and_shade_fields_compose() {
        // Round-trip: shade survives an alpha write, alpha survives a shade write.
        let t = with_shade(alpha_bits(1.0), 200);
        assert_eq!(shade_of(t), 200);
        let t = with_alpha(t, 0.25);
        assert_eq!(shade_of(t), 200);
        assert_eq!(t & ALPHA_MASK, alpha_bits(0.25));
        let t = with_shade(t, 10);
        assert_eq!(t & ALPHA_MASK, alpha_bits(0.25));
        assert_eq!(shade_of(t), 10);
    }

    #[test]
    fn probe_bits_compose_with_the_alpha_field() {
        // The slot rides bits 16..=29; a fade write must preserve it (the zoom-feather bug).
        let t = probe_bits(6660);
        assert_eq!((t >> 16) & 0x3fff, 6660);
        assert_eq!(t & ALPHA_MASK, 0xffff); // opaque by default — the 0-sentinel can't fire
        let t = with_alpha(t, 0.25);
        assert_eq!((t >> 16) & 0x3fff, 6660); // slot survives the feather
        assert_eq!(t & ALPHA_MASK, alpha_bits(0.25));
        assert_eq!(t & HIGHLIGHT_BIT, 0);
        // The max slot (8191) stays inside bits 16..=29: the highlight bit stays clear, and the
        // interior-fog flag is BAKED IN (a probe payload always fogs interior).
        assert_eq!(probe_bits(8191) & HIGHLIGHT_BIT, 0);
        assert_eq!(probe_bits(8191) & INTERIOR_FOG_BIT, INTERIOR_FOG_BIT);
    }

    #[test]
    fn interior_fog_bit_survives_the_field_writers() {
        // The bit is owned by the classifier's whole-payload writes; the alpha/shade
        // read-modify-writes must carry it (a feathering or MCSH-ramping indoor unit keeps
        // its room fog).
        let t = INTERIOR_FOG_BIT | alpha_bits(1.0);
        assert_eq!(with_alpha(t, 0.25) & INTERIOR_FOG_BIT, INTERIOR_FOG_BIT);
        assert_eq!(with_shade(t, 191) & INTERIOR_FOG_BIT, INTERIOR_FOG_BIT);
        // And it never leaks into the payload fields it rides above.
        assert_eq!(shade_of(t), 0);
        assert_eq!(t & ALPHA_MASK, 0xffff);
        // A probe payload keeps its slot decode with the flag set.
        assert_eq!((probe_bits(6660) >> 16) & 0x3fff, 6660);
    }

    #[test]
    fn with_shade_materializes_the_untagged_sentinel_as_opaque() {
        // Shading an untagged (payload 0) instance must not defeat the "0 ⇒ opaque" rule by making
        // the payload non-zero with a zero alpha field.
        let t = with_shade(0, 128);
        assert_eq!(t & ALPHA_MASK, 0xffff);
        assert_eq!(shade_of(t), 128);
        // Same through the highlight bit (payload still reads 0 under the mask).
        let t = with_shade(HIGHLIGHT_BIT, 128);
        assert_eq!(t & ALPHA_MASK, 0xffff);
    }
}
