//! **The reference FrameXML this client EXECUTES off the player's own patch chain**, rather than
//! shipping a copy of it — the mechanism half of decision 1751.
//!
//! ## The rule
//!
//! The end state for the in-game interface is the stock 1.12 FrameXML, run off the file the player
//! already owns. `assets/ui` is scaffolding: it retires file by file, and a migrated window means
//! *its stock XML + Lua run off the chain and our counterpart file is deleted* (1751 §2). Fidelity
//! by construction — the reference's text cannot drift from itself, and every frame name, id,
//! template and stratum an addon reaches for is right because it **is** the reference's.
//!
//! What stays ours permanently: the glue screens (0068 §8 — GlueXML is a separate engine surface
//! even in the real client), dev-only frames, and adapter shims only while a genuine engine
//! difference forces one.
//!
//! ## Where the list lives — the manifest, not a second list in Rust
//!
//! Until 1751 this module carried its own `SOURCED` array and ran it *before* `assets/ui`, which
//! was the only ordering a Lua-only mechanism could express. Sourcing **XML** needs a real
//! position in the load order instead: stock `ContainerFrame.xml` inherits `ItemButtonTemplate`,
//! `CooldownFrameTemplate`, `SmallMoneyFrameTemplate` and `UIPanelCloseButton`, so it has to load
//! *after* the files that declare them.
//!
//! So there is exactly one ordered list of what loads and when, and it is `assets/ui/benilla.toc`
//! — the manifest that already had that job. **A manifest entry carrying a path separator is
//! sourced off the chain; a bare filename is a file we ship** ([`is_chain_entry`]). Our tree is
//! flat, so the two can never be confused, and the migration reads as what it is: the line
//! `BagFrame.xml` becomes `Interface\FrameXML\ContainerFrame.xml`, and `BagFrame.xml` is deleted.
//!
//! Everything else — the XML parse, `<Include>` / `<Script file=>` resolution against the
//! document's own directory, chunk naming, the error reporting that reaches the player — is
//! [`super::addons::Addon`]'s, unchanged. This module is only a third [`super::addons::Source`]:
//! *the player's install*.
//!
//! ## Where a reference file and our own UI still collide, and which one wins
//!
//! **Order decides, and the manifest is the order.** A name defined by both goes to whichever line
//! is later. That is the whole rule; there is no precedence machinery. A file sourced *before* our
//! own (`PaperDollFrame.lua`, which is there for one frame-agnostic button family and collides on
//! eighteen names we drive ourselves) has its collisions overwritten by ours; a file sourced at the
//! position of the window it replaces owns its names outright, which is what migrating a window
//! means.
//!
//! **Nothing is stubbed silently.** A reference body that reaches for something this client does
//! not have raises, naming it — which is loud, correct, and strictly better than a no-op that
//! pretends (1203, 1205, 1211, 1230). The answer is to build the verb, or to adapt the body in one
//! of our own files and say why at the site.
//!
//! ## No install, no file
//!
//! A machine with no client data (CI, a bare checkout) simply does not get these files, and says so
//! once, loudly. It is the same condition under which `GlobalStrings` is absent, and the addon
//! survey already prints which mode it ran in for that reason. An install-less checkout cannot run
//! most meaningful UI tests anyway — the art, fonts and MPQs come from the install too — so tests
//! that need these files gate on the install like every other client-data test.

use std::sync::OnceLock;

use benilla_formats::Chain;
use benilla_ui::toc::Toc;
use bevy::prelude::*;

use super::addons::{Addon, Source};

/// The addon name the reference's own files load under — the reference's word for its interface.
///
/// It is not `Interface\AddOns\…` anything: FrameXML is not an addon, gets no `ADDON_LOADED`, and
/// an addon that derives its folder from a `debugstack` pattern (`"\\AddOns\\(.*)\\"` —
/// `benilla_ui::script::addon_chunk_name`'s reason for existing) must not match a FrameXML frame.
/// [`Addon::chunk_name`] is what keeps that true: a chain file's chunk is named after its own
/// chain path, which is exactly what the real client names it.
pub(super) const NAME: &str = "FrameXML";

/// Is this manifest entry **sourced off the player's chain**, rather than shipped by us?
///
/// The test is a path separator, and it is decidable because our own shipped tree is *flat*: every
/// `assets/ui` entry is a bare filename, and every chain entry is a full internal path
/// (`Interface\FrameXML\ContainerFrame.xml`). `manifest::tests` pins both halves so the day
/// somebody adds a subdirectory to `assets/ui` is a failing test rather than a file that silently
/// stops loading.
pub(super) fn is_chain_entry(entry: &str) -> bool {
    entry.contains('\\') || entry.contains('/')
}

/// The reference interface as an [`Addon`] whose files come off the chain — the peer of
/// [`Addon::builtin`], and the thing [`super::manifest`] hands a manifest's chain entries to.
///
/// `files` are full chain paths, so the addon's prefix is empty and each entry is already in its
/// source's path space.
pub(super) fn addon(files: Vec<String>) -> Addon {
    Addon::new(
        NAME.to_string(),
        Toc {
            directives: Vec::new(),
            files,
        },
        Source::Chain,
    )
}

/// One file's bytes, read off the player's installed patch chain by internal path.
///
/// **Bytes, not text** (1193): a `.lua` chunk goes to Lua as it sits in the archive, and only an
/// XML parse decodes — a `read_to_string` here would not make a cp1252 file lose a glyph, it would
/// make the file *not exist*.
pub(super) fn read(req: &str) -> Option<Vec<u8>> {
    let chain = chain()?;
    match chain.read(req) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            debug!("ui_script: {req} is not in the patch chain: {e:#}");
            None
        }
    }
}

/// The player's patch chain, opened once per process and cached.
///
/// Cached because the addon survey stands up 218 VMs and this would otherwise be per-VM work. A
/// process-local chain rather than the one [`benilla_assets`] holds: the interface loads from
/// places that have no Bevy world to ask (the tests, the addon harness, a bare `UiScript`), and
/// `Chain`'s reads are `&self` and lock-free, so a second handle costs the mount and nothing else.
fn chain() -> Option<&'static Chain> {
    static CHAIN: OnceLock<Option<Chain>> = OnceLock::new();
    CHAIN
        .get_or_init(|| {
            let Some(data) = benilla_formats::wow_data() else {
                warn!(
                    "ui_script: no client data — every interface file this client SOURCES off the \
                     player's install (benilla.toc's `Interface\\…` entries) is absent, so the \
                     windows they build do not exist and addons that call their globals will raise"
                );
                return None;
            };
            match benilla_formats::open_chain(&data) {
                Ok(chain) => Some(chain),
                Err(e) => {
                    error!("ui_script: opening the patch chain to source the reference UI: {e:#}");
                    None
                }
            }
        })
        .as_ref()
}

#[cfg(test)]
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use benilla_ui::script::UiScript;

    /// **A chain entry really loads off the player's install, and order really decides.**
    ///
    /// The two halves of this module's rule, asserted rather than described, because nothing else
    /// would notice if either flipped: a chain entry that silently resolved to nothing would leave
    /// its globals nil (the failure mode is a window that does not exist, not an error), and the
    /// collision direction is invisible until an addon calls the wrong body.
    ///
    /// Skips without client data, like every other test that reads the install.
    #[test]
    fn a_chain_entry_loads_and_the_later_line_owns_the_collision() {
        let _data = benilla_formats::wow_data_or_skip!();
        let mut s = UiScript::new().expect("VM");
        s.set_screen_size(1024.0, 768.0);

        let failures = super::super::manifest::load_default_ui(&s);
        assert!(failures.is_empty(), "the default UI: {failures:#?}");

        // The item-button family the `ContainerFrame.lua` line is there for — nothing but the
        // sourced file defines these.
        for name in [
            "ContainerFrameItemButton_OnEnter",
            "ContainerFrameItemButton_OnClick",
            "ContainerFrameItemButton_OnLoad",
            "ContainerFrameItemButton_OnUpdate",
            "KeyRingItemButton_OnClick",
        ] {
            assert!(
                s.eval::<bool>(&format!("return type({name}) == \"function\""))
                    .unwrap(),
                "{name} must come from the sourced reference file"
            );
        }
        // …and its constants, which the corpus reads directly.
        assert_eq!(s.eval::<i64>("return NUM_BAG_FRAMES").unwrap(), 4);
        assert_eq!(s.eval::<i64>("return NUM_CONTAINER_FRAMES").unwrap(), 12);
        // The `PaperDollFrame.lua` line's own reason to exist.
        assert!(s
            .eval::<bool>("return type(PaperDollItemSlotButton_OnLoad) == \"function\"")
            .unwrap());

        // Order decides: `PaperDollFrame.lua` is sourced ABOVE CharacterFrame.xml, so OUR body of
        // a colliding name is the live one. `PaperDollFrame_SetLevel` is in the 18-name overlap.
        assert!(
            s.eval::<bool>(
                "return type(PaperDollFrame_SetLevel) == \"function\" \
                 and BenillaPaperDollSlot_OnLoad ~= nil"
            )
            .unwrap(),
            "our character sheet's own bodies must still be the live ones"
        );
        assert!(s.errors().is_empty(), "{:#?}", s.errors());
    }

    /// **The migration readiness probe** — which stock FrameXML file could be swapped in *today*,
    /// asked of the running loader rather than guessed from a source scan.
    ///
    /// ```text
    /// cargo test -p benilla-app --lib chain_readiness_report -- --ignored --nocapture
    /// ```
    ///
    /// 1751 is a long migration — 88 manifest entries, three of them chain entries at the time this
    /// was written — and the expensive question at every step is *which window is ready*. Picking by
    /// eye means reading a stock file, listing the globals it calls, and grepping each one; that is
    /// slow, and it is wrong in both directions. It over-reports (a name that exists in a comment
    /// greps as present — `framexml-file-demand.py` states that crudeness about itself) and it
    /// under-reports the things a grep cannot see at all: an XML element type the loader does not
    /// build, a script handler nothing dispatches, an attribute silently dropped (1739 measured 151
    /// of those), a template inherited before its definer.
    ///
    /// So the probe does not analyse. It **loads the file** — the whole shipped manifest first, into
    /// a fresh VM, exactly as a real run does, and then the candidate off the chain on top — and
    /// reports what the loader and the VM actually said. That is ground truth: the same machinery
    /// that would run it for real, answering the same question, with no model of the engine in
    /// between that could be out of date.
    ///
    /// **What a clean line does and does not mean.** It means the file *loads* — every element
    /// built, every template resolved, every load-time body ran without raising. It does not mean
    /// the window *works*: a verb that only a click reaches is not exercised by loading, and neither
    /// is anything behind an event. Clean is "start here", not "done"; the window's own test module
    /// and the director's eye are what finish it (§7).
    ///
    /// **Loading on top of the manifest, not instead of it**, because that is the position a
    /// migrated file occupies — every template it inherits is declared by an earlier entry, and
    /// asking whether `ContainerFrame.xml` loads *alone* only measures that it has predecessors.
    ///
    /// ## The false positive this method has, and how to recognise it
    ///
    /// A candidate whose frame NAMES our own shipped file already owns produces failures that are
    /// artefacts of the probe, not of the window. `publish_global` is deliberately non-overwriting
    /// (RF-0023), so the second frame to claim a name gets a wrapper that `_G` never points at —
    /// and any reference body using the `getglobal(this:GetName())` idiom then reads a DIFFERENT
    /// table than the `this` it just wrote to.
    ///
    /// That is exactly what the money frames look like: stock `TradeFrame.xml` reports
    /// `MoneyFrame_Update: attempt to index local 'info'`, because `MoneyFrame_SetType` set
    /// `this.info` on the new frame and `MoneyFrame_Update` read it back off OUR TradeFrame's
    /// same-named one. Delete our counterpart — which is what migrating the window does — and the
    /// collision goes with it. The same shape covers `MailFrame` and `QuestLogFrame`.
    ///
    /// **So a failure inside a name our own manifest also declares is suspect and has to be
    /// re-measured with the counterpart removed.** A failure naming something nothing of ours
    /// declares (`CreateFrame: unknown frame type 'LootButton'`, `attempt to call global
    /// 'UnitFrame_Initialize'`) is real. The probe does not tell the two apart for you; the
    /// question to ask of every line is "does our tree already own this name?".
    ///
    /// Ignored because it stands up ~90 fresh VMs and each one loads the entire interface; it is an
    /// instrument you run when choosing the next window, not a gate.
    #[test]
    #[ignore = "instrument: run by hand when choosing the next window to migrate"]
    fn chain_readiness_report() {
        let _data = benilla_formats::wow_data_or_skip!();

        // The reference's OWN order, off the chain — never a hand-kept list here. A file's position
        // in it is also the answer to "where does its manifest line go", so the report prints it.
        let toc = String::from_utf8_lossy(
            &super::read("Interface\\FrameXML\\FrameXML.toc").expect("the reference's own toc"),
        )
        .into_owned();
        let stock: Vec<String> = toc
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#') && l.ends_with(".xml"))
            .map(str::to_string)
            .collect();

        let migrated: Vec<String> = super::super::addons::Addon::builtin()
            .toc
            .files
            .iter()
            .filter(|f| super::is_chain_entry(f))
            .map(|f| {
                f.rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(f.as_str())
                    .to_string()
            })
            .collect();

        println!(
            "\n=== 1751 migration readiness — {} stock windows ===",
            stock.len()
        );
        println!(
            "{:>3}  {:<32} what stops it (empty = loads clean)",
            "pos", "file"
        );

        let mut clean = Vec::new();
        for (i, name) in stock.iter().enumerate() {
            let pos = i + 1;
            if migrated.iter().any(|m| m == name) {
                println!("{pos:>3}  {name:<32} — already migrated");
                continue;
            }
            let mut s = UiScript::new().expect("VM");
            s.set_screen_size(1024.0, 768.0);
            let base = super::super::manifest::load_default_ui(&s);
            assert!(base.is_empty(), "the shipped manifest itself: {base:#?}");
            s.resolve();
            let before = s.errors().len();

            let path = format!("Interface\\FrameXML\\{name}");
            let addon = super::addon(vec![path.clone()]);
            let mut said = addon.load_files(&s, std::slice::from_ref(&path));
            s.resolve();
            said.extend(s.errors().into_iter().skip(before));

            if said.is_empty() {
                clean.push((pos, name.clone()));
                println!("{pos:>3}  {name:<32} CLEAN");
            } else {
                // One line per distinct complaint, deduped and truncated: the same missing verb
                // reported by twelve frames is one fact, and the tail of a Lua traceback is noise.
                let mut seen: Vec<String> = Vec::new();
                for e in said {
                    let one = e.lines().next().unwrap_or("").trim().to_string();
                    let one = if one.len() > 140 {
                        format!("{}…", &one[..140])
                    } else {
                        one
                    };
                    if !one.is_empty() && !seen.contains(&one) {
                        seen.push(one);
                    }
                }
                println!("{pos:>3}  {name:<32} {} issue(s)", seen.len());
                for one in seen.iter().take(6) {
                    println!("         · {one}");
                }
                if seen.len() > 6 {
                    println!("         · … and {} more", seen.len() - 6);
                }
            }
        }

        println!("\n=== loads clean today: {} ===", clean.len());
        for (pos, name) in &clean {
            println!("  {pos:>3}  {name}");
        }
    }

    /// **The readiness probe's companion: not "does it load" but "what would I have to BUILD".**
    ///
    /// ```text
    /// cargo test -p benilla-app --lib chain_gap_report -- --ignored --nocapture
    /// ```
    ///
    /// [`chain_readiness_report`] answers one question well and is silent on the next one. A window
    /// it calls CLEAN can still be a week of work (its verbs are only reached by a click, which
    /// loading never makes), and a window it reports failing may be blocked on a single name. When
    /// the migration ran out of drop-in windows, "which of these is actually cheap" became the
    /// question, and the probe could not answer it.
    ///
    /// So this one reads the calls instead of running them. For every stock window not yet in the
    /// manifest it collects the `Name(` sites across the file and its `.lua`, subtracts what the
    /// file defines itself and what this client already has, and splits the remainder against
    /// **the reference's own `_G`** (`reference/1.12-globals.tsv`, captured from the running
    /// client):
    ///
    /// * `engine=` — the reference has it as an engine binding and we do not. **Real work**, and
    ///   the only column worth planning from.
    /// * `fx=` — the reference has it as a FrameXML function. Cheap by comparison: it lives in
    ///   some stock file, and the name beside it says which, so sourcing that file may be the whole
    ///   fix. `GetText` looked like an engine binding for an hour and turned out to be
    ///   `LocaleProperties.lua`; this column is that lesson, mechanised.
    /// * A name in NEITHER is dropped, and that is the load-bearing filter: 1.12's widget methods
    ///   do not live in `_G`, so `SetText(` and `Hide(` and their two hundred siblings would
    ///   otherwise drown the report. Anything the reference's own global table does not carry is
    ///   not a global.
    ///
    /// **What "already has" means, and why it is asked of a LOADED VM.** An earlier hand-rolled
    /// version of this compared against a bare `UiScript::new()`, which is the ENGINE surface
    /// alone — so every FrameXML function our own interface defines (`ShowUIPanel`,
    /// `StaticPopup_Visible`, `UpdateMicroButtons`, …) read as missing, and the `fx=` column was
    /// mostly noise. Here the manifest is loaded first and `_G` is read after, so the answer is
    /// what this client *actually* answers to.
    ///
    /// Two crudenesses, stated because they decide how to read the output. The `Name(` scan counts
    /// a call inside an XML comment (1.12 comments out whole blocks — `GuildRegistrarFrame`'s
    /// tabard button is one), so it can over-report; and it cannot see a name reached through
    /// `getglobal`, so it can under-report. It is for ranking work, not for proving a window done —
    /// [`chain_readiness_report`] and the window's own tests are that.
    #[test]
    #[ignore = "instrument: run by hand when choosing what to build next"]
    fn chain_gap_report() {
        let _data = benilla_formats::wow_data_or_skip!();

        // The reference's own global table, with each name's origin.
        let tsv = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../reference/1.12-globals.tsv"
        );
        let text = std::fs::read_to_string(tsv).expect("the reference surface");
        let mut origin: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let mut f = line.split('\t');
            if let (Some(name), Some(_kind), Some(from)) = (f.next(), f.next(), f.next()) {
                origin.insert(name, from);
            }
        }

        // What this client answers to with its whole interface up — engine bindings AND every
        // global our own FrameXML defines.
        let mut s = UiScript::new().expect("VM");
        s.set_screen_size(1024.0, 768.0);
        let failures = super::super::manifest::load_default_ui(&s);
        assert!(failures.is_empty(), "the shipped manifest: {failures:#?}");
        let have: std::collections::HashSet<String> = s
            .eval::<Vec<String>>(
                "local t = {} for k in pairs(_G) do table.insert(t, k) end return t",
            )
            .expect("dump _G")
            .into_iter()
            .collect();

        let migrated: std::collections::HashSet<String> = super::super::addons::Addon::builtin()
            .toc
            .files
            .iter()
            .filter(|f| super::is_chain_entry(f))
            .filter_map(|f| f.rsplit(['\\', '/']).next().map(str::to_string))
            .collect();

        let toc = String::from_utf8_lossy(
            &super::read("Interface\\FrameXML\\FrameXML.toc").expect("the reference's own toc"),
        )
        .into_owned();
        let stock: Vec<String> = toc
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#') && l.ends_with(".xml"))
            .map(str::to_string)
            .collect();

        // `function Name(` across the whole corpus, so an fx gap can name the file that holds it.
        let mut home: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for f in &stock {
            for cand in [f.clone(), format!("{}.lua", &f[..f.len() - 4])] {
                let Some(bytes) = super::read(&format!("Interface\\FrameXML\\{cand}")) else {
                    continue;
                };
                for line in String::from_utf8_lossy(&bytes).lines() {
                    if let Some(rest) = line.trim_start().strip_prefix("function ") {
                        let name: String = rest
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty() {
                            home.entry(name).or_insert_with(|| cand.clone());
                        }
                    }
                }
            }
        }

        let called = |text: &str| -> std::collections::HashSet<String> {
            let b: Vec<char> = text.chars().collect();
            let mut out = std::collections::HashSet::new();
            let mut i = 0;
            while i < b.len() {
                if b[i].is_ascii_uppercase() && (i == 0 || !super::is_word(b[i - 1])) {
                    let mut j = i;
                    while j < b.len() && super::is_word(b[j]) {
                        j += 1;
                    }
                    let mut k = j;
                    while k < b.len() && b[k] == ' ' {
                        k += 1;
                    }
                    if k < b.len() && b[k] == '(' {
                        out.insert(b[i..j].iter().collect::<String>());
                    }
                    i = j;
                    continue;
                }
                i += 1;
            }
            out
        };

        println!("\n=== 1751 gap report — what each unmigrated window would cost ===");
        let mut rows: Vec<(usize, String, Vec<String>, Vec<String>)> = Vec::new();
        for f in &stock {
            if migrated.contains(f) {
                continue;
            }
            let mut text = String::new();
            for cand in [f.clone(), format!("{}.lua", &f[..f.len() - 4])] {
                if let Some(b) = super::read(&format!("Interface\\FrameXML\\{cand}")) {
                    text.push_str(&String::from_utf8_lossy(&b));
                }
            }
            let own: std::collections::HashSet<String> = text
                .lines()
                .filter_map(|l| l.trim_start().strip_prefix("function "))
                .map(|r| {
                    r.chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect::<String>()
                })
                .collect();
            let (mut eng, mut fx) = (Vec::new(), Vec::new());
            let mut names: Vec<String> = called(&text)
                .into_iter()
                .filter(|c| !own.contains(c) && !have.contains(c))
                .collect();
            names.sort();
            for c in names {
                match origin.get(c.as_str()).copied() {
                    Some("engine") => eng.push(c),
                    Some(_) => {
                        let h = home.get(&c).cloned().unwrap_or_else(|| "?".into());
                        fx.push(format!("{c}<{h}>"));
                    }
                    None => {} // a widget method, not a global — the reference's _G would have it
                }
            }
            rows.push((eng.len(), f.clone(), eng, fx));
        }
        rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        for (n, f, eng, fx) in &rows {
            println!("{n:>3} engine  {f}");
            if !eng.is_empty() {
                println!("            engine: {}", eng.join(" "));
            }
            if !fx.is_empty() {
                println!("            fx:     {}", fx.join(" "));
            }
        }
        let free: Vec<&String> = rows.iter().filter(|r| r.0 == 0).map(|r| &r.1).collect();
        println!(
            "\n=== {} windows need NO engine work at all ===",
            free.len()
        );
        for f in free {
            println!("  {f}");
        }
    }

    /// A path is a chain entry; a bare name is ours. The one-line rule the manifest rests on.
    #[test]
    fn a_separator_is_what_makes_an_entry_the_players_own_file() {
        assert!(super::is_chain_entry(
            "Interface\\FrameXML\\ContainerFrame.xml"
        ));
        assert!(super::is_chain_entry(
            "Interface/FrameXML/ContainerFrame.xml"
        ));
        assert!(!super::is_chain_entry("BagFrame.xml"));
        assert!(!super::is_chain_entry("Fonts.xml"));
    }
}
