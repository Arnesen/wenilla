//! The `.toc` manifest parser — the load list + metadata of every addon, including Blizzard's own
//! (`FrameXML.toc` and the `Blizzard_*` AddOns are ordinary manifests; third-party addons are the
//! identical mechanism — decision 0068).
//!
//! Grammar, shared by 1.12 and Era manifests (Era only adds directive *keys*, not syntax):
//! - `## Key: Value` — a directive. Keys compare case-insensitively; localized variants carry a
//!   locale suffix (`## Title-deDE: …`). Era manifests list several client builds in one
//!   `## Interface: 11507, 11508`.
//! - `#` (not followed by another `#`) — a comment.
//! - anything else non-blank — a file to load, in order (`.lua`/`.xml`, `\` or `/` separators).
//!
//! The parser is lossless about order (directives keep file order for duplicate keys; files keep
//! load order) and byte-tolerant: UTF-8 BOM, `\r\n`, and stray whitespace are the norm in shipped
//! manifests, not the exception.

/// A parsed `.toc` manifest: ordered directives + the ordered file load list.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Toc {
    /// `## Key: Value` directives in file order, keys as written (compare via [`Toc::directive`]).
    pub directives: Vec<(String, String)>,
    /// Files to load, in order, exactly as written (path separators are normalized at *load* time,
    /// not here — the manifest is quoted verbatim in errors and tooling).
    pub files: Vec<String>,
}

impl Toc {
    /// Parse manifest text. Never fails: unrecognizable lines are file entries by definition of
    /// the grammar, and an empty input is an empty manifest.
    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut toc = Toc::default();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(directive) = line.strip_prefix("##") {
                // `##` alone (or without a colon) is treated as a comment, matching the client's
                // tolerance for malformed directives.
                if let Some((key, value)) = directive.split_once(':') {
                    let key = key.trim();
                    if !key.is_empty() {
                        toc.directives
                            .push((key.to_string(), value.trim().to_string()));
                    }
                }
            } else if !line.starts_with('#') {
                toc.files.push(line.to_string());
            }
        }
        toc
    }

    /// The first directive whose key matches `key` case-insensitively. Duplicate keys: we read the
    /// first occurrence (client behavior on duplicates is unverified — flag for the loader work;
    /// shipped manifests don't duplicate keys).
    pub fn directive(&self, key: &str) -> Option<&str> {
        self.directives
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// `## Interface:` as client build numbers. Era manifests list several (`11507, 11508`);
    /// 1.12's lists one (`11200`). Unparseable entries are skipped, not errors.
    pub fn interface_versions(&self) -> Vec<u32> {
        self.directive("Interface")
            .map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect())
            .unwrap_or_default()
    }

    /// `## Interface:` **as the 1.12 client reads it** (decision 1292): the leading integer of
    /// the value, and a manifest with no `## Interface` line (or a non-numeric one) is `0`.
    /// Byte-verified (wow-re `addon-version-gate.md` §2.2): `Toc_Parse 0x51c9b0` stores
    /// `SStrToInt` of the value at `[rec+0x1c]`, the record ctor leaves it `0`, and the version
    /// gate compares that single dword — so an Era manifest's `11507, 11508` reads as `11507`,
    /// exactly as the reference would read it. [`Toc::interface_versions`] stays for display;
    /// the GATE runs on this.
    pub fn interface_version(&self) -> u32 {
        self.directive("Interface")
            .map(|v| {
                let digits: String = v.trim().chars().take_while(char::is_ascii_digit).collect();
                digits.parse().unwrap_or(0)
            })
            .unwrap_or(0)
    }

    /// A comma-separated list directive (`SavedVariables`, `OptionalDeps`, `Dependencies`, …),
    /// split and trimmed. Missing directive = empty list.
    pub fn list(&self, key: &str) -> Vec<&str> {
        self.directive(key)
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Hard dependencies: the client accepts both `## Dependencies:` and `## RequiredDeps:`
    /// (and `## RequiredDep:`) as synonyms.
    pub fn dependencies(&self) -> Vec<&str> {
        for key in ["Dependencies", "RequiredDeps", "RequiredDep"] {
            let deps = self.list(key);
            if !deps.is_empty() {
                return deps;
            }
        }
        Vec::new()
    }

    /// Soft dependencies: `## OptionalDeps:` (and `## OptionalDep:` by symmetry with the required
    /// pair above).
    ///
    /// **They load FIRST and their failures are ignored** — `AddOn_Load 0x51f240`'s own order,
    /// quoted in 1191 §2 — which is what lets `## OptionalDeps: FuBar, Ace2` mean "if Ace2 is
    /// installed, its libraries are already global by the time my files run". **130 corpus addons
    /// declare them**, and the whole FuBar family leans on exactly that: `FuBar_BagFu`'s `.toc`
    /// lists `FuBarPlugin-2.0.lua` BEFORE `AceLibrary.lua`, which only works because the `Ace2`
    /// addon went first.
    ///
    /// **`## OptionalDependencies:` is NOT accepted here**, though 4 corpus addons write it. The
    /// long form is verified for the required half (`Dependencies`) and merely plausible for this
    /// one, and 1204's rule is to be lenient only where the grammar admits no ambiguity. Queued as
    /// an RE question with its cost attached: four addons.
    pub fn optional_dependencies(&self) -> Vec<&str> {
        for key in ["OptionalDeps", "OptionalDep"] {
            let deps = self.list(key);
            if !deps.is_empty() {
                return deps;
            }
        }
        Vec::new()
    }

    /// `## LoadOnDemand: 1` (Era loader; absent in 1.12 manifests).
    pub fn load_on_demand(&self) -> bool {
        self.directive("LoadOnDemand").map(str::trim) == Some("1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directives_files_and_comments() {
        let toc = Toc::parse(
            "\u{feff}## Interface: 11507, 11508\r\n\
             ## Title: Probe |cff00ff00Addon|r\r\n\
             ## SavedVariables: ProbeDB, ProbeSettings\r\n\
             ## X-Custom: anything: with colons\r\n\
             # a comment line\r\n\
             ##\r\n\
             ## malformed directive without colon\r\n\
             \r\n\
             Libs\\LibStub\\LibStub.lua\r\n\
             Core.lua\r\n\
             Frames.xml\r\n",
        );
        assert_eq!(toc.interface_versions(), vec![11507, 11508]);
        assert_eq!(toc.directive("title"), Some("Probe |cff00ff00Addon|r"));
        assert_eq!(toc.list("SavedVariables"), vec!["ProbeDB", "ProbeSettings"]);
        // Values keep everything after the first colon.
        assert_eq!(toc.directive("X-Custom"), Some("anything: with colons"));
        assert_eq!(
            toc.files,
            vec!["Libs\\LibStub\\LibStub.lua", "Core.lua", "Frames.xml"]
        );
        // Comments and malformed directives contribute nothing.
        assert_eq!(toc.directives.len(), 4);
    }

    #[test]
    fn vanilla_1_12_shape() {
        let toc = Toc::parse(
            "## Interface: 11200\n\
             ## Title: FrameXML\n\
             ## Secure: 1\n\
             GlobalStrings.lua\n\
             Fonts.xml\n",
        );
        assert_eq!(toc.interface_versions(), vec![11200]);
        assert_eq!(toc.directive("Secure"), Some("1"));
        assert!(!toc.load_on_demand());
        assert_eq!(toc.files.len(), 2);
    }

    #[test]
    fn dependency_synonyms_and_first_occurrence_wins() {
        let toc = Toc::parse(
            "## RequiredDeps: LibA, LibB\n\
             ## Title: first\n\
             ## Title: second\n\
             ## LoadOnDemand: 1\n",
        );
        assert_eq!(toc.dependencies(), vec!["LibA", "LibB"]);
        assert_eq!(toc.directive("Title"), Some("first"));
        assert!(toc.load_on_demand());
    }

    #[test]
    fn empty_and_degenerate_inputs() {
        assert_eq!(Toc::parse(""), Toc::default());
        let toc = Toc::parse("# only a comment\n\n");
        assert!(toc.directives.is_empty() && toc.files.is_empty());
    }

    /// Parses a REAL shipped manifest when `BENILLA_TOC` points at one (never committed — extract
    /// e.g. `Interface\FrameXML\FrameXML.toc` with benilla-extract and export the path). Skips
    /// silently otherwise, so CI/gates don't depend on client data.
    #[test]
    fn real_manifest_when_available() {
        let Ok(path) = std::env::var("BENILLA_TOC") else {
            return;
        };
        let text = std::fs::read_to_string(&path).expect("reading BENILLA_TOC");
        let toc = Toc::parse(&text);
        assert!(!toc.files.is_empty(), "a real manifest lists files");
        assert!(
            !toc.interface_versions().is_empty(),
            "a real manifest declares ## Interface"
        );
    }
}
