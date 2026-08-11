//! Rust-driven tests of the Lua host: the object model, layout+size reads, show/hide + event + tick
//! firing (both RF-0025 conventions), the WoW stdlib (positional `format`, `strsplit`, `wipe`,
//! `getglobal`), the sandbox holes, and an end-to-end extract in ZKey order.
//!
//! Split by subject; the shared `script()` fixture lives in [`common`].

mod anchors;
mod backdrop;
mod button;
mod common;
mod cooldown;
mod create_frame_template;
mod end_to_end;
mod events;
mod font_object;
mod frame_api;
mod input;
mod layout_gate;
mod measure;
mod minimap;
mod movable;
mod object_model;
mod reference_surface;
mod regions;
mod scrollframe;
mod size_changed;
mod slider;
mod statusbar;
mod stdlib;
mod talent;
mod taxi;
mod texcoord_font;
mod tooltip;
mod tooltip_item;
mod tooltip_spell;
mod tooltip_unit;
mod toplevel;
mod visibility;
mod worldmap;
