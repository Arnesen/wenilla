//! The server config files, edited in place. Values are single tokens on `Key = value` lines
//! (the `Motd` string is the quoted exception); a write is a temp file + rename in the same
//! directory, so a crash never leaves a half-written config for mangosd to read.
//!
//! Which keys apply live: `reload config` re-reads everything except `WorldServerPort`,
//! `GameType`, `RealmZone`, `MaxPlayerLevel`, `GuidReserveSize.*` and `DataDir`
//! (`World::LoadConfigSettings(reload)`); `AiPlayerbot.*` counts apply on `rndbot reload`, but a
//! count above `RandomBotAccountCount` needs a restart; anything in `realmd.conf` needs realmd
//! restarted.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

#[derive(Clone, Debug)]
pub struct ConfFiles {
    pub mangosd: PathBuf,
    pub aiplayerbot: PathBuf,
    pub ahbot: PathBuf,
}

impl ConfFiles {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            mangosd: dir.join("mangosd.conf"),
            aiplayerbot: dir.join("aiplayerbot.conf"),
            ahbot: dir.join("ahbot.conf"),
        }
    }
}

/// `Key = value` → `value` (the last matching line wins, like the parser).
pub fn read(path: &Path, key: &str) -> Result<Option<String>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(read_str(&text, key))
}

pub fn read_str(text: &str, key: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        if let Some(v) = parse_line(line, key) {
            found = Some(v);
        }
    }
    found
}

fn parse_line(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let v = rest.trim();
    let v = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v);
    Some(v.to_string())
}

/// Replace (or append) `Key = value`. `quoted` wraps the value in double quotes (`Motd`).
pub fn write(path: &Path, key: &str, value: &str, quoted: bool) -> Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let out = write_str(&text, key, value, quoted)?;
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, out).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn write_str(text: &str, key: &str, value: &str, quoted: bool) -> Result<String> {
    if quoted {
        if value.contains('"') || value.contains('\n') {
            return Err(anyhow!("value may not contain quotes or newlines"));
        }
    } else if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(anyhow!("value must be a single token"));
    }
    let line = if quoted {
        format!("{key} = \"{value}\"")
    } else {
        format!("{key} = {value}")
    };
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    let mut replaced = false;
    for l in text.lines() {
        if parse_line(l, key).is_some() {
            out.push_str(&line);
            replaced = true;
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if !replaced {
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// What applying a key needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apply {
    /// `reload config`
    Hot,
    /// `rndbot reload`
    Bots,
    /// `ahbot reload`
    AhBot,
    /// a world-server restart
    Restart,
}

pub const XP_KEYS: [&str; 3] = ["Rate.XP.Kill", "Rate.XP.Quest", "Rate.XP.Explore"];
pub const LOOT_KEYS: [&str; 8] = [
    "Rate.Drop.Item.Poor",
    "Rate.Drop.Item.Normal",
    "Rate.Drop.Item.Uncommon",
    "Rate.Drop.Item.Rare",
    "Rate.Drop.Item.Epic",
    "Rate.Drop.Item.Legendary",
    "Rate.Drop.Item.Artifact",
    "Rate.Drop.Item.Referenced",
];
pub const MONEY_KEY: &str = "Rate.Drop.Money";

/// How a gameplay tweak is edited.
#[derive(Clone, Copy, Debug)]
pub enum TweakKind {
    /// A number written to every key; `int` keys are integers in the core.
    Rate { min: f64, max: f64, int: bool },
    /// Checked writes `on` to every key, unchecked writes `off`; the box shows checked when the
    /// first key reads `on`.
    Toggle { on: &'static str, off: &'static str },
}

/// One extra mangosd.conf switch. Every one of these is re-read by `reload config` *and* read by
/// the core each time it is used (traced in mangos-classic at the core pin), so it applies live
/// without a restart; `default` is the core's own default, used when the key is absent.
#[derive(Clone, Copy, Debug)]
pub struct Tweak {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub keys: &'static [&'static str],
    pub default: &'static str,
    pub kind: TweakKind,
}

pub const TWEAKS: &[Tweak] = &[
    Tweak {
        id: "regen_rate",
        group: "rates",
        label: "Health & mana regen",
        hint: "players, pets and mobs",
        keys: &["Rate.Health", "Rate.Mana"],
        default: "1",
        kind: TweakKind::Rate {
            min: 0.1,
            max: 20.0,
            int: false,
        },
    },
    Tweak {
        id: "rep_rate",
        group: "rates",
        label: "Reputation",
        hint: "",
        keys: &["Rate.Reputation.Gain"],
        default: "1",
        kind: TweakKind::Rate {
            min: 0.1,
            max: 20.0,
            int: false,
        },
    },
    Tweak {
        id: "profession_gain",
        group: "rates",
        label: "Profession skill-ups",
        hint: "points per craft or gather",
        keys: &["SkillGain.Crafting", "SkillGain.Gathering"],
        default: "1",
        kind: TweakKind::Rate {
            min: 1.0,
            max: 10.0,
            int: true,
        },
    },
    Tweak {
        id: "aggro_rate",
        group: "rates",
        label: "Mob aggro range",
        hint: "0 = mobs never attack first",
        keys: &["Rate.Creature.Aggro"],
        default: "1",
        kind: TweakKind::Rate {
            min: 0.0,
            max: 2.0,
            int: false,
        },
    },
    Tweak {
        id: "fall_damage",
        group: "rates",
        label: "Fall damage",
        hint: "0 = none",
        keys: &["Rate.Damage.Fall"],
        default: "1",
        kind: TweakKind::Rate {
            min: 0.0,
            max: 5.0,
            int: false,
        },
    },
    Tweak {
        id: "gear_wear",
        group: "rules",
        label: "Gear wears down when hit (death still costs durability)",
        hint: "",
        keys: &["DurabilityLossChance.Damage"],
        default: "0.5",
        kind: TweakKind::Toggle {
            on: "0.5",
            off: "0",
        },
    },
    Tweak {
        id: "res_sickness",
        group: "rules",
        label: "Resurrection sickness at the spirit healer",
        hint: "",
        keys: &["Death.SicknessLevel"],
        default: "11",
        kind: TweakKind::Toggle {
            on: "11",
            off: "99",
        },
    },
    Tweak {
        id: "ignore_instance_reqs",
        group: "rules",
        label: "Anyone may enter dungeons and raids (no level or raid-group requirement)",
        hint: "",
        keys: &["Instance.IgnoreLevel", "Instance.IgnoreRaid"],
        default: "0",
        kind: TweakKind::Toggle { on: "1", off: "0" },
    },
    Tweak {
        id: "cross_faction",
        group: "rules",
        label: "Cross-faction grouping, chat, trade, mail, /who and friends",
        hint: "",
        keys: &[
            "AllowTwoSide.Interaction.Group",
            "AllowTwoSide.Interaction.Chat",
            "AllowTwoSide.Interaction.Trade",
            "AllowTwoSide.Interaction.Mail",
            "AllowTwoSide.WhoList",
            "AllowTwoSide.AddFriend",
        ],
        default: "0",
        kind: TweakKind::Toggle { on: "1", off: "0" },
    },
    Tweak {
        id: "show_low_quests",
        group: "rules",
        label: "Show grey (low-level) quests in the world",
        hint: "",
        keys: &["Quests.LowLevelHideDiff"],
        default: "5",
        kind: TweakKind::Toggle { on: "-1", off: "5" },
    },
];

/// A tweak and its current value, as the settings page renders it.
pub struct TweakRow {
    pub t: &'static Tweak,
    pub value: f64,
}

impl TweakRow {
    pub fn is_rate(&self) -> bool {
        matches!(self.t.kind, TweakKind::Rate { .. })
    }
    pub fn checked(&self) -> bool {
        self.value != 0.0
    }
    pub fn min(&self) -> f64 {
        match self.t.kind {
            TweakKind::Rate { min, .. } => min,
            TweakKind::Toggle { .. } => 0.0,
        }
    }
    pub fn max(&self) -> f64 {
        match self.t.kind {
            TweakKind::Rate { max, .. } => max,
            TweakKind::Toggle { .. } => 1.0,
        }
    }
    pub fn step(&self) -> &'static str {
        match self.t.kind {
            TweakKind::Rate { int: true, .. } | TweakKind::Toggle { .. } => "1",
            TweakKind::Rate { int: false, .. } => "0.1",
        }
    }
}

/// The current value of each tweak: the rate, or 1.0/0.0 for a toggle.
fn load_tweaks(m: &str) -> std::collections::BTreeMap<String, f64> {
    TWEAKS
        .iter()
        .map(|t| {
            let raw = read_str(m, t.keys[0]).unwrap_or_else(|| t.default.to_string());
            let v = match t.kind {
                TweakKind::Rate { .. } => raw.parse().unwrap_or(1.0),
                TweakKind::Toggle { on, .. } => {
                    if raw == on {
                        1.0
                    } else {
                        0.0
                    }
                }
            };
            (t.id.to_string(), v)
        })
        .collect()
}

/// The whole settings surface the panel exposes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub xp_rate: f64,
    pub loot_rate: f64,
    pub money_rate: f64,
    pub player_limit: i64,
    pub save_interval_secs: i64,
    pub max_player_level: i64,
    pub motd: String,
    pub bots: i64,
    pub bot_account_count: i64,
    pub ahbot: bool,
    /// [`TWEAKS`] by id.
    #[serde(default)]
    pub tweaks: std::collections::BTreeMap<String, f64>,
}

impl Settings {
    pub fn tweak_rows(&self, group: &str) -> Vec<TweakRow> {
        TWEAKS
            .iter()
            .filter(|t| t.group == group)
            .map(|t| TweakRow {
                t,
                value: self.tweaks.get(t.id).copied().unwrap_or(0.0),
            })
            .collect()
    }
}

fn num(text: &str, key: &str, default: f64) -> f64 {
    read_str(text, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn load(files: &ConfFiles) -> Result<Settings> {
    let m = std::fs::read_to_string(&files.mangosd)
        .with_context(|| format!("reading {}", files.mangosd.display()))?;
    let b = std::fs::read_to_string(&files.aiplayerbot).unwrap_or_default();
    let a = std::fs::read_to_string(&files.ahbot).unwrap_or_default();
    let bots_enabled = read_str(&b, "AiPlayerbot.Enabled")
        .map(|v| v != "0")
        .unwrap_or(false);
    Ok(Settings {
        xp_rate: num(&m, "Rate.XP.Kill", 1.0),
        loot_rate: num(&m, "Rate.Drop.Item.Normal", 1.0),
        money_rate: num(&m, MONEY_KEY, 1.0),
        player_limit: num(&m, "PlayerLimit", 100.0) as i64,
        save_interval_secs: (num(&m, "PlayerSave.Interval", 900_000.0) / 1000.0) as i64,
        max_player_level: num(&m, "MaxPlayerLevel", 60.0) as i64,
        motd: read_str(&m, "Motd").unwrap_or_default(),
        bots: if bots_enabled {
            num(&b, "AiPlayerbot.MaxRandomBots", 0.0) as i64
        } else {
            0
        },
        bot_account_count: num(&b, "AiPlayerbot.RandomBotAccountCount", 50.0) as i64,
        ahbot: num(&a, "AuctionHouseBot.Chance.Sell", 0.0) > 0.0
            || num(&a, "AuctionHouseBot.Chance.Buy", 0.0) > 0.0,
        tweaks: load_tweaks(&m),
    })
}

fn fmt_rate(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Write `new` where it differs from `old`; returns the set of apply steps needed.
pub fn save(files: &ConfFiles, old: &Settings, new: &Settings) -> Result<Vec<Apply>> {
    let mut steps = Vec::new();
    let need = |a: Apply, steps: &mut Vec<Apply>| {
        if !steps.contains(&a) {
            steps.push(a)
        }
    };
    let rate_ok = |v: f64| (0.1..=20.0).contains(&v);
    if !rate_ok(new.xp_rate) || !rate_ok(new.loot_rate) || !rate_ok(new.money_rate) {
        return Err(anyhow!("rates must be between 0.1 and 20"));
    }
    if new.xp_rate != old.xp_rate {
        for k in XP_KEYS {
            write(&files.mangosd, k, &fmt_rate(new.xp_rate), false)?;
        }
        need(Apply::Hot, &mut steps);
    }
    if new.loot_rate != old.loot_rate {
        for k in LOOT_KEYS {
            write(&files.mangosd, k, &fmt_rate(new.loot_rate), false)?;
        }
        need(Apply::Hot, &mut steps);
    }
    if new.money_rate != old.money_rate {
        write(&files.mangosd, MONEY_KEY, &fmt_rate(new.money_rate), false)?;
        need(Apply::Hot, &mut steps);
    }
    if new.player_limit != old.player_limit {
        if !(1..=5000).contains(&new.player_limit) {
            return Err(anyhow!("player limit must be 1–5000"));
        }
        write(
            &files.mangosd,
            "PlayerLimit",
            &new.player_limit.to_string(),
            false,
        )?;
        need(Apply::Hot, &mut steps);
    }
    if new.save_interval_secs != old.save_interval_secs {
        if !(30..=3600).contains(&new.save_interval_secs) {
            return Err(anyhow!("save interval must be 30–3600 seconds"));
        }
        write(
            &files.mangosd,
            "PlayerSave.Interval",
            &(new.save_interval_secs * 1000).to_string(),
            false,
        )?;
        need(Apply::Hot, &mut steps);
    }
    if new.max_player_level != old.max_player_level {
        if !(1..=60).contains(&new.max_player_level) {
            return Err(anyhow!("max level must be 1–60"));
        }
        write(
            &files.mangosd,
            "MaxPlayerLevel",
            &new.max_player_level.to_string(),
            false,
        )?;
        need(Apply::Restart, &mut steps);
    }
    if new.motd != old.motd {
        write(&files.mangosd, "Motd", &new.motd, true)?;
        need(Apply::Hot, &mut steps);
    }
    if new.bots != old.bots {
        if new.bots < 0 || new.bots > 2000 {
            return Err(anyhow!("bot count must be 0–2000"));
        }
        write(
            &files.aiplayerbot,
            "AiPlayerbot.Enabled",
            if new.bots > 0 { "1" } else { "0" },
            false,
        )?;
        if new.bots > 0 {
            write(
                &files.aiplayerbot,
                "AiPlayerbot.MinRandomBots",
                &new.bots.to_string(),
                false,
            )?;
            write(
                &files.aiplayerbot,
                "AiPlayerbot.MaxRandomBots",
                &new.bots.to_string(),
                false,
            )?;
        }
        if new.bots > old.bot_account_count {
            write(
                &files.aiplayerbot,
                "AiPlayerbot.RandomBotAccountCount",
                &new.bots.to_string(),
                false,
            )?;
            need(Apply::Restart, &mut steps);
        }
        if (old.bots == 0) != (new.bots == 0) {
            need(Apply::Restart, &mut steps);
        } else {
            need(Apply::Bots, &mut steps);
        }
    }
    if new.ahbot != old.ahbot {
        let (sell, buy) = if new.ahbot {
            ("100", "100")
        } else {
            ("0", "0")
        };
        write(&files.ahbot, "AuctionHouseBot.Chance.Sell", sell, false)?;
        write(&files.ahbot, "AuctionHouseBot.Chance.Buy", buy, false)?;
        need(Apply::AhBot, &mut steps);
    }
    for t in TWEAKS {
        let (Some(&v), o) = (new.tweaks.get(t.id), old.tweaks.get(t.id)) else {
            continue;
        };
        if o == Some(&v) {
            continue;
        }
        let value = match t.kind {
            TweakKind::Rate { min, max, int } => {
                if !(min..=max).contains(&v) {
                    return Err(anyhow!("{} must be between {min} and {max}", t.label));
                }
                if int {
                    (v.round() as i64).to_string()
                } else {
                    fmt_rate(v)
                }
            }
            TweakKind::Toggle { on, off } => (if v != 0.0 { on } else { off }).to_string(),
        };
        for k in t.keys {
            write(&files.mangosd, k, &value, false)?;
        }
        need(Apply::Hot, &mut steps);
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# comment\nRate.XP.Kill    = 1\nRate.XP.Quest   = 1\nMotd = \"Welcome\"\nPlayerLimit = 100\n";

    #[test]
    fn reads_tokens_and_quoted() {
        assert_eq!(read_str(SAMPLE, "Rate.XP.Kill").as_deref(), Some("1"));
        assert_eq!(read_str(SAMPLE, "Motd").as_deref(), Some("Welcome"));
        assert_eq!(read_str(SAMPLE, "Rate.XP").as_deref(), None);
        assert_eq!(read_str(SAMPLE, "Nope"), None);
    }

    #[test]
    fn writes_replace_and_append() {
        let out = write_str(SAMPLE, "Rate.XP.Kill", "3", false).unwrap();
        assert!(out.contains("Rate.XP.Kill = 3\n"));
        assert!(!out.contains("Rate.XP.Kill    = 1"));
        let out = write_str(&out, "Motd", "Hello there", true).unwrap();
        assert_eq!(read_str(&out, "Motd").as_deref(), Some("Hello there"));
        let out = write_str(&out, "New.Key", "7", false).unwrap();
        assert!(out.ends_with("New.Key = 7\n"));
        assert!(write_str(SAMPLE, "PlayerLimit", "1 2", false).is_err());
    }

    #[test]
    fn save_reports_apply_steps() {
        let dir = tempfile::tempdir().unwrap();
        let files = ConfFiles::in_dir(dir.path());
        std::fs::write(&files.mangosd, "Rate.XP.Kill = 1\nRate.XP.Quest = 1\nRate.XP.Explore = 1\nRate.Drop.Item.Normal = 1\nRate.Drop.Money = 1\nPlayerLimit = 100\nPlayerSave.Interval = 900000\nMaxPlayerLevel = 60\nMotd = \"Hi\"\n").unwrap();
        std::fs::write(&files.aiplayerbot, "AiPlayerbot.Enabled = 1\nAiPlayerbot.MinRandomBots = 50\nAiPlayerbot.MaxRandomBots = 50\nAiPlayerbot.RandomBotAccountCount = 50\n").unwrap();
        std::fs::write(
            &files.ahbot,
            "AuctionHouseBot.Chance.Sell = 0\nAuctionHouseBot.Chance.Buy = 0\n",
        )
        .unwrap();
        let old = load(&files).unwrap();
        assert_eq!(old.xp_rate, 1.0);
        assert_eq!(old.bots, 50);
        assert!(!old.ahbot);
        let mut new = old.clone();
        new.xp_rate = 2.0;
        new.bots = 20;
        new.max_player_level = 40;
        let steps = save(&files, &old, &new).unwrap();
        assert_eq!(steps, vec![Apply::Hot, Apply::Restart, Apply::Bots]);
        let re = load(&files).unwrap();
        assert_eq!(re, new);
        assert_eq!(
            read(&files.mangosd, "Rate.XP.Explore").unwrap().as_deref(),
            Some("2")
        );
    }

    #[test]
    fn tweaks_default_when_absent_and_write_every_key() {
        let dir = tempfile::tempdir().unwrap();
        let files = ConfFiles::in_dir(dir.path());
        std::fs::write(
            &files.mangosd,
            "Rate.Health = 1\nDeath.SicknessLevel = 11\n",
        )
        .unwrap();
        let old = load(&files).unwrap();
        assert_eq!(old.tweaks["regen_rate"], 1.0);
        assert_eq!(old.tweaks["res_sickness"], 1.0);
        assert_eq!(old.tweaks["cross_faction"], 0.0);
        assert_eq!(
            old.tweaks["gear_wear"], 1.0,
            "absent key reads the core default"
        );
        let mut new = old.clone();
        new.tweaks.insert("regen_rate".into(), 2.5);
        new.tweaks.insert("res_sickness".into(), 0.0);
        new.tweaks.insert("cross_faction".into(), 1.0);
        new.tweaks.insert("profession_gain".into(), 3.0);
        assert_eq!(save(&files, &old, &new).unwrap(), vec![Apply::Hot]);
        assert_eq!(load(&files).unwrap(), new);
        let r = |k| read(&files.mangosd, k).unwrap();
        assert_eq!(r("Rate.Mana").as_deref(), Some("2.5"));
        assert_eq!(r("Death.SicknessLevel").as_deref(), Some("99"));
        assert_eq!(r("AllowTwoSide.AddFriend").as_deref(), Some("1"));
        assert_eq!(r("SkillGain.Gathering").as_deref(), Some("3"));
        assert_eq!(
            r("DurabilityLossChance.Damage"),
            None,
            "untouched tweak is not written"
        );
        let mut bad = new.clone();
        bad.tweaks.insert("aggro_rate".into(), 3.0);
        assert!(save(&files, &new, &bad).is_err());
    }
}
