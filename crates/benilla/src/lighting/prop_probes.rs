//! The per-instance **interior-prop light-probe table**: one 7-row SH probe per lit interior MODD
//! prop, folded once at spawn ([`super::sh::prop_probe_coeffs`] over the MODD-colour base, the
//! fixed-axis lobe, and the owning group's MOLR point lobes) and uploaded with the shared light
//! blob. The prop's `MeshTag` payload carries its slot index; `wow_model.wgsl`'s interior-prop lane
//! evaluates the probe per fragment. Slots free themselves when the prop despawns (streaming out)
//! via the [`PropProbeSlot`] component hook — no per-frame bookkeeping.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::math::Vec4;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;

/// Capacity of the probe table (mirrored by `wow_model.wgsl`'s `prop_probes` array — keep in sync).
/// Concurrently-resident lit interior props across every streamed placement: the abbey holds ~150
/// interior MODDs, but a CITY-scale WMO streams thousands at once (the Northshire login radius
/// reaches Stormwind's placement — >1024 interior props spawned in one frame, measured live).
/// 8192 × 7 rows = 896 KB — a per-frame `write_buffer` of that size is noise on any target GPU.
pub(crate) const MAX_PROP_PROBES: usize = 8192;

/// A probe's identity for content dedup: the 7 rows, compared/hashed BIT-EXACT (the fold is a pure
/// function of its inputs, so identical props — the same MODD colour with the same lobe set — fold
/// to identical bits; a fence WMO placed 200 times collapses to one slot).
#[derive(Clone, PartialEq, Eq, Hash)]
struct ProbeKey([[u32; 4]; 7]);

impl ProbeKey {
    fn of(coeffs: &[Vec4; 7]) -> Self {
        Self(coeffs.map(|v| v.to_array().map(f32::to_bits)))
    }
}

/// The main-world slot table. `rows` is allocated to capacity once; `free`/`high` implement a slab
/// (freed slots recycle before the high-water mark advances), and identical probes SHARE a slot via
/// [`ProbeKey`] refcounting — without it, a login scene's outdoor prop-WMO armies (every placement ×
/// every MODD of a not-EXTERIOR-flagged group takes the MODD-colour law, faithfully) blew straight
/// through an 8192 table (measured live at Northshire: peak 8192, still overflowing). The rows live
/// behind an `Arc` so the render-world extract is a pointer bump, NOT a ~900 KB copy — probes change
/// on spawn/despawn, not per frame, and [`upload_prop_probes`] writes the GPU region only when
/// [`Self::generation`] moves (`Arc::make_mut` gives copy-on-write for the frame a change races the
/// extract).
#[derive(Resource)]
pub(crate) struct PropProbes {
    rows: Arc<Vec<[[f32; 4]; 7]>>,
    free: Vec<u16>,
    high: usize,
    /// Content dedup: probe bits → live slot. Entries live exactly as long as their refcount.
    by_key: HashMap<ProbeKey, u16>,
    /// Per-slot (refcount, key) — the release path's reverse lookup. `None` = free slot; a live
    /// slot with key `None` is OWNED (a dynamic entity's ramping probe — never deduped/shared).
    slots: Vec<Option<(u32, Option<ProbeKey>)>>,
    /// Bumped on every write — the render-world upload's change detector.
    generation: u64,
    /// Peak concurrent DISTINCT occupancy (diagnostics: logged by the spawner on overflow; a peak
    /// near capacity is the signal to grow the table).
    peak: usize,
}

impl Default for PropProbes {
    fn default() -> Self {
        Self {
            rows: Arc::new(vec![[[0.0; 4]; 7]; MAX_PROP_PROBES]),
            free: Vec::new(),
            high: 0,
            by_key: HashMap::new(),
            slots: vec![None; MAX_PROP_PROBES],
            generation: 0,
            peak: 0,
        }
    }
}

impl PropProbes {
    /// Claim a slot for this probe — a live identical probe just gains a reference; a new one
    /// takes a free slot and writes its rows. `None` when the table is full of DISTINCT probes
    /// (the caller falls back to exterior lighting for that prop and warns — graceful, never
    /// wrong-lit from a stale slot).
    pub(crate) fn alloc(&mut self, coeffs: [Vec4; 7]) -> Option<u16> {
        let key = ProbeKey::of(&coeffs);
        if let Some(&slot) = self.by_key.get(&key) {
            if let Some(Some((refs, _))) = self.slots.get_mut(slot as usize) {
                *refs += 1;
                return Some(slot);
            }
        }
        let slot = self.take_free_slot()?;
        Arc::make_mut(&mut self.rows)[slot as usize] = coeffs.map(|v| v.to_array());
        self.by_key.insert(key.clone(), slot);
        self.slots[slot as usize] = Some((1, Some(key)));
        self.generation += 1;
        self.peak = self.peak.max(self.high - self.free.len());
        Some(slot)
    }

    /// Claim an OWNED slot for a dynamic entity's ramping probe (decision 0354): no content dedup —
    /// two units walking the same room ramp independently, so sharing would couple their light —
    /// and the holder updates it in place via [`Self::update_owned`] while its node ramps/moves.
    /// Freed through the same [`PropProbeSlot`] hook as deduped slots.
    pub(crate) fn alloc_owned(&mut self, coeffs: [Vec4; 7]) -> Option<u16> {
        let slot = self.take_free_slot()?;
        Arc::make_mut(&mut self.rows)[slot as usize] = coeffs.map(|v| v.to_array());
        self.slots[slot as usize] = Some((1, None));
        self.generation += 1;
        self.peak = self.peak.max(self.high - self.free.len());
        Some(slot)
    }

    /// Rewrite an OWNED slot's rows in place (the per-frame refold of a moving/ramping entity).
    /// Refuses a deduped or free slot — an owned holder can never legitimately point at one, and
    /// swallowing the write SILENTLY is how a stale holder stays black forever (the stuck
    /// black-unit-indoors bug rode exactly this no-op), so the refusal warns.
    pub(crate) fn update_owned(&mut self, slot: u16, coeffs: [Vec4; 7]) {
        if !matches!(self.slots.get(slot as usize), Some(Some((_, None)))) {
            warn_once!("update_owned({slot}) on a non-owned slot — the holder's slot is stale");
            return;
        }
        Arc::make_mut(&mut self.rows)[slot as usize] = coeffs.map(|v| v.to_array());
        self.generation += 1;
    }

    fn take_free_slot(&mut self) -> Option<u16> {
        match self.free.pop() {
            Some(s) => Some(s),
            None if self.high < MAX_PROP_PROBES => {
                self.high += 1;
                Some((self.high - 1) as u16)
            }
            None => None,
        }
    }

    /// Live and peak DISTINCT occupancy — the spawner's overflow diagnostic.
    pub(crate) fn occupancy(&self) -> (usize, usize) {
        (self.high - self.free.len(), self.peak)
    }

    /// Drop one reference (frees at zero). Crate-visible for exactly one caller besides the
    /// [`PropProbeSlot`] on-remove hook: the interior classifier's apply-time orphan path — a
    /// slot allocated the same frame its anchor was despawned by the net teardown never gets
    /// its component (whose hook would free it), so the queued command releases it directly.
    pub(crate) fn release(&mut self, slot: u16) {
        let Some(entry) = self.slots.get_mut(slot as usize) else {
            return;
        };
        let Some((refs, _)) = entry else {
            return;
        };
        *refs -= 1;
        if *refs > 0 {
            return;
        }
        let Some((_, key)) = entry.take() else {
            return;
        };
        if let Some(key) = key {
            self.by_key.remove(&key);
        }
        // Zero the freed probe so a stale MeshTag (a frame of despawn skew) reads black, not the
        // previous occupant's light.
        Arc::make_mut(&mut self.rows)[slot as usize] = [[0.0; 4]; 7];
        self.generation += 1;
        self.free.push(slot);
    }
}

/// The render-world mirror of the probe table: an `Arc` bump per frame (never a data copy).
/// [`upload_prop_probes`] writes the shared buffer's probe region — at [`prop_probe_region_offset`],
/// past the per-frame light blob — only when the generation moves.
#[derive(Resource, Clone, ExtractResource)]
pub(crate) struct PropProbeExtract {
    rows: Arc<Vec<[[f32; 4]; 7]>>,
    high: usize,
    generation: u64,
}

impl Default for PropProbeExtract {
    fn default() -> Self {
        Self {
            rows: Arc::new(Vec::new()),
            high: 0,
            // != PropProbes' initial 0, so the first refresh publishes even an empty table.
            generation: u64::MAX,
        }
    }
}

/// Main world, after the spawners: publish the table for extraction when it changed.
pub(super) fn publish_prop_probes(probes: Res<PropProbes>, mut out: ResMut<PropProbeExtract>) {
    if out.generation != probes.generation {
        out.rows = Arc::clone(&probes.rows);
        out.high = probes.high;
        out.generation = probes.generation;
    }
}

/// Byte offset of the probe region inside the shared light buffer (right after the per-frame blob).
pub(crate) fn prop_probe_region_offset() -> u64 {
    super::global_light::per_frame_blob_bytes()
}

/// Render world (`PrepareResources`): write the probe region in place when the table changed. The
/// per-frame `upload_light` writes only the prefix, so the tail persists between changes.
pub(super) fn upload_prop_probes(
    queue: Res<bevy::render::renderer::RenderQueue>,
    buffer: Option<Res<super::SharedLightBuffer>>,
    data: Option<Res<PropProbeExtract>>,
    mut last: Local<Option<u64>>,
) {
    let (Some(buffer), Some(data)) = (buffer, data) else {
        return;
    };
    if *last == Some(data.generation) {
        return;
    }
    *last = Some(data.generation);
    // Upload the whole allocated span (high slots). Freed slots inside it are zeroed rows.
    let rows = &data.rows[..data.high.min(data.rows.len())];
    if !rows.is_empty() {
        queue.write_buffer(
            &buffer.0,
            prop_probe_region_offset(),
            bytemuck::cast_slice(rows),
        );
    }
}

/// Attached to ONE entity of each lit interior prop instance (they despawn together with the
/// placement); the on-remove hook returns the slot to the table whoever despawns it.
#[derive(Component)]
#[component(on_remove = free_prop_probe_slot)]
pub(crate) struct PropProbeSlot(pub(crate) u16);

fn free_prop_probe_slot(mut world: DeferredWorld, ctx: HookContext) {
    let slot = world
        .get::<PropProbeSlot>(ctx.entity)
        .map(|s| s.0)
        .expect("on_remove runs with the component still present");
    world.resource_mut::<PropProbes>().release(slot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_probes_share_one_slot_and_refcount() {
        let mut t = PropProbes::default();
        let c = [Vec4::splat(0.5); 7];
        let a = t.alloc(c).unwrap();
        let b = t.alloc(c).unwrap();
        assert_eq!(a, b); // content dedup: a fence army costs ONE slot
        let other = t.alloc([Vec4::splat(0.25); 7]).unwrap();
        assert_ne!(a, other);
        // First release keeps the shared slot alive; the second frees it.
        t.release(a);
        assert_ne!(t.rows[a as usize], [[0.0; 4]; 7]);
        t.release(b);
        assert_eq!(t.rows[a as usize], [[0.0; 4]; 7]); // freed ⇒ black, not stale light
                                                       // The freed slot recycles, and the same content maps to it afresh.
        let c2 = t.alloc(c).unwrap();
        assert_eq!(c2, a);
        assert_eq!(t.high, 2);
    }

    #[test]
    fn owned_slots_never_dedup_and_update_in_place() {
        let mut t = PropProbes::default();
        let c = [Vec4::splat(0.5); 7];
        let a = t.alloc_owned(c).unwrap();
        let b = t.alloc_owned(c).unwrap();
        assert_ne!(a, b); // identical content still gets its own slot — ramps stay independent
        let g0 = t.generation;
        t.update_owned(a, [Vec4::splat(0.7); 7]);
        assert_eq!(t.rows[a as usize], [[0.7; 4]; 7]);
        assert!(t.generation > g0, "in-place update must republish");
        // A deduped slot ignores update_owned (an owned holder can never point at one).
        let d = t.alloc(c).unwrap();
        let before = t.rows[d as usize];
        t.update_owned(d, [Vec4::splat(0.9); 7]);
        assert_eq!(t.rows[d as usize], before);
        // Owned slots free through the same release path (the component hook) and zero their rows.
        t.release(a);
        assert_eq!(t.rows[a as usize], [[0.0; 4]; 7]);
        // …and the freed slot recycles for the next owned claim.
        assert_eq!(t.alloc_owned(c).unwrap(), a);
    }

    #[test]
    fn alloc_fails_gracefully_at_capacity() {
        let mut t = PropProbes::default();
        for i in 0..MAX_PROP_PROBES {
            let unique = [Vec4::splat(i as f32 + 1.0); 7];
            assert!(t.alloc(unique).is_some());
        }
        assert!(t.alloc([Vec4::splat(-1.0); 7]).is_none());
        // …but a DUPLICATE of a live probe still succeeds at capacity (it costs no slot).
        assert!(t.alloc([Vec4::splat(1.0); 7]).is_some());
    }

    /// A shared (extracted) Arc never sees an in-flight mutation: make_mut copies instead.
    #[test]
    fn extracted_rows_are_copy_on_write() {
        let mut t = PropProbes::default();
        let a = t.alloc([Vec4::splat(0.5); 7]).unwrap();
        let extracted = Arc::clone(&t.rows);
        t.release(a);
        assert_eq!(extracted[a as usize], [[0.5; 4]; 7]); // the render copy is untouched
        assert_eq!(t.rows[a as usize], [[0.0; 4]; 7]);
    }
}
