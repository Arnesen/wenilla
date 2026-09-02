//! The `PlaySound`/`PlaySoundFile` bindings — the first **Lua→app intent queue** (decisions
//! 0068 §3 / 0070 §4).
//!
//! The engine must not touch the app's systems (the same seam that keeps [`super::unit`]'s
//! bindings reading a pushed snapshot), so an outbound *action* can't call the mixer either:
//! `PlaySound` pushes a plain [`SoundRequest`] onto a queue in [`super::Model`], and the app
//! drains it each frame via [`UiScript::take_sounds`](super::UiScript::take_sounds) into its kit
//! player. This queue-drain shape is the pattern every Lua→app intent follows (`CastSpell`,
//! `UseAction`, … ride the same seam as they land).
//!
//! Surface: the Era signature `PlaySound(soundKitID[, channel, forceNoDuplicates])` (addons pass
//! numeric `SOUNDKIT.*` ids), plus the 1.12-native by-name form `PlaySound("KitName")` — the
//! client resolved both (`PlaySoundById 0x458850` / `PlaySoundByName 0x458030`), and our kit
//! player keeps both entries. Returns `willPlay, soundHandle`: `willPlay` reports "queued" —
//! whether the kit actually plays is decided at drain time by the kit player's own gates
//! (audibility, duplicate suppression), which the client also treats as silent non-errors; the
//! handle is nil (no stop-by-handle surface yet). The `channel`/`forceNoDuplicates` extras are
//! accepted and ignored for now — the kit's own `SoundType`/flags decide category and dedup.
//!
//! `PlaySoundFile("Sound\\...\\file.wav")` is the sibling by-path form (the 1.12 client resolves
//! it through the same file layer as kit entries — MPQ chain, loose files shadowing): no kit, so
//! no gates and no variation; same queue, same returns.

use mlua::{Lua, Value};

use super::Model;

/// One queued `PlaySound` intent, drained by the app (plain data — the engine-free seam).
#[derive(Clone, Debug, PartialEq)]
pub enum SoundRequest {
    /// `PlaySound(id)` — a `SoundEntries` kit id (the Era `SOUNDKIT.*` form).
    KitId(u32),
    /// `PlaySound("name")` — a kit name (the 1.12 `PlaySoundByName` form).
    KitName(String),
    /// `PlaySoundFile("path")` — a raw file path, no kit.
    File(String),
}

impl super::UiScript {
    /// Drain the sounds queued by `PlaySound` since the last call. The app plays each through its
    /// kit player (2D — UI sounds have no world position).
    /// Queue a kit play from the APP side (the client's C++-fired UI sounds — QUESTADDED on a
    /// log add, QUESTCOMPLETED on the turn-in packet — which no Lua handler owns). Drained by the
    /// same [`UiScript::take_sounds`] as every Lua `PlaySound`.
    pub fn queue_sound_kit(&mut self, name: &str) {
        self.model_mut()
            .sound_queue
            .push(SoundRequest::KitName(name.to_string()));
    }

    pub fn take_sounds(&mut self) -> Vec<SoundRequest> {
        std::mem::take(&mut self.model_mut().sound_queue)
    }
}

/// Register the `PlaySound` and `PlaySoundFile` globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "PlaySoundFile",
        lua.create_function(|lua, args: mlua::MultiValue| {
            let Some(Value::String(s)) = args.front() else {
                return Err(mlua::Error::runtime("Usage: PlaySoundFile(\"filePath\")"));
            };
            let path = s.to_str()?.to_owned();
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.sound_queue.push(SoundRequest::File(path));
            drop(model);
            Ok((true, Value::Nil))
        })?,
    )?;
    lua.globals().set(
        "PlaySound",
        lua.create_function(|lua, args: mlua::MultiValue| {
            let req = match args.front() {
                Some(Value::Integer(i)) if *i >= 0 => Some(SoundRequest::KitId(*i as u32)),
                Some(Value::Number(n)) if *n >= 0.0 => Some(SoundRequest::KitId(*n as u32)),
                Some(Value::String(s)) => Some(SoundRequest::KitName(s.to_str()?.to_owned())),
                _ => None,
            };
            let Some(req) = req else {
                // The client raises a usage error on a bad argument; in a handler this lands in
                // the host's collected script errors, never a panic.
                return Err(mlua::Error::runtime(
                    "Usage: PlaySound(soundKitID or \"KitName\")",
                ));
            };
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            // `PlaySoundByName 0x458030`'s first act: read the UI-load suppression depth
            // (`0x458046`) and bail at `0x45804d` if it is non-zero — BEFORE the
            // `MasterSoundEffects` CVar and before the kit-hash lookup. So the call is dropped
            // rather than muted or queued, and the binding still answers as if it had played.
            // NAME-keyed only: the id-keyed entry (`0x457fb0`) has no such gate, which is why the
            // arm above pushes unconditionally.
            let suppressed = matches!(req, SoundRequest::KitName(_)) && model.sound_suppression > 0;
            if !suppressed {
                model.sound_queue.push(req);
            }
            drop(model);
            // willPlay (queued), soundHandle (nil — module docs).
            Ok((true, Value::Nil))
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::SoundRequest;
    use crate::script::UiScript;

    #[test]
    fn playsound_queues_by_id_and_name_and_drains() {
        let mut s = UiScript::new().unwrap();
        let (will_play, handle_is_nil): (bool, bool) = s
            .eval("local w, h = PlaySound(1234) return w, h == nil")
            .unwrap();
        assert!(will_play && handle_is_nil);
        // Era extras accepted and ignored; the by-name 1.12 form works too.
        s.run(r#"PlaySound(841, "SFX", true)"#).unwrap();
        s.run(r#"PlaySound("GAMEOBJECT_DOOROPEN")"#).unwrap();
        s.run(r#"PlaySoundFile("Sound\\Doodad\\BellTollHorde.wav")"#)
            .unwrap();

        assert_eq!(
            s.take_sounds(),
            vec![
                SoundRequest::KitId(1234),
                SoundRequest::KitId(841),
                SoundRequest::KitName("GAMEOBJECT_DOOROPEN".into()),
                SoundRequest::File("Sound\\Doodad\\BellTollHorde.wav".into()),
            ]
        );
        // Drained: the queue is empty until the next PlaySound.
        assert!(s.take_sounds().is_empty());
    }

    #[test]
    fn playsound_with_a_bad_argument_is_a_usage_error() {
        let mut s = UiScript::new().unwrap();
        assert!(s.run("PlaySound(nil)").is_err());
        assert!(s.run("PlaySound()").is_err());
        assert!(s.run("PlaySoundFile()").is_err());
        assert!(s.run("PlaySoundFile(42)").is_err());
        assert!(s.take_sounds().is_empty());
    }
}
