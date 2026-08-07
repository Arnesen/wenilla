//! The engine-free core of benilla's UI engine (decision 0068): the WoW interface is data + a
//! runtime — TOC manifests, FrameXML documents, and Lua — and this crate owns everything about
//! that runtime which needs neither Bevy nor a GPU: parsing, template expansion, the anchor/layout
//! resolver, and the draw-order key. The Bevy side (rendering, input, the game-state API bindings)
//! lives in the app; the Lua VM is embedded by the app and calls back into this crate's model.
//!
//! Built currently: [`toc`] — the addon/FrameXML manifest parser; [`framexml`] — the FrameXML
//! document parser (order-preserving tree, template expansion, `$parent` substitution); [`layout`]
//! — the anchor/layout resolver (anchor graph → resolved rects), transcribed bit-faithfully from
//! wow-5875-re's binary-verified spec; [`widget`] — the frame arena and its show/hide/strata/level/
//! scale/alpha propagation mutations; [`order`] — the strata/draw-layer vocabulary, the packed
//! total-order key, and the visible-tree traversal that realizes the client's painter order;
//! [`script`] — the engine-free Lua host (mlua 5.1) binding the arena/layout/order model to the
//! FrameScript object model + WoW stdlib so FrameXML/addon Lua runs against it (decision 0068);
//! [`loader`] — the FrameXML loader that joins the two, walking a parsed document to materialize live
//! frames in a running [`script::UiScript`] by driving the object model exactly as an addon does;
//! [`markup`] — the single owner of WoW's inline-markup grammar (`|cAARRGGBB`, `|H…|h…|h`, `|n`,
//! `||`): the client's token decoder and the per-byte class map its cursor law is built on.

pub mod framexml;
pub mod layout;
pub mod loader;
pub mod markup;
pub mod order;
pub mod script;
pub mod toc;
pub mod widget;
