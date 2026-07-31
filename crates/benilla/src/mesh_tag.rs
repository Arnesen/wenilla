//! The per-instance `MeshTag` channel: **one home for its bit conventions and ownership protocol**
//! (decisions 0066, 0173 — the typed field layout; 0720 — the rig field + the 6-bit alpha).
//!
//! Every `WowModelMaterial` submesh carries a Bevy `MeshTag` (a raw `u32` the shader reads per
//! instance). **Bits 31 and 30 are standalone flags; bits 0..=29 are the payload.** The **rig
//! field** (bits 19..=29) and the **alpha field** (bits 0..=5) mean the same thing in every mode;
//! the bits between them switch meaning on **material state** — so per entity each bit has exactly
//! one meaning at a time:
//!
//! - **Highlight** ([`HIGHLIGHT_BIT`], bit 31): the hover/target model-brighten flag — the shader
//!   adds the client's emissive lift to the lighting sum when set. Orthogonal by construction:
//!   no payload mode ever sets bit 31, and the shader masks it off before reading the payload —
//!   so the untagged-⇒-opaque `0` sentinel still works on the masked value.
//! - **Interior fog** ([`INTERIOR_FOG_BIT`], bit 30): this instance's model stands in a WMO
//!   interior, so the shader fogs it with the INTERIOR triple (shared-light rows 18-19) instead of
//!   the scene fog — the reference stages a unit's fog by the unit's OWN interior classification
//!   (`0x71c110` → collector `+0x184..`, lane by `[node+0xc]`; wow-re `m2-unit-interior-fog.md`).
//!   Carried by BOTH indoor laws: [`probe_bits`] bakes it in, and the entity classifier ORs it
//!   onto the Matte (day/night) payload. Masked off with bit 31 before the payload decode.
//! - **Instance slot** (bits 19..=29, BOTH payload modes — decisions 0720, 0812, 0820): the
//!   per-instance index into the shared buffer's slot-keyed regions. `0` is the world's shared
//!   no-instance sentinel (terrain, WMO, doodads, clutter, and every part of a rig-less model). Two
//!   consumers, reading it in different stages — which is why it is no longer called "the rig slot":
//!     - the **vertex** stage indexes the skin-palette region with it ([`crate::rig_palette`], 0720)
//!       — but only under `WOW_RIG_SKIN`, which `WowModelExt::specialize` compiles from the **mesh's
//!       own joint attributes**, never from this field. A static mesh carrying a slot is not skinned.
//!     - the **fragment** stage indexes the per-instance body-tint table with it
//!       ([`crate::instance_tint`], 0812) — for every part, skinned or not.
//!
//!   That asymmetry is why the field is written from the **unit** and not from the part: a unit's
//!   boneless geosets, its helm and shoulders, its held items and its billboard cards all carry
//!   their wearer's slot, so a tinted unit tints *whole* — the reference's own rule (an
//!   attached/chained model inherits the parent CM2's computed colours, `0x714000`). Before 0820 the
//!   field was skinning-only, and a dwarf's Stoneform tint stopped at his pauldrons.
//!
//!   Written ONCE at part spawn ([`rig_bits`]); every runtime writer below preserves it by
//!   construction (the `with_*` accessors carry bits the writer doesn't own). 11 bits ⇔
//!   [`MAX_RIG_SLOTS`] concurrent instances.
//! - **Alpha** (bits 0..=5, BOTH payload modes): the fade alpha as a 6-bit fraction (`63` =
//!   opaque), multiplying the cutout alpha. 64 steps is past perceptual for the sub-second fade
//!   ramps that write it (it was u16 pre-0720 — the rig field bought its bits from here). A whole
//!   payload of `0` is the shader's *untagged ⇒ opaque* sentinel, so the alpha field is never
//!   legitimately `0` — a true zero alpha writes `1` (≈0, invisible) instead ([`alpha_bits`]).
//! - **Exterior payload** (the default): bits 6..=13 carry the **ground-shade byte** (`0` = lit,
//!   `255` = fully MCSH-shadowed): the per-instance mix from the batch's lit sun level toward the
//!   shaded one (`wow_model.wgsl`). Entities (units/players/GameObjects) ramp it per frame
//!   ([`crate::entity_shade`]); statics (doodads/props) leave it `0` — their shade is the
//!   per-material selector (`sun_scale.x`). Bits 14..=18 are reserved (0).
//! - **Interior probe slot** (interior M2 props/entities — material in interior mode,
//!   `model_flags.z` set, not a WMO): bits 6..=18 carry the SH-probe TABLE SLOT (see
//!   [`crate::lighting::PropProbes`]; 8192 slots = 13 bits), alpha and rig ride their fixed
//!   fields — so a fade (the self-avatar zoom feather, a despawn ramp) and the skin palette both
//!   compose with the slot through the accessors instead of clobbering it (the pre-0355 layout
//!   put the slot in the alpha bits: a feathering indoor character lost its probe AND read shade
//!   byte 0 = the lit exterior intensity — the director's "light jumps outdoor when I zoom in").
//!   Slot 0 is a valid payload: [`probe_bits`] always carries a non-zero alpha field, so the
//!   shader's untagged-⇒-opaque `0` sentinel never fires for these.
//!
//! **Who writes it** — five systems share the channel, deconflicted *by design*, not by luck:
//!
//! 1. `model_fade::apply_render_fade` (appear/despawn ramps) owns an entity's **alpha field**, and
//!    its **material**, while a `RenderFade` lives on it; the other *alpha* writers filter those
//!    entities out (`Without<RenderFade>`, `Without<PendingAppearFade>`). It writes through
//!    [`with_alpha`], so the shade, probe-slot and rig fields survive a fade.
//! 2. `interior::classify_entity_interior` is the owner of the **light law** for interior-capable
//!    parts (`InteriorLit` holders) — the payload's non-alpha fields and [`INTERIOR_FOG_BIT`]:
//!    probe slot + fog bit on the Bake law, fog bit alone on the Matte law, plain payload outdoors
//!    (dropping the shade byte — the shade writer below re-asserts it the same frame, ordered
//!    after). Its whole-payload writes go through [`with_interior_probe`] /
//!    [`with_exterior_reset`], which carry the rig **and alpha** fields.
//!
//!    It is **not** filtered by fade state (decision 0755): the light law and the fade alpha are
//!    orthogonal, so 1 and 2 are deconflicted by *field*, not by lockout — the law follows a part
//!    through its appear/despawn ramp, and only the *material* defers to 1 while a fade is live
//!    (the fade picks the blend twin **of the law's family**, `FadeMaterials::material_for`). The
//!    old lockout is what made a streamed indoor unit ramp in under exterior light and then swap
//!    laws in a single frame the instant the ramp latched.
//! 3. `debug_panel::apply_model_visibility` drives the small-prop distance fade (`DoodadFade`
//!    holders) + glow-card dimming; `DoodadFade` is **never attached** to a lit interior prop
//!    (spawn-time exclusion in `terrain_stream`), so 2 and 3 are disjoint.
//! 4. `player::apply_self_model_fade` (the zoom-to-first-person feather) runs **after** 1–3 and wins
//!    on the self body submeshes' alpha while feathering; on the frame it ends it restores the
//!    alpha field itself and hands the material back to the part's law (it cannot lean on 2 to
//!    re-opaque it — since 0755 that writer carries alpha through rather than forcing it).
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

/// Bits 0..=5 of BOTH payload modes: the fade alpha as a 6-bit fraction (`63` = opaque).
const ALPHA_MASK: u32 = 0x0000_003f;
const ALPHA_MAX: f32 = 63.0;
/// Bits 6..=13 of the exterior payload: the ground-shade byte.
const SHADE_MASK: u32 = 0x0000_3fc0;
const SHADE_SHIFT: u32 = 6;
/// Bits 6..=18 of the interior payload: the SH-probe table slot (13 bits ⇔ 8192 slots).
const PROBE_MASK: u32 = 0x0007_ffc0;
const PROBE_SHIFT: u32 = 6;
/// Bits 19..=29 of BOTH payload modes: the skin-palette rig slot (decision 0720); `0` = no rig.
const RIG_MASK: u32 = 0x3ff8_0000;
const RIG_SHIFT: u32 = 19;
/// Rig slots addressable by the tag's 11-bit rig field (slot 0 = the "no rig" sentinel). The
/// palette allocator (`crate::rig_palette`) sizes itself to this.
pub(crate) const MAX_RIG_SLOTS: usize = 1 << 11;

/// An interior-probe payload constructor for RIG-LESS spawns (the static-prop spawner): the slot
/// in bits 6..=18 + an opaque alpha field + the [`INTERIOR_FOG_BIT`] (a probe payload always
/// means the model stands indoors). Rig-carrying writers (the entity classifier) go through
/// [`with_interior_probe`] instead, which preserves the rig and alpha fields.
pub(crate) fn probe_bits(slot: u16) -> u32 {
    INTERIOR_FOG_BIT | (u32::from(slot) << PROBE_SHIFT) | alpha_bits(1.0)
}

/// The alpha field a whole-payload rewrite carries through, with the whole-payload-`0`
/// *untagged ⇒ opaque* sentinel materialized as opaque (the field is never legitimately `0` —
/// [`alpha_bits`] floors at `1`, so a `0` here can only be the sentinel).
///
/// This is what lets the interior classifier **re-lane a part while a fade owns its alpha**: the
/// law rewrites the payload, the ramp's alpha rides through untouched (decision 0755). Before it,
/// the classifier's payload writes hardcoded opaque, which is why it had to be locked out of
/// fading parts entirely — and why a freshly-streamed indoor entity spent its whole 2 s appear
/// ramp on the exterior lane and swapped light laws in one frame when the ramp latched.
fn carried_alpha(tag: u32) -> u32 {
    match tag & ALPHA_MASK {
        0 => ALPHA_MASK,
        a => a,
    }
}

/// Rewrite a tag as an interior-probe payload, preserving the rig and alpha fields (and nothing
/// else — the classifier owns the rest of the payload when it fires): the entity classifier's
/// Bake-law write for skinned unit parts, whose rig slot must ride through the indoor/outdoor
/// transitions and whose fade alpha must ride through a mid-ramp re-lane.
pub(crate) fn with_interior_probe(tag: u32, slot: u16) -> u32 {
    INTERIOR_FOG_BIT | (tag & RIG_MASK) | (u32::from(slot) << PROBE_SHIFT) | carried_alpha(tag)
}

/// Rewrite a tag as a fresh exterior payload, preserving the rig and alpha fields: the
/// classifier's outdoor reclaim (the shade writer re-asserts the shade byte the same frame,
/// ordered after).
pub(crate) fn with_exterior_reset(tag: u32) -> u32 {
    (tag & RIG_MASK) | carried_alpha(tag)
}

/// The rig field of a part's spawn tag (decision 0720): the skin-palette rig slot, written once
/// when the skinned part spawns (composed with [`alpha_bits`]`(1.0)` or [`probe_bits`]). Every
/// runtime writer preserves it. Slot 0 is the "no rig" value — [`crate::rig_palette`] never
/// allocates it.
pub(crate) fn rig_bits(slot: u16) -> u32 {
    debug_assert!((slot as usize) < MAX_RIG_SLOTS);
    u32::from(slot) << RIG_SHIFT
}

/// Read back a tag's rig field (`0` = not skinned).
pub(crate) fn rig_of(tag: u32) -> u16 {
    ((tag & RIG_MASK) >> RIG_SHIFT) as u16
}

/// A fade alpha as `MeshTag` bits: 6-bit fraction in the low field, everything else zero. Handles
/// the shader's `0`-sentinel: `MeshTag == 0` means *untagged ⇒ opaque 1.0* in `wow_model.wgsl`,
/// so a true zero (or negative) alpha returns `1` (≈1/63, near-invisible) instead of an
/// accidentally-opaque `0`.
pub(crate) fn alpha_bits(alpha: f32) -> u32 {
    if alpha <= 0.0 {
        1u32
    } else {
        ((alpha.min(1.0) * ALPHA_MAX).round() as u32).max(1)
    }
}

/// Write the alpha field of a tag, preserving every other field (shade or probe slot, rig, both
/// flag bits) — the fade writers' read-modify-write (an appear-fade on a unit standing in MCSH
/// shadow must not flash it lit, and a feathering skinned part must keep its palette).
pub(crate) fn with_alpha(tag: u32, alpha: f32) -> u32 {
    (tag & !ALPHA_MASK) | alpha_bits(alpha)
}

/// Write the shade byte of an exterior-payload tag (`0` = lit … `255` = fully MCSH-shadowed),
/// preserving the alpha and rig fields and the flag bits. If the alpha field reads `0`, the tag
/// was the whole-payload-`0` *untagged ⇒ opaque* sentinel (the field is never legitimately `0` —
/// [`alpha_bits`] floors at `1`), so materialize it as opaque — otherwise a non-zero shade byte
/// would defeat the sentinel and the instance would decode alpha 0 (invisible).
pub(crate) fn with_shade(tag: u32, shade: u8) -> u32 {
    (tag & !(ALPHA_MASK | SHADE_MASK)) | carried_alpha(tag) | (u32::from(shade) << SHADE_SHIFT)
}

/// Read back the alpha field of a tag as a fraction — the decoder twin of [`alpha_bits`], for a
/// writer's change gate and for the tests that assert what actually reached an instance. A whole
/// payload of `0` is the shader's *untagged ⇒ opaque* sentinel, so it reads `1.0`.
pub(crate) fn alpha_of(tag: u32) -> f32 {
    if tag == 0 {
        return 1.0;
    }
    (tag & ALPHA_MASK) as f32 / ALPHA_MAX
}

/// Read back the shade byte of an exterior-payload tag (the shade writer's change gate).
pub(crate) fn shade_of(tag: u32) -> u8 {
    ((tag & SHADE_MASK) >> SHADE_SHIFT) as u8
}

/// Whether this tag's instance draws **translucent** (`0 < α < 1`) — the depth-prime twin's
/// activation rule ([`crate::zfill`], decision 0831), decoding exactly as the shader does: the
/// flag bits split off first, a whole payload of `0` is the *untagged ⇒ opaque* sentinel, and a
/// full alpha field (63) is opaque. The field is never legitimately `0` ([`alpha_bits`] floors at
/// `1`), so there is no "invisible" branch to exclude — bits `1..=62` are the whole translucent
/// band, matching the reference's twin gate (any batch whose evaluated `A < 0.99999` routes to the
/// transparent lists and clones a twin; only `A ≤ 0` culls the batch outright).
pub(crate) fn translucent(tag: u32) -> bool {
    let payload = tag & !(HIGHLIGHT_BIT | INTERIOR_FOG_BIT);
    payload != 0 && matches!(payload & ALPHA_MASK, 1..=62)
}

/// Human-readable decode of a `MeshTag`, for the probes (`WOW_PICK`'s per-frame shading dump).
///
/// It lives **here** because this module owns the bit conventions: a probe that re-derived the masks
/// would silently drift the day a field moves — which is precisely how 0355 broke (the slot moved and
/// one of its two writers kept the old bits). The masks stay private; this is the read-out.
///
/// The payload's meaning switches on **material state**, which a tag alone cannot know, so BOTH
/// readings of bits 6..=18 are printed side by side: `shade` is the exterior law's byte, `slot` the
/// interior law's probe index. A writer using the wrong law for its material is invisible in either
/// reading alone and obvious in the pair — a shade byte of 255 sits inside a slot of 2047.
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
        "{tag:#010x}{flags} α {:.3} shade {} / slot {} rig {}",
        alpha_of(tag),
        shade_of(tag),
        (tag & PROBE_MASK) >> PROBE_SHIFT,
        rig_of(tag),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_prints_both_readings_of_the_shared_bits() {
        // The 0-sentinel is called out by name rather than decoded as "α 0.000" (invisible), which
        // is the one thing it never means.
        assert!(describe(0).contains("untagged"));
        // Exterior law: the shade byte. Interior law: the same bits inside the slot. Both, always.
        let t = with_shade(alpha_bits(1.0), 255);
        assert!(describe(t).contains("shade 255"), "{}", describe(t));
        assert!(describe(t).contains("slot 255"), "{}", describe(t));
        // A probe payload reads back its slot, and carries the fog flag it bakes in.
        let t = probe_bits(6660);
        assert!(describe(t).contains("slot 6660"), "{}", describe(t));
        assert!(describe(t).contains("fog"), "{}", describe(t));
        // The rig field is printed from its own bits.
        let t = rig_bits(1234) | alpha_bits(1.0);
        assert!(describe(t).contains("rig 1234"), "{}", describe(t));
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
        assert_eq!(probe_bits(8191) & HIGHLIGHT_BIT, 0);
        assert_eq!(rig_bits(2047) & HIGHLIGHT_BIT, 0);
        // Both field writers preserve an already-set flag.
        assert_eq!(
            with_alpha(HIGHLIGHT_BIT | 0x3fff_ffff, 0.5) & HIGHLIGHT_BIT,
            HIGHLIGHT_BIT
        );
        assert_eq!(
            with_shade(HIGHLIGHT_BIT | 0x3f, 7) & HIGHLIGHT_BIT,
            HIGHLIGHT_BIT
        );
    }

    #[test]
    fn alpha_bits_never_hits_the_opaque_sentinel() {
        assert_eq!(alpha_bits(0.0), 1);
        assert_eq!(alpha_bits(-0.5), 1);
        assert_ne!(alpha_bits(f32::MIN_POSITIVE), 0);
        assert_eq!(alpha_bits(1.0), ALPHA_MASK);
        // Full alpha stays inside its field — disjoint from shade, probe, and rig.
        assert_eq!(alpha_bits(1.0) & (SHADE_MASK | PROBE_MASK | RIG_MASK), 0);
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
        // The slot rides bits 6..=18; a fade write must preserve it (the zoom-feather bug).
        let t = probe_bits(6660);
        assert_eq!((t & PROBE_MASK) >> PROBE_SHIFT, 6660);
        assert_eq!(t & ALPHA_MASK, ALPHA_MASK); // opaque by default — the 0-sentinel can't fire
        let t = with_alpha(t, 0.25);
        assert_eq!((t & PROBE_MASK) >> PROBE_SHIFT, 6660); // slot survives the feather
        assert_eq!(t & ALPHA_MASK, alpha_bits(0.25));
        assert_eq!(t & HIGHLIGHT_BIT, 0);
        // The max slot (8191) stays inside bits 6..=18: the rig field stays clear, and the
        // interior-fog flag is BAKED IN (a probe payload always fogs interior).
        assert_eq!(probe_bits(8191) & RIG_MASK, 0);
        assert_eq!(probe_bits(8191) & INTERIOR_FOG_BIT, INTERIOR_FOG_BIT);
    }

    #[test]
    fn rig_field_survives_every_runtime_writer() {
        // The rig slot is written once at spawn; every steady-state writer must carry it.
        let spawn = rig_bits(1000) | alpha_bits(1.0);
        assert_eq!(rig_of(spawn), 1000);
        assert_eq!(rig_of(with_alpha(spawn, 0.3)), 1000); // fades (writers 1, 3, 4)
        assert_eq!(rig_of(with_shade(spawn, 200)), 1000); // the ground-shade ramp (writer 5)
        let indoor = with_interior_probe(spawn, 4321); // the classifier's Bake law (writer 2)
        assert_eq!(rig_of(indoor), 1000);
        assert_eq!((indoor & PROBE_MASK) >> PROBE_SHIFT, 4321);
        assert_eq!(indoor & INTERIOR_FOG_BIT, INTERIOR_FOG_BIT);
        let outdoor = with_exterior_reset(indoor); // …and its outdoor reclaim
        assert_eq!(rig_of(outdoor), 1000);
        assert_eq!(outdoor & (PROBE_MASK | INTERIOR_FOG_BIT), 0);
        assert_eq!(outdoor & ALPHA_MASK, ALPHA_MASK);
        // Alpha and probe compose under the rig field exactly as they did without it.
        let t = with_alpha(indoor, 0.5);
        assert_eq!(rig_of(t), 1000);
        assert_eq!((t & PROBE_MASK) >> PROBE_SHIFT, 4321);
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
        assert_eq!(t & ALPHA_MASK, ALPHA_MASK);
        // A probe payload keeps its slot decode with the flag set.
        assert_eq!((probe_bits(6660) & PROBE_MASK) >> PROBE_SHIFT, 6660);
    }

    /// Decision 0755: the classifier's whole-payload rewrites carry the tag's ALPHA field, which
    /// is what lets a part change light law while a fade owns its ramp. Before this they wrote
    /// `alpha_bits(1.0)`, so the classifier had to be locked out of fading parts entirely — and a
    /// streamed indoor entity therefore had no interior law until its 2 s appear ramp latched.
    #[test]
    fn a_law_rewrite_carries_the_fade_alpha() {
        let mid_ramp = alpha_bits(0.25);
        // Into the interior lane and back out again, at a quarter alpha the whole way.
        let indoor = with_interior_probe(rig_bits(7) | mid_ramp, 1234);
        assert_eq!(
            indoor & ALPHA_MASK,
            mid_ramp,
            "the ramp survives the re-lane"
        );
        assert_eq!((indoor & PROBE_MASK) >> PROBE_SHIFT, 1234);
        assert_eq!(rig_of(indoor), 7);
        assert_eq!(indoor & INTERIOR_FOG_BIT, INTERIOR_FOG_BIT);
        let outdoor = with_exterior_reset(indoor);
        assert_eq!(outdoor & ALPHA_MASK, mid_ramp, "and the reclaim too");
        assert_eq!(rig_of(outdoor), 7);
        assert_eq!(outdoor & (PROBE_MASK | INTERIOR_FOG_BIT), 0);
        // A settled (opaque) part is unaffected — the steady state still reads exactly as before.
        assert_eq!(
            with_interior_probe(alpha_bits(1.0), 1234),
            probe_bits(1234),
            "an opaque part's Bake payload is unchanged"
        );
        assert_eq!(with_exterior_reset(probe_bits(1234)), alpha_bits(1.0));
        // The untagged-⇒-opaque sentinel is materialized, never propagated as alpha 0 (invisible).
        assert_eq!(with_interior_probe(0, 1234) & ALPHA_MASK, ALPHA_MASK);
        assert_eq!(with_exterior_reset(0) & ALPHA_MASK, ALPHA_MASK);
    }

    #[test]
    fn with_shade_materializes_the_untagged_sentinel_as_opaque() {
        // Shading an untagged (payload 0) instance must not defeat the "0 ⇒ opaque" rule by making
        // the payload non-zero with a zero alpha field.
        let t = with_shade(0, 128);
        assert_eq!(t & ALPHA_MASK, ALPHA_MASK);
        assert_eq!(shade_of(t), 128);
        // Same through the highlight bit (payload still reads 0 under the mask).
        let t = with_shade(HIGHLIGHT_BIT, 128);
        assert_eq!(t & ALPHA_MASK, ALPHA_MASK);
    }
}
