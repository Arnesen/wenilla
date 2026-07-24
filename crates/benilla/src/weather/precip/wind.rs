//! The weather **wind** — the local player's trailing-average velocity (`0x67c150`) and the
//! frames derived from it: the streak apex tilt and the mist spawn yaw. Split from `precip`'s
//! root; the sim/spawn economics stay there.

use bevy::prelude::*;

/// Streak tilt: `lerp(0°, 45°, sat(|wind.xy|/30))` (`weather_wind_tilt 0x674a70`; 30 @`0x8680f0`,
/// 45 @`0x8680f4`). Applied to the streak's APEX vertex only (the `M·(2·antiVel)` term).
const WIND_TILT_DIV: f32 = 30.0;
const WIND_TILT_MAX_DEG: f32 = 45.0;

/// Wind = the **local player's** trailing-average velocity (`0x67c150`, verified: the list is
/// per-frame position deltas of object 0x8fe — the player, not the camera — trimmed to a
/// **~149 ms** window; `wind = Σ(Δpos)/((Σ(Δms)+1)·0.001)`, a true yd/s velocity). Orbiting
/// the camera therefore does NOT stir the wind. The streak apex tilt derives from it here; the
/// wind heading (manager+0x78) updates only while `|wind.xy| ≥ 1` (`wx_wind_angle`'s gate) and
/// holds its last value through calm.
#[derive(Resource)]
pub(crate) struct WeatherWind {
    pub(super) last_pos: Option<Vec3>,
    /// The trailing window: per-frame (Δseconds, Δpos), trimmed from the head to ≤ 149 ms.
    window: Vec<(f32, Vec3)>,
    /// The windowed planar velocity (Bevy x/z, y forced 0), yd/s.
    pub(super) vel: Vec3,
    /// The held wind heading (WoW frame, `manager+0x78`) — updated only at |wind| ≥ 1.
    heading: f32,
    /// Streak-field tilt (rotates the fall axis toward the motion), from `weather_wind_tilt`.
    pub(super) tilt: Quat,
}

impl Default for WeatherWind {
    fn default() -> Self {
        Self {
            last_pos: None,
            window: Vec::new(),
            vel: Vec3::ZERO,
            heading: 0.0,
            tilt: Quat::IDENTITY,
        }
    }
}

/// The wind window length: `Σ(Δtick) ≤ 0x95 = 149 ms` (the pre-pass trims the delta list to a
/// trailing ~149 ms).
const WIND_WINDOW_S: f32 = 0.149;

impl WeatherWind {
    pub(super) fn update(&mut self, pos: Vec3, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        let delta = match self.last_pos {
            Some(prev) => {
                let mut d = pos - prev;
                d.y = 0.0;
                // A teleport/worldport delta is not wind — restart the window.
                if d.length_squared() > (100.0 * dt).powi(2).max(100.0) {
                    self.window.clear();
                    d = Vec3::ZERO;
                }
                d
            }
            None => Vec3::ZERO,
        };
        self.last_pos = Some(pos);
        self.window.push((dt, delta));
        // Trim from the head to the trailing window (the reference's Σ(Δtick) ≤ 149 ms).
        let mut total: f32 = self.window.iter().map(|(d, _)| d).sum();
        while self.window.len() > 1 && total - self.window[0].0 >= WIND_WINDOW_S {
            total -= self.window[0].0;
            self.window.remove(0);
        }
        let sum_dpos: Vec3 = self.window.iter().map(|(_, d)| *d).sum();
        // wind = Σ(Δpos) / ((Σ(Δms)+1)·0.001) — displacement over seconds, true yd/s.
        self.vel = sum_dpos / (total.mul_add(1000.0, 1.0) * 0.001);

        let mag2 = self.vel.length_squared();
        // wx_wind_angle's gate: the heading only re-derives while |wind| ≥ 1; through calm it
        // HOLDS (the mist frame doesn't spin on idle noise).
        if mag2 >= 1.0 {
            // wind_wow = (−vel.z, −vel.x) per the bevy→wow basis; heading = atan2(y, x).
            self.heading = (-self.vel.x).atan2(-self.vel.z);
        }

        // weather_wind_tilt (0x674a70): tilt = lerp(0°, 45°, sat(|wind.xy|/30)) about the
        // horizontal axis ⊥ the wind; below |wind|² < 0.001 no tilt at all. The handedness is
        // verified: the apex tips DOWNWIND (+wind = motion heading) — rain rushes into a
        // moving player.
        self.tilt = if mag2 < 0.001 {
            Quat::IDENTITY
        } else {
            let mag = mag2.sqrt();
            let tilt_deg = (mag / WIND_TILT_DIV).clamp(0.0, 1.0) * WIND_TILT_MAX_DEG;
            let axis = Vec3::new(self.vel.z, 0.0, -self.vel.x) / mag;
            Quat::from_axis_angle(axis, tilt_deg.to_radians())
        };
    }

    /// The held wind heading (`manager+0x78`, WoW frame). Starts 0 = world north.
    pub(crate) fn heading_wow(&self) -> f32 {
        self.heading
    }

    /// The mist spawn frame: a yaw about the vertical by **+heading**. `mist_spawn 0x67a990`
    /// feeds `−mgr+0x78` into the Z-rotation builder `0x7bdd60` — a wrapper around `0x7bdb00`,
    /// which is the NEGATED-handedness family (`M(axis, θ) = R_standard(axis, −θ)`, rf-mist-motion
    /// Q3) — so the true rotation is `R_standard(+Z_wow, +heading)`, and WoW +Z ↔ Bevy +Y carries
    /// the angle sign-intact. Net effect: the −1.57 base azimuth (south = anti-north) spins to
    /// **anti-player-motion** — the stream always blows into a moving player's face, and holds
    /// that axis when they stop. (The earlier `−heading` transcribed the builder's literal minus
    /// into Bevy's standard API — a mirror; the director saw it as lateral drift.)
    pub(crate) fn mist_yaw(&self) -> Quat {
        Quat::from_rotation_y(self.heading_wow())
    }
}

/// A WoW-frame horizontal azimuth (the spawn kernels' `(vx, vy) = (mag·sin a, mag·cos a)` pair)
/// as a Bevy unit direction: `wow (x,y,z) → bevy (−y, z, −x)` ⇒ `(sin a, cos a, 0) →
/// (−cos a, 0, −sin a)`.
pub(crate) fn wow_azimuth_to_bevy(a: f32) -> Vec3 {
    Vec3::new(-a.cos(), 0.0, -a.sin())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The inline drift-azimuth mapping must agree with the canonical WoW→Bevy basis —
    /// `(sin a, cos a, 0)_wow` is the spawn kernels' `(vx, vy)` pair, and the world-fixed
    /// −1.57 centre only means anything if the frame conversion is the terrain's.
    #[test]
    fn wow_azimuth_matches_coords() {
        for i in 0..16 {
            let a = i as f32 / 16.0 * std::f32::consts::TAU - std::f32::consts::PI;
            let via_coords = benilla_assets::coords::wow_to_bevy([a.sin(), a.cos(), 0.0]);
            assert!(
                (wow_azimuth_to_bevy(a) - via_coords).length() < 1e-6,
                "azimuth {a} maps to {:?}, coords say {via_coords:?}",
                wow_azimuth_to_bevy(a),
            );
        }
    }

    /// Calm start ⇒ heading 0 (the fallback facing) ⇒ an identity mist frame; walking
    /// re-derives the heading once `|wind| ≥ 1` (`wx_wind_angle`'s gate); standing still again
    /// HOLDS it — the mist frame must not snap back or spin on idle noise.
    #[test]
    fn heading_gates_and_holds() {
        let mut wind = WeatherWind::default();
        assert_eq!(wind.heading_wow(), 0.0);
        assert!(wind.mist_yaw().angle_between(Quat::IDENTITY) < 1e-6);
        // Walk Bevy +X at 8 yd/s for 0.3 s — the ~149 ms window reads ~8 yd/s.
        let dt = 1.0 / 60.0;
        for i in 0..18 {
            wind.update(Vec3::new(8.0 * dt * i as f32, 0.0, 0.0), dt);
        }
        assert!(wind.vel.length() > 1.0, "window should read ~8 yd/s");
        let moving = wind.heading_wow();
        // Bevy +X = WoW −Y ⇒ wind_wow = (0, −8) ⇒ heading −π/2.
        assert!((moving + std::f32::consts::FRAC_PI_2).abs() < 1e-3);
        // Stand still: the wind decays through the window, the heading holds.
        let last = wind.last_pos.unwrap();
        for _ in 0..60 {
            wind.update(last, dt);
        }
        assert!(wind.vel.length() < 0.01);
        assert_eq!(wind.heading_wow(), moving);
    }

    /// The mist stream is ANTI-player-motion (`0x67a990`: `−heading` through the negated
    /// builder `0x7bdd60`→`0x7bdb00` = `R_std(+Z, +heading)`): a player moving along +v gets
    /// the −1.57-base stream spun to blow along −v — into their face, passing the camera on
    /// both sides. A sign regression here reads as lateral side-to-side drift.
    #[test]
    fn mist_stream_blows_anti_motion() {
        let mut wind = WeatherWind::default();
        let dt = 1.0 / 60.0;
        // Walk Bevy +X (east) at 8 yd/s.
        for i in 0..18 {
            wind.update(Vec3::new(8.0 * dt * i as f32, 0.0, 0.0), dt);
        }
        let motion = Vec3::X;
        // The base azimuth −1.57 is the stream's centre; spin it by the mist frame.
        let stream = wind.mist_yaw() * wow_azimuth_to_bevy(-1.57);
        assert!(
            stream.dot(motion) < -0.99,
            "stream {stream:?} must oppose motion {motion:?}"
        );
    }
}
