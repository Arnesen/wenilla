//! **`WOW_GPU_MS=1` — the whole-frame GPU meter** (1389's scoped follow-on, built 2026-08-18;
//! the drastic-options census' unknown #2 was "GPU frame time and VRAM headroom — no instrument
//! exists").
//!
//! Two plain render-graph nodes bracket the camera driver — `begin` writes timestamp 0 into a
//! two-slot query set from its command encoder, `end` writes timestamp 1 — legal on this machine
//! because we hold `TIMESTAMP_QUERY_INSIDE_ENCODERS` (Apple Silicon samples at stage boundaries;
//! bevy's own `RenderDiagnosticsPlugin` needs INSIDE_PASSES and reads zero GPU spans here — the
//! `perf` module header's ⚠ note, measured in 1389).
//!
//! **The resolve rides its own submission, one frame later.** 1389's recorded trap: resolving
//! the query set in the SAME command buffer as the timed work returns zeros on Metal (5 of 6
//! reads; the 6th a plausible-looking near-miss). So the cleanup system resolves last frame's
//! set into a small ring of MAP_READ buffers via a fresh encoder, and reads the mapping two
//! frames behind. The published number is the freshest mapped delta, nanoseconds in one
//! `AtomicU64` shared with the main world — `FPS_PROBE` samples it per frame under the same env
//! and prints its own percentiles, so a leg gets `gpu_p50/gpu_p99` beside `cpu_ms` with zero
//! cost when the env is off (nothing registers).

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::graph::CameraDriverLabel;
use bevy::render::render_graph::{
    Node, NodeRunError, RenderGraph, RenderGraphContext, RenderLabel,
};
use bevy::render::render_resource::{
    Buffer, BufferDescriptor, BufferUsages, CommandEncoderDescriptor, MapMode,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::{Render, RenderApp, RenderSystems};
use wgpu::{PollType, QuerySet, QuerySetDescriptor, QueryType};

/// Is the meter armed? One read; everything registers behind it.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WOW_GPU_MS").as_deref() == Ok("1"))
}

/// The freshest whole-frame GPU duration, nanoseconds. Written by the render app's readback,
/// read by `FPS_PROBE`/whoever in the main world. 0 = no reading yet.
#[derive(Resource, Clone)]
pub(crate) struct GpuMsShared(pub Arc<AtomicU64>);

const RING: usize = 4;

/// Ring-slot states: a copy may only target a FREE slot — submitting into a mapped (or
/// map-pending) buffer is a wgpu validation error, which the first live run demonstrated.
const FREE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;

/// Render-world state: the query set, its resolve buffer, and the mapping ring.
#[derive(Resource)]
struct GpuStamp {
    query_set: QuerySet,
    /// A 1×1 target the sentinel passes clear — real (if trivial) work, because an EMPTY pass
    /// never reaches the Metal encoder and samples nothing (this meter's second zero-read).
    sentinel_view: wgpu::TextureView,
    resolve: Buffer,
    ring: Vec<(Buffer, Arc<AtomicU8>)>,
    frame: usize,
    period: f32,
    shared: Arc<AtomicU64>,
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct GpuStampBeginLabel;
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct GpuStampEndLabel;

struct GpuStampNode {
    index: u32,
}

impl Node for GpuStampNode {
    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        world: &World,
    ) -> Result<(), NodeRunError> {
        if let Some(stamp) = world.get_resource::<GpuStamp>() {
            // A SENTINEL PASS, not a bare `write_timestamp`: Apple Silicon samples counters only
            // at stage boundaries, and an encoder-level stamp with no stages around it resolves
            // to zero (measured on this meter's first live run — 1389's "two sentinel nodes"
            // meant exactly this). An empty compute pass is the cheapest stage boundary there is.
            let (begin, end) = if self.index == 0 {
                (Some(0), None)
            } else {
                (None, Some(1))
            };
            render_context
                .command_encoder()
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("gpu-ms sentinel"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &stamp.sentinel_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                        query_set: &stamp.query_set,
                        beginning_of_pass_write_index: begin,
                        end_of_pass_write_index: end,
                    }),
                    occlusion_query_set: None,
                });
        }
        Ok(())
    }
}

/// Build [`GpuStamp`] on the first render frame — the device does not exist yet at plugin
/// build time (`RenderPlugin` creates it in `finish`), which a startup-eager version learned
/// as a `RenderDevice does not exist` panic on its first live run.
fn init_stamp(
    mut commands: Commands,
    stamp: Option<Res<GpuStamp>>,
    device: Option<Res<RenderDevice>>,
    queue: Option<Res<RenderQueue>>,
    shared: Res<GpuMsShared>,
) {
    if stamp.is_some() {
        return;
    }
    let (Some(device), Some(queue)) = (device, queue) else {
        return;
    };
    let query_set = device.wgpu_device().create_query_set(&QuerySetDescriptor {
        label: Some("gpu-ms stamps"),
        ty: QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&BufferDescriptor {
        label: Some("gpu-ms resolve"),
        size: 16,
        usage: BufferUsages::QUERY_RESOLVE | BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let ring = (0..RING)
        .map(|_| {
            (
                device.create_buffer(&BufferDescriptor {
                    label: Some("gpu-ms read"),
                    size: 16,
                    usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                Arc::new(AtomicU8::new(FREE)),
            )
        })
        .collect();
    let sentinel = device
        .wgpu_device()
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("gpu-ms sentinel"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
    commands.insert_resource(GpuStamp {
        query_set,
        sentinel_view: sentinel.create_view(&wgpu::TextureViewDescriptor::default()),
        resolve,
        ring,
        frame: 0,
        period: queue.get_timestamp_period(),
        shared: shared.0.clone(),
    });
}

/// After the graph's own submission: resolve LAST frame's stamps on a fresh encoder (its own
/// command buffer — the 1389 trap), kick the map, and publish the oldest mapped delta.
fn readback(stamp: Option<ResMut<GpuStamp>>, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let Some(mut stamp) = stamp else { return };
    // Copy into a FREE slot only (none free → skip this frame's sample; the meter is a meter).
    if let Some(i) = (0..RING).find(|&i| stamp.ring[i].1.load(Ordering::Relaxed) == FREE) {
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("gpu-ms resolve"),
        });
        encoder.resolve_query_set(&stamp.query_set, 0..2, &stamp.resolve, 0);
        encoder.copy_buffer_to_buffer(&stamp.resolve, 0, &stamp.ring[i].0, 0, 16);
        queue.submit([encoder.finish()]);
        let flag = stamp.ring[i].1.clone();
        flag.store(PENDING, Ordering::Relaxed);
        let cb_flag = flag.clone();
        stamp.ring[i]
            .0
            .slice(..)
            .map_async(MapMode::Read, move |r| {
                cb_flag.store(if r.is_ok() { MAPPED } else { FREE }, Ordering::Relaxed);
            });
    }
    let _ = device.poll(PollType::Poll);
    // Publish every landed mapping; slots return to FREE.
    for i in 0..RING {
        if stamp.ring[i].1.load(Ordering::Relaxed) == MAPPED {
            let (t0, t1) = {
                let data = stamp.ring[i].0.slice(..).get_mapped_range();
                let words: &[u64] = bytemuck::cast_slice(&data);
                (words[0], words[1])
            };
            stamp.ring[i].0.unmap();
            stamp.ring[i].1.store(FREE, Ordering::Relaxed);
            if t1 > t0 {
                let ns = (t1 - t0) as f64 * f64::from(stamp.period);
                stamp.shared.store(ns as u64, Ordering::Relaxed);
            }
        }
    }
    stamp.frame += 1;
}

pub(crate) fn plugin(app: &mut App) {
    if !enabled() {
        return;
    }
    let shared = Arc::new(AtomicU64::new(0));
    app.insert_resource(GpuMsShared(shared.clone()));
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app.insert_resource(GpuMsShared(shared));
    render_app.add_systems(
        Render,
        (init_stamp, readback)
            .chain()
            .in_set(RenderSystems::Cleanup),
    );
    let mut graph = render_app.world_mut().resource_mut::<RenderGraph>();
    graph.add_node(GpuStampBeginLabel, GpuStampNode { index: 0 });
    graph.add_node(GpuStampEndLabel, GpuStampNode { index: 1 });
    graph.add_node_edge(GpuStampBeginLabel, CameraDriverLabel);
    graph.add_node_edge(CameraDriverLabel, GpuStampEndLabel);
}
