//! The chat windows' **saved state** — the whole `chat-cache.txt` record: the per-type colour
//! table and, per window, the tint, alpha, font size, lock, dock, shown flag, name, the message
//! types it shows and the channels it carries (decisions 1589 → 1714 → 1948).
//!
//! The state itself lives in the VM ([`benilla_ui::script::ChatWindowLook`] and the chat-type
//! registry, written by the reference's own `SetChatWindow*`, `Add/RemoveChatWindowMessages`,
//! `Add/RemoveChatWindowChannel`, `ChangeChatColor`, and read straight back out of
//! `GetChatWindowInfo`/`GetChatWindowMessages`/`GetChatWindowChannels`/`GetChatTypeIndex`); this
//! module is the two ends the VM cannot own — **where it comes from at login and where it goes at
//! logout**.
//!
//! ## The grammar is the reference's
//!
//! 1.12 keeps these in `WTF/Account/<ACC>/<REALM>/<CHAR>/chat-cache.txt`, written whole by
//! `0x499a80` and read by `0x498a60` (wow-re `system/ui/scratch/chat-cache-grammar.md`, §1 and
//! §3 — every line below is that note's):
//!
//! ```text
//! VERSION 2
//! ADDEDVERSION 2
//! OPTION_GUILD_RECRUITMENT_CHANNEL AUTO
//! CHANNELS                 ← the custom channels the client is in, re-joined at login
//! MyChannel
//! END
//! ZONECHANNELS 18874371    ← the joined zone channels, as bits 1<<(ChannelID-1)
//! COLORS                   ← the chat-type registry, R G B bytes
//! SAY 255 255 255
//! …
//! END
//!
//! WINDOW 1
//! NAME General             ← only when a name was stored (name[0] != 0)
//! SIZE 0
//! COLOR 0 0 0 0            ← R G B A bytes, from the record's packed BGRA quad
//! LOCKED 1
//! DOCKED 1
//! SHOWN 1
//!
//! MESSAGES                 ← the enabled groups of the 68-entry CHATMSGGROUP table, table order
//! SYSTEM
//! …
//! END
//! CHANNELS                 ← this window's CUSTOM channels only — a zone channel is never a name
//! END
//! ZONECHANNELS 18874371    ← this window's zone channels, as bits, masked by the joined set
//! END
//! ```
//!
//! Two consequences worth stating. **The file is the sole source**: the loader zeroes every
//! window's flags before parsing, so a `MESSAGES` block is the set, not an addition — and a
//! window the file does not mention keeps the boot init ([`ChatWindowLook::stock`]). **Zone
//! channels are bits, not names**: the record stores the DBC Shortcut (`General`) with its id,
//! the file stores the bit, and the loader's in-window `ZONECHANNELS` arm turns bits back into
//! `(Shortcut, id)` rows. A file older than `ADDEDVERSION 2` gets the groups added since
//! back-filled (`COMBAT_FACTION_CHANGE`, `MONEY` — into window 2), exactly as the loader does.
//!
//! Ours is `benilla-config/chat/<realm>-<character>.txt`
//! ([`crate::local_state::chat_character_path`]) in that grammar, so a stock file reads here and
//! ours reads there. The reader is as lenient as the reference's — case-insensitive keys, an
//! unknown key skipped rather than failing the line, blank lines nothing — and still accepts the
//! one-line `WINDOW 1  SIZE 0  COLOR …` rows the files written before 1948 hold, so no player's
//! saved looks are lost to the grammar change.
//!
//! `LOCKED` is an `i32` in the record (`CHATWINDOW+0x8c`) but the cache writer booleanises it
//! through `setne`, so only `{0,1}` round-trip there — which is why ours is a `bool`.
//!
//! **The loader's `WINDOW` bound is off by one in the reference** — `0x498d1c` uses `ja` where the
//! array wants `jae`. Ours cannot: the index is bounds-checked at the seam that consumes it
//! (`set_chat_window_looks` uses `get_mut`), and an out-of-range window is simply dropped.
//!
//! ## The login events
//!
//! The loader fires `UPDATE_CHAT_WINDOWS` once and then `UPDATE_CHAT_COLOR` for **every** registry
//! entry — file or no file (§8). The first is what `FloatingChatFrame_Update` docks, hides and
//! colours the windows from; the second is how a saved colour reaches `ChatTypeInfo` and repaints
//! the lines already in the window (`ChatFrame_OnEvent`'s arm). So does ours, once per character
//! per VM.
//!
//! ## Why per character, and not a CVar
//!
//! Both halves matter. *Per character*, because it is where the reference puts it and because it
//! is what the setting means — a raid alt reading a 40-man combat log wants a solid box where a
//! questing alt wants glass. And *not a CVar*, because `SetChatWindowAlpha` is the API 1.12 addons
//! are written against: an addon that reads a window's alpha calls `GetChatWindowInfo`, and
//! routing benilla's store through `config.toml` instead would have given the same player setting
//! two different names depending on who asked.
//!
//! ## The write posture
//!
//! **Debounced by one quiet second, plus both session edges** — the colour picker's opacity slider
//! drives `FCF_SetChatWindowOpacity` on *every drag step*, so this is a slider, not a discrete
//! edit: [`crate::cvars`]'s `SAVE_QUIET` reasoning applies verbatim ("long enough to coalesce a
//! slider drag, short enough that a crash loses one gesture, not a session"). The edges are
//! `OnExit(InWorld)` and `AppExit`, the same two the camera pose and the saved variables use.

use std::path::PathBuf;

use bevy::prelude::*;

use benilla_ui::script::{ChatTypeColor, ChatWindowLook, UiScript, MESSAGE_GROUPS};

use crate::net::{ClientCommand, NetCommands};
use crate::ui_script::VmMemo;

/// How long a dirty look sits before the save fires. [`crate::cvars`]'s own constant and its own
/// reasoning — an opacity drag is exactly the gesture it was sized for.
const SAVE_QUIET: std::time::Duration = std::time::Duration::from_secs(1);

/// The file's header — where these values come from and where the law lives. Comment lines; the
/// reference's reader has none, ours skips them.
const HEADER: &str = "\
# benilla chat cache (decisions 1589, 1948) — the reference's chat-cache.txt grammar (wow-re
# chat-cache-grammar.md): the custom channels to re-join, the joined zone channels as bits, the
# per-type COLORS table, then one WINDOW block per chat frame — NAME (when one was set), SIZE,
# COLOR r g b a as bytes, LOCKED, DOCKED, SHOWN, the MESSAGES … END list of the groups the window
# shows, its CHANNELS … END list of custom channels, and its zone channels as ZONECHANNELS bits.
# Written whole; the tab menu, /join and ChangeChatColor are what move it.
";

/// Which character's file we are on, where it lives, and whether it is owed a write.
#[derive(Resource, Default)]
pub(super) struct ChatWindowFile {
    path: Option<PathBuf>,
    /// The `(realm, character)` [`Self::path`] was built for. Session-keyed (1290) like the macro
    /// and binding loads: the *same* character coming back still meets a fresh VM whose look table
    /// is back at the stock row.
    identity: VmMemo<Option<(String, String)>>,
    /// Whether **this VM** has unsaved writes. Session-keyed for a reason that is one-way and
    /// therefore worth the wrapper: the values live in the VM, so a plain `bool` surviving a VM
    /// replacement would let a save compose the player's file out of a table that is back at the
    /// stock row — the "refusing to compose the file from nothing" hazard `crate::cvars` guards
    /// against, one store over. A fresh VM starts undirty and cannot write until Lua writes.
    dirty: VmMemo<bool>,
    last_change: Option<bevy::platform::time::Instant>,
}

/// What a file parses to: the windows it names (0-based index, the record), the `COLORS` rows
/// it carries in file order, and the custom channels its header lists for re-joining.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Parsed {
    pub(super) looks: Vec<(usize, ChatWindowLook)>,
    pub(super) colors: Vec<(String, [u8; 3])>,
    pub(super) joined: Vec<String>,
}

/// The bit a zone channel's id occupies in a `ZONECHANNELS` word.
fn zone_bit(id: u32) -> u32 {
    if id == 0 || id > 32 {
        0
    } else {
        1 << (id - 1)
    }
}

/// Render the file exactly as the writer does (§1), window order. `joined` is the client's
/// current channel roster — the custom names go to the header's `CHANNELS`, the zone ids to the
/// two `ZONECHANNELS` words.
fn render(looks: &[ChatWindowLook], colors: &[ChatTypeColor], joined: &[(String, u32)]) -> String {
    let zone_mask = joined.iter().fold(0, |m, (_, id)| m | zone_bit(*id));
    let mut out = String::from(HEADER);
    out.push_str(
        "\nVERSION 2\n\nADDEDVERSION 2\n\nOPTION_GUILD_RECRUITMENT_CHANNEL AUTO\n\nCHANNELS\n",
    );
    for (name, id) in joined {
        if *id == 0 {
            out.push_str(name);
            out.push('\n');
        }
    }
    out.push_str(&format!("END\n\nZONECHANNELS {zone_mask}\n\nCOLORS\n"));
    for c in colors {
        out.push_str(&format!(
            "{} {} {} {}\n",
            c.name, c.rgb[0], c.rgb[1], c.rgb[2]
        ));
    }
    out.push_str("END\n\n");
    for (i, l) in looks.iter().enumerate() {
        out.push_str(&format!("WINDOW {}\n", i + 1));
        if !l.name.is_empty() {
            out.push_str(&format!("NAME {}\n", l.name));
        }
        out.push_str(&format!(
            "SIZE {}\nCOLOR {} {} {} {}\nLOCKED {}\nDOCKED {}\nSHOWN {}\n\nMESSAGES\n",
            l.font_size,
            l.r,
            l.g,
            l.b,
            l.a,
            i32::from(l.locked),
            l.docked.unwrap_or(0),
            i32::from(l.shown),
        ));
        for m in &l.messages {
            out.push_str(m);
            out.push('\n');
        }
        out.push_str("END\n\nCHANNELS\n");
        let mut window_mask = 0;
        for (name, id) in &l.channels {
            if *id == 0 {
                out.push_str(name);
                out.push('\n');
            } else {
                window_mask |= zone_bit(*id);
            }
        }
        out.push_str(&format!(
            "END\n\nZONECHANNELS {}\n\nEND\n\n",
            window_mask & zone_mask
        ));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    /// Top level, or inside a `WINDOW` whose keys arrive one per line.
    Top,
    /// The header's `CHANNELS … END` — the custom channels to re-join.
    Joined,
    Colors,
    Messages,
    Channels,
}

/// Parse a file. `rows` is `ChatChannels.dbc` as `(id, Shortcut)`, which the in-window
/// `ZONECHANNELS` arm needs to turn bits back into `(Shortcut, id)` channel rows.
fn parse(text: &str, rows: &[(u32, String)]) -> Parsed {
    let mut out = Parsed::default();
    let mut current: Option<(usize, ChatWindowLook)> = None;
    let mut block = Block::Top;
    let mut added_version: u8 = 0;
    let byte = |s: Option<&str>| -> u8 { s.and_then(|v| v.parse::<u8>().ok()).unwrap_or(0) };
    let flush = |current: &mut Option<(usize, ChatWindowLook)>, out: &mut Parsed| {
        if let Some(w) = current.take() {
            out.looks.push(w);
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(head) = it.next() else { continue };
        let is_end = head.eq_ignore_ascii_case("END");
        match block {
            Block::Colors => {
                if is_end {
                    block = Block::Top;
                } else {
                    let rgb = [byte(it.next()), byte(it.next()), byte(it.next())];
                    out.colors.push((head.to_ascii_uppercase(), rgb));
                }
                continue;
            }
            Block::Joined => {
                if is_end {
                    block = Block::Top;
                } else {
                    out.joined.push(line.to_string());
                }
                continue;
            }
            Block::Messages => {
                if is_end {
                    block = Block::Top;
                } else if let Some((_, look)) = current.as_mut() {
                    look.messages.push(head.to_ascii_uppercase());
                }
                continue;
            }
            Block::Channels => {
                if is_end {
                    block = Block::Top;
                } else if let Some((_, look)) = current.as_mut() {
                    // The loader takes the first word, id 0.
                    look.channels.push((head.to_string(), 0));
                }
                continue;
            }
            Block::Top => {}
        }
        if head.eq_ignore_ascii_case("COLORS") {
            block = Block::Colors;
            continue;
        }
        if head.eq_ignore_ascii_case("WINDOW") {
            flush(&mut current, &mut out);
            let Some(index) = it.next().and_then(|n| n.parse::<usize>().ok()) else {
                warn!("chat cache: WINDOW line with no number ignored: {line}");
                continue;
            };
            if index == 0 {
                continue;
            }
            let mut look = ChatWindowLook::stock(index - 1);
            // The pre-1948 one-line row: the keys follow on the same line.
            while let Some(key) = it.next() {
                apply_key(&mut look, key, &mut it, rows);
            }
            current = Some((index - 1, look));
            continue;
        }
        let Some((_, look)) = current.as_mut() else {
            if head.eq_ignore_ascii_case("CHANNELS") {
                block = Block::Joined;
            } else if head.eq_ignore_ascii_case("ADDEDVERSION") {
                added_version = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            // VERSION, OPTION_*, and the header's ZONECHANNELS (the joined mask — ours is
            // whatever the zone walk joins) are the reference's and not read here.
            continue;
        };
        if is_end {
            flush(&mut current, &mut out);
        } else if head.eq_ignore_ascii_case("MESSAGES") {
            look.messages.clear();
            block = Block::Messages;
        } else if head.eq_ignore_ascii_case("CHANNELS") {
            look.channels.retain(|(_, id)| *id != 0);
            block = Block::Channels;
        } else if head.eq_ignore_ascii_case("NAME") {
            look.name = line[4..].trim().to_string();
        } else {
            apply_key(look, head, &mut it, rows);
        }
    }
    flush(&mut current, &mut out);
    // The loader's EOF back-fill (§3): a file older than the groups added since gets them, into
    // window 1 for the first ten groups and window 2 otherwise — the two `addedVersion` rows are
    // both window 2's.
    if added_version < 2 {
        for (i, (name, on, ver)) in MESSAGE_GROUPS.iter().enumerate() {
            if *on && *ver > added_version {
                let target = usize::from(i >= 10);
                if let Some((_, look)) = out.looks.iter_mut().find(|(w, _)| *w == target) {
                    if !look.messages.iter().any(|m| m == name) {
                        look.messages.push((*name).to_string());
                    }
                }
            }
        }
    }
    for (_, look) in &mut out.looks {
        look.normalize_messages();
    }
    out
}

/// One `KEY value…` of a window block, whichever line it arrived on.
fn apply_key<'a>(
    look: &mut ChatWindowLook,
    key: &str,
    it: &mut impl Iterator<Item = &'a str>,
    rows: &[(u32, String)],
) {
    let byte = |s: Option<&str>| -> u8 { s.and_then(|v| v.parse::<u8>().ok()).unwrap_or(0) };
    let flag = |s: Option<&str>| -> bool { s.is_none_or(|v| v.trim() != "0") };
    if key.eq_ignore_ascii_case("SIZE") {
        look.font_size = it
            .next()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            .max(0);
    } else if key.eq_ignore_ascii_case("COLOR") {
        look.r = byte(it.next());
        look.g = byte(it.next());
        look.b = byte(it.next());
        look.a = byte(it.next());
    } else if key.eq_ignore_ascii_case("LOCKED") {
        look.locked = flag(it.next());
    } else if key.eq_ignore_ascii_case("DOCKED") {
        look.docked = it
            .next()
            .and_then(|v| v.trim().parse::<u8>().ok())
            .filter(|p| *p > 0);
    } else if key.eq_ignore_ascii_case("SHOWN") {
        look.shown = flag(it.next());
    } else if key.eq_ignore_ascii_case("ZONECHANNELS") {
        // The in-window arm: every DBC row whose bit is set joins the window as (Shortcut, id).
        let mask = it
            .next()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(0);
        for (id, shortcut) in rows {
            if mask & zone_bit(*id) != 0
                && !look
                    .channels
                    .iter()
                    .any(|(c, _)| c.eq_ignore_ascii_case(shortcut))
            {
                look.channels.push((shortcut.clone(), *id));
            }
        }
    }
}

/// The `(id, Shortcut)` rows the parser needs, off the loaded `ChatChannels.dbc`.
fn shortcut_rows(channels: &super::edit::ChannelState) -> Vec<(u32, String)> {
    channels
        .channels
        .rows()
        .iter()
        .map(|r| (r.id, r.shortcut.clone()))
        .collect()
}

/// The client's channel roster as `(name, zone id)` — what the writer's header and masks are made
/// of.
fn roster(channels: &super::edit::ChannelState) -> Vec<(String, u32)> {
    channels
        .joined
        .iter()
        .flatten()
        .map(|name| (name.clone(), channels.channels.zone_channel_id(name)))
        .collect()
}

/// Restore the file into a fresh VM — once per character per VM — and fire the loader's two
/// events, file or no file.
fn load_chat_looks(
    script: Option<NonSendMut<UiScript>>,
    roster_res: Res<crate::char_select::Roster>,
    channels: Res<super::edit::ChannelState>,
    commands: Res<NetCommands>,
    mut file: ResMut<ChatWindowFile>,
) {
    let Some(mut script) = script else { return };
    let Some(id) = crate::ui_macro::identity(&roster_res) else {
        return;
    };
    if file.identity.get(&script).as_ref() == Some(&id) {
        return; // already restored for this character, into the VM that is live now
    }
    file.path = crate::local_state::chat_character_path(&id.0, &id.1);
    *file.identity.get(&script) = Some(id);
    *file.dirty.get(&script) = false;
    file.last_change = None;
    let text = file
        .path
        .as_ref()
        .and_then(|path| match crate::local_state::read_to_string(path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                warn!("chat cache: cannot read {}: {e}", path.display());
                None
            }
        });
    let had_file = text.is_some();
    let mut parsed = text
        .map(|t| parse(&t, &shortcut_rows(&channels)))
        .unwrap_or_default();
    if !had_file {
        // The loader's no-file path (§3, `0x4997ad`): window 1's channel slots get every
        // `ChatChannels.dbc` row the client joins by itself (`flags & 1`) as `(Shortcut, id)` —
        // the rows `ChatFrame_RegisterForChannels` will match zone speech against by id. The
        // rest of the record is the boot init the VM already holds.
        let mut general = ChatWindowLook::stock(0);
        general.channels = channels
            .channels
            .auto_join_rows()
            .map(|r| (r.shortcut.clone(), r.id))
            .collect();
        parsed.looks.push((0, general));
    }
    if !parsed.looks.is_empty() || !parsed.colors.is_empty() {
        info!(
            "chat cache: {} windows, {} colour rows, {} custom channels restored",
            parsed.looks.len(),
            parsed.colors.len(),
            parsed.joined.len()
        );
    }
    script.set_chat_colors(parsed.colors);
    script.set_chat_window_looks(parsed.looks);
    // §8: UPDATE_CHAT_WINDOWS once, then UPDATE_CHAT_COLOR for every registry entry, on the file
    // path and the no-file path alike.
    script.fire_event("UPDATE_CHAT_WINDOWS", vec![]);
    let renorm = |b: u8| f64::from(b as f32 * (1.0f32 / 255.0f32));
    for entry in script.chat_colors() {
        script.fire_event(
            "UPDATE_CHAT_COLOR",
            vec![
                benilla_ui::script::ScriptValue::Str(entry.name),
                benilla_ui::script::ScriptValue::Number(renorm(entry.rgb[0])),
                benilla_ui::script::ScriptValue::Number(renorm(entry.rgb[1])),
                benilla_ui::script::ScriptValue::Number(renorm(entry.rgb[2])),
            ],
        );
    }
    // The header's CHANNELS: the custom channels the character was in, re-joined the way the
    // reference re-joins them at login (the zone channels are the zone walk's, not the file's).
    for name in parsed.joined {
        let _ = commands.0.send(ClientCommand::JoinChannel {
            name,
            password: String::new(),
        });
    }
}

/// A Lua-side write landed — arm the debounce.
fn watch_chat_looks(script: Option<NonSendMut<UiScript>>, mut file: ResMut<ChatWindowFile>) {
    let Some(mut script) = script else { return };
    let moved = !script.take_chat_window_changes().is_empty();
    let coloured = script.take_chat_color_changes();
    if !(moved || coloured) {
        return;
    }
    *file.dirty.get(&script) = true;
    file.last_change = Some(bevy::platform::time::Instant::now());
}

fn write(script: &UiScript, channels: &super::edit::ChannelState, path: &std::path::Path) {
    let body = render(
        &script.chat_window_looks(),
        &script.chat_colors(),
        &roster(channels),
    );
    if let Err(e) = crate::local_state::write_atomic(path, &body) {
        warn!("chat cache: cannot write {}: {e}", path.display());
    }
}

/// The debounced save, and the `AppExit` flush.
fn save_chat_looks(
    script: Option<NonSendMut<UiScript>>,
    channels: Res<super::edit::ChannelState>,
    mut file: ResMut<ChatWindowFile>,
    mut exits: MessageReader<AppExit>,
) {
    let exiting = exits.read().next().is_some();
    let Some(script) = script else { return };
    if !*file.dirty.get(&script) {
        return;
    }
    if !(exiting || file.last_change.is_none_or(|t| t.elapsed() >= SAVE_QUIET)) {
        return;
    }
    let Some(path) = file.path.clone() else {
        *file.dirty.get(&script) = false;
        return;
    };
    write(&script, &channels, &path);
    *file.dirty.get(&script) = false;
}

/// The session-end flush — `OnExit(InWorld)`.
fn save_on_session_end(
    script: Option<NonSendMut<UiScript>>,
    channels: Res<super::edit::ChannelState>,
    mut file: ResMut<ChatWindowFile>,
) {
    let Some(script) = script else { return };
    if !*file.dirty.get(&script) {
        return;
    }
    if let Some(path) = file.path.clone() {
        write(&script, &channels, &path);
    }
    *file.dirty.get(&script) = false;
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<ChatWindowFile>()
        .add_systems(
            Update,
            (load_chat_looks, watch_chat_looks)
                .chain()
                .run_if(in_state(crate::char_select::ClientState::InWorld))
                // …and never before the in-game UI exists (1348's law, the unit feed's own
                // words): the restore fires `UPDATE_CHAT_WINDOWS` once and latches a per-VM
                // memo, and the VM that is live between the world-entry edge and the deferred
                // entry load is the SAME object the load then fills — so a restore made in
                // that window reaches no chat frame and its memo blocks the one that would.
                // Every plate texture stayed at the XML's white, alpha 1 (director report,
                // 2026-09-04).
                .run_if(bevy::ecs::schedule::common_conditions::not(
                    crate::ui_script::ingame_ui_pending,
                )),
        )
        .add_systems(
            Update,
            save_chat_looks.run_if(in_state(crate::char_select::ClientState::InWorld)),
        )
        .add_systems(
            OnExit(crate::char_select::ClientState::InWorld),
            // **Explicitly before the session ender, because the edge is not an ordering.** This
            // flush composes the file out of the DYING VM, and `end_ui_session` replaces that VM
            // with a fresh boot one on the same edge. Two unconstrained systems there are placed
            // by the executor, and that placement was measured (bevy 0.18, five systems on one
            // `OnExit`) to move with nothing but their registration positions — a VM-reading
            // saver ran after the exclusive ender in one arrangement and before it in another.
            // Winning that race by luck is what B353 cost the layout cache one module over; the
            // layout cache answered it by joining the reference's shutdown tail, which this file
            // cannot do (its reference is the type-7 chat-cache saver `0x499a80`, not
            // `0x490bd0`'s tail), so it says the order instead.
            save_on_session_end.before(crate::ui_script::end_ui_session),
        );
    // The quit flush rides the exit edge rather than `Update` for decision 1528's reason: the
    // close button's `AppExit` is not written until `PostUpdate`, so a save chained beside the
    // watcher would lose the last second of drags to the process ending.
    crate::shutdown::on_app_exit(app, save_chat_looks.into_configs());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record for WINDOW `n` (1-based) — the window's own boot-init row with the look fields
    /// set. The window number is a parameter because `DOCKED`, `SHOWN` and `MESSAGES` differ per
    /// window at init, so an expectation that does not say which window it is cannot be right
    /// for all of them.
    fn look(n: usize, r: u8, g: u8, b: u8, a: u8, font_size: i32) -> ChatWindowLook {
        ChatWindowLook {
            r,
            g,
            b,
            a,
            font_size,
            locked: true,
            ..ChatWindowLook::stock(n - 1)
        }
    }

    fn rows() -> Vec<(u32, String)> {
        vec![
            (1, "General".to_string()),
            (2, "Trade".to_string()),
            (24, "LookingForGroup".to_string()),
        ]
    }

    fn colors(rows: &[(&str, [u8; 3])]) -> Vec<ChatTypeColor> {
        rows.iter()
            .map(|(n, rgb)| ChatTypeColor {
                name: (*n).to_string(),
                rgb: *rgb,
            })
            .collect()
    }

    /// The file round-trips — every field of the record, the colour rows, the header's custom
    /// channels, the zone bits back into `(Shortcut, id)` rows — indices included.
    #[test]
    fn the_file_round_trips() {
        let mut named = look(3, 9, 8, 7, 6, 12);
        named.name = "Loot & Trade".into();
        named.shown = true;
        named.messages = vec!["LOOT".into(), "MONEY".into()];
        named.channels = vec![("MyChan".into(), 0), ("Trade".into(), 2)];
        let looks = vec![
            look(1, 0, 0, 0, 64, 14),
            look(2, 255, 128, 0, 255, 0),
            named,
        ];
        let colors = colors(&[("SAY", [1, 2, 3]), ("CHANNEL7", [4, 5, 6])]);
        let joined = vec![
            ("General - Elwynn Forest".to_string(), 1),
            ("Trade - City".to_string(), 2),
            ("MyChan".to_string(), 0),
        ];
        let text = render(&looks, &colors, &joined);
        assert!(
            text.contains("\nCHANNELS\nMyChan\nEND\n\nZONECHANNELS 3\n"),
            "{text}"
        );
        let parsed = parse(&text, &rows());
        let mut expect = looks.clone();
        // The zone channel comes back by its Shortcut row, after the custom names.
        expect[2].channels = vec![("MyChan".into(), 0), ("Trade".into(), 2)];
        assert_eq!(
            parsed.looks,
            vec![
                (0, expect[0].clone()),
                (1, expect[1].clone()),
                (2, expect[2].clone())
            ],
            "0-based indices, values intact"
        );
        assert_eq!(
            parsed.colors,
            vec![
                ("SAY".to_string(), [1, 2, 3]),
                ("CHANNEL7".to_string(), [4, 5, 6])
            ]
        );
        assert_eq!(parsed.joined, vec!["MyChan".to_string()]);
    }

    /// A window's zone bits are masked by the joined set — a zone channel the client has left
    /// is not written for the window either, the way the writer ANDs the two words.
    #[test]
    fn a_windows_zone_bits_are_masked_by_the_joined_set() {
        let mut w = look(1, 0, 0, 0, 0, 0);
        w.channels = vec![("General".into(), 1), ("Trade".into(), 2)];
        let text = render(&[w], &[], &[("General - Elwynn Forest".to_string(), 1)]);
        let windows: Vec<&str> = text.split("WINDOW 1").collect();
        assert!(windows[1].contains("ZONECHANNELS 1\n"), "{text}");
    }

    /// The header is a comment block and survives the round trip as one — a reader that choked on
    /// its own header would lose the player's settings on the second launch.
    #[test]
    fn the_header_is_skipped_not_parsed() {
        assert!(render(&[ChatWindowLook::default()], &[], &[]).starts_with('#'));
        assert_eq!(parse(HEADER, &rows()), Parsed::default());
    }

    /// The rows the files written before 1948 hold — keys on the `WINDOW` line, no
    /// `ADDEDVERSION` — still read, case-insensitively, an unknown key skipped and a missing
    /// field left at its init value; and the loader's back-fill lands `COMBAT_FACTION_CHANGE` and
    /// `MONEY` in window 2 as it would for any pre-`ADDEDVERSION 2` file.
    #[test]
    fn the_pre_1948_one_line_rows_still_parse() {
        let got = parse(
            "window 1  size 16  color 10 20 30 40\n\
             WINDOW 2  SHOWN 0  COLOR 1 2 3 4  DOCKED 2  BOGUS 9\n\
             WINDOW 3  SIZE 12\n",
            &rows(),
        );
        let mut w2 = look(2, 1, 2, 3, 4, 0);
        w2.shown = false;
        w2.docked = Some(2);
        assert_eq!(
            got.looks,
            vec![
                (0, look(1, 10, 20, 30, 40, 16)),
                (1, w2),
                (2, look(3, 0, 0, 0, 0, 12)),
            ]
        );
        assert!(got.looks[1].1.messages.iter().any(|m| m == "MONEY"));
    }

    /// A file in the reference's own layout — the one a stock client writes — parses: the global
    /// blocks are read for what they are, the colours land, both dock windows carry their sets,
    /// and the window's `ZONECHANNELS` word comes back as `(Shortcut, id)` rows.
    #[test]
    fn a_stock_reference_file_parses() {
        let text = "VERSION 2\n\nADDEDVERSION 2\n\nOPTION_GUILD_RECRUITMENT_CHANNEL AUTO\n\n\
                    CHANNELS\nMyGuildChat\nEND\n\nZONECHANNELS 8388611\n\n\
                    COLORS\nSAY 255 255 255\nSYSTEM 200 200 0\nEND\n\n\
                    WINDOW 1\nSIZE 0\nCOLOR 0 0 0 0\nLOCKED 1\nDOCKED 1\nSHOWN 1\n\n\
                    MESSAGES\nSYSTEM\nSAY\nEND\n\nCHANNELS\nMyGuildChat\nEND\n\n\
                    ZONECHANNELS 8388611\n\nEND\n\n\
                    WINDOW 2\nNAME Combat Log\nSIZE 0\nCOLOR 0 0 0 0\nLOCKED 1\nDOCKED 2\nSHOWN 0\n\n\
                    MESSAGES\nCOMBAT_XP_GAIN\nEND\n\nCHANNELS\nEND\n\nZONECHANNELS 0\n\nEND\n";
        let got = parse(text, &rows());
        assert_eq!(
            got.colors,
            vec![
                ("SAY".to_string(), [255, 255, 255]),
                ("SYSTEM".to_string(), [200, 200, 0])
            ]
        );
        assert_eq!(got.joined, vec!["MyGuildChat".to_string()]);
        assert_eq!(got.looks.len(), 2);
        let (i, w1) = &got.looks[0];
        assert_eq!(*i, 0);
        assert!(w1.shown && w1.name.is_empty());
        assert_eq!(w1.messages, vec!["SYSTEM".to_string(), "SAY".to_string()]);
        assert_eq!(
            w1.channels,
            vec![
                ("MyGuildChat".to_string(), 0),
                ("General".to_string(), 1),
                ("Trade".to_string(), 2),
                ("LookingForGroup".to_string(), 24),
            ],
            "bits 0, 1 and 23 of 8388611 — the shortcut rows, in DBC order"
        );
        let (_, w2) = &got.looks[1];
        assert!(!w2.shown);
        assert_eq!(w2.name, "Combat Log");
        assert_eq!(w2.docked, Some(2));
        assert_eq!(w2.messages, vec!["COMBAT_XP_GAIN".to_string()]);
    }

    /// `LOCKED` round-trips, and a file that never mentions it keeps the init's **locked** row —
    /// the lenient default that matters, because reading it as "unlocked" would hand every
    /// pre-existing player's chat window to a stray drag on their next login.
    #[test]
    fn the_lock_round_trips_and_an_absent_key_stays_locked() {
        let unlocked = ChatWindowLook {
            locked: false,
            ..look(1, 0, 0, 0, 64, 14)
        };
        let text = render(std::slice::from_ref(&unlocked), &[], &[]);
        assert!(text.contains("LOCKED 0"));
        assert_eq!(parse(&text, &rows()).looks, vec![(0, unlocked.clone())]);
        assert_eq!(
            parse("WINDOW 1\nSIZE 14\nCOLOR 0 0 0 64\nEND\n", &rows()).looks,
            vec![(0, look(1, 0, 0, 0, 64, 14))],
            "no LOCKED key = the init's LOCKED 1"
        );
    }

    /// **`DOCKED` round-trips, and an absent key keeps the window's own init position** — the
    /// leniency `LOCKED` gets, in the one field where a single flat default could not express it
    /// (1714). `SHOWN` and the message sets get the same treatment for the same reason.
    #[test]
    fn dock_positions_round_trip_and_an_absent_key_keeps_the_init() {
        let moved = ChatWindowLook {
            docked: Some(3),
            ..look(1, 0, 0, 0, 0, 0)
        };
        let text = render(std::slice::from_ref(&moved), &[], &[]);
        assert!(text.contains("DOCKED 3"));
        assert_eq!(parse(&text, &rows()).looks, vec![(0, moved.clone())]);
        // No DOCKED/SHOWN/MESSAGES at all: the boot init stands — window 1 shown and undocked
        // with the ten General groups, window 2 shown at dock index 1 with the 34, window 3 out.
        let got = parse(
            "WINDOW 1  SIZE 0\nWINDOW 2  SIZE 0\nWINDOW 3  SIZE 0\n",
            &rows(),
        );
        assert_eq!(
            got.looks
                .iter()
                .map(|(_, l)| (l.docked, l.shown, l.messages.len()))
                .collect::<Vec<_>>(),
            vec![(None, true, 10), (Some(1), true, 34), (None, false, 0)]
        );
    }

    /// Junk costs the line it is on and nothing more.
    #[test]
    fn junk_costs_only_its_own_line() {
        let got = parse(
            "WINDOW\nnot a window line\nWINDOW 0 SIZE 1\nWINDOW 2 SIZE 18\n",
            &rows(),
        );
        assert_eq!(got.looks, vec![(1, look(2, 0, 0, 0, 0, 18))]);
    }

    /// A `MESSAGES` block replaces the init set rather than adding to it — an empty block is a
    /// window that shows nothing, which is what the writer means by it (the loader zeroes every
    /// flag before it reads).
    #[test]
    fn an_empty_messages_block_is_an_empty_set() {
        let got = parse("ADDEDVERSION 2\nWINDOW 1\nMESSAGES\nEND\nEND\n", &rows());
        assert!(got.looks[0].1.messages.is_empty());
    }
}
