//! **The reference's frame FLAGS are the test oracle** — the flag half of decision 0675's "the
//! reference file is the test oracle", and the gate for the class decision 1739 closed.
//!
//! `framexml_diff` guards the *numbers* a transcribed window carries. Nothing guarded its *flags*,
//! and they went quietly wrong at scale: on 2026-08-30 a sweep found **47 frames** the reference
//! marks `toplevel` and ours did not (so no window ever came to the front), **23** the reference
//! makes mouse-interactive and ours did not (so a click on a window's own background fell through
//! to the 3D world), and **81** carrying a reference `id=` ours dropped (so `GetID()` — the
//! contract every 1.12 addon reads a slot index out of — answered 0). Every gate was green
//! throughout, because a flag is invisible to a behavioural test that never presses on the frame.
//!
//! ## Two properties make this a guard rather than a comfort
//!
//! **Our side is read from the ENGINE, never from our XML.** An attribute-vs-attribute diff is
//! precisely what let the class hide, twice over: benilla renames templates (`ChatFrameTemplate` →
//! `BenillaChatFrameTemplate`), so a name-keyed text diff reports a gap of zero on a window that is
//! entirely missing the flag; and the `<Scripts>` **auto-enable** law (wow-re
//! `ui/scratch/scripts-auto-enable.md` §1, VERIFIED — an `<OnEnter>`/`<OnLeave>`/`<OnMouseDown>`/
//! `<OnMouseUp>`/`<OnDragStart>` reaches the same enable primitive `0x76af00(2,-1)` the attribute
//! does) makes `enableMouse=` a poor proxy for whether the frame actually takes the mouse. Asking
//! the loaded engine — `IsToplevel()`, `IsMouseEnabled()`, `GetID()`, `GetParent()` — is immune to
//! both, and to a third: a `parent=` can arrive from a template or from nesting, and `GetParent()`
//! answers the same whichever way it came.
//!
//! **Divergences are an explicit list with a reason each, never a pattern.** [`KNOWN`] carries
//! them, in both directions; a new one cannot hide inside a tolerance. The list is where the
//! seven frames the reference makes interactive through *handlers we do not carry* are recorded —
//! those want the handler (and its tooltip), never a bare `enableMouse="true"` that would swallow
//! the click and give nothing back.
//!
//! ## Four flags, and only four
//!
//! `toplevel`, the effective mouse enable, `id` and the **parent** are mechanical: the reference's
//! value is right for any frame we transcribe, and a difference is a defect. The neighbours are
//! **not**, and are deliberately out of scope rather than silently tolerated — `movable` is an
//! inert flag for all but two reference frames (nothing calls `StartMoving` on the rest, so
//! copying it would be cargo-cult), `frameStrata` decides what draws over what and is the
//! director's call, `setAllPoints` has an exact `<Size>`+`<Anchors>` equivalent that five of our
//! pages deliberately use, and `hidden` is equivalent whenever the reference hides in `OnLoad`
//! instead. Decision 1739 carries the reasoning per attribute.
//!
//! **The parent joined them at decision 1757, and it is the flag with teeth.** Until 1734 restored
//! `SetFullScreenFrame`'s `UIParent:Hide()`, a `parent=` was little more than a coordinate space;
//! after it, the seat decides whether a frame can be on screen *at all* while a fullscreen panel
//! is up. 1734 swept the direction it went looking for — 72 declarations the reference has and we
//! had dropped — and nothing asked the opposite question, so the frames we parent that the
//! reference leaves top-level went the other way in silence: opening the world map hid the map's
//! own blackout (the 3D world came back through the margins beside the 4:3 sheet — the director
//! reported it), the shared dropdown lists could not open over it, and the screenshot
//! confirmation could not appear during the fly-by whose SCREENSHOT key `CinematicFrame` hands
//! back by name. A gate that reads one direction of a two-directional property is a gate someone
//! has to remember to run backwards.
//!
//! Both ways a frame acquires a parent count, because the XML makes them look unrelated: the
//! `parent=` attribute — **on the element or on a template it inherits**, which is where the
//! reference keeps the chat frames' — and nesting inside another frame's `<Frames>`. Reading only
//! instances reports all fourteen chat frames as top-level; reading only attributes reports
//! `ScreenshotStatus` as parentless when the reference nests it in `WorldFrame` for the property
//! `WorldFrame.xml`'s header states outright: *"Children of the world frame are visible even when
//! the UI is turned off."*
//!
//! ## What it can and cannot see
//!
//! The population is the shipped tree's **named, published frames**, so a virtual template is not
//! compared directly — but every instance of one is, carrying the template's resolved flags, which
//! is the same coverage by a different route (a `KNOWN` entry naming a template therefore reads as
//! stale and is refused).
//!
//! The reference side is **XML only**: a `SetID` or `EnableMouse` the reference makes from Lua at
//! `OnLoad` is invisible here and reads as absent. That blind spot only ever under-reports — it
//! can hide a divergence, never invent one — so nothing it misses turns into a false failure; the
//! handful of frames it does hide are in `KNOWN` with that reason.
//!
//! The whole module skips cleanly with no install — `_extracted_framexml/` is a gitignored
//! Blizzard asset, like every other client-data test here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use benilla_ui::framexml::{self, Element, TopLevel};

/// Element tags that are *frames* — the ones with flags to compare. Regions (`<Texture>`,
/// `<FontString>`) carry names too and must not shadow a frame of the same name.
const FRAME_TAGS: &[&str] = &[
    "Frame",
    "Button",
    "CheckButton",
    "EditBox",
    "ScrollFrame",
    "Slider",
    "StatusBar",
    "MessageFrame",
    "ScrollingMessageFrame",
    "Model",
    "PlayerModel",
    "DressUpModel",
    "TabardModel",
    "ColorSelect",
    "SimpleHTML",
    "GameTooltip",
    "Minimap",
    "MovieFrame",
    "WorldFrame",
    "Cooldown",
];

/// The five `<Scripts>` handler names that auto-enable the MOUSE kind, and only those five
/// (wow-re `ui/scratch/scripts-auto-enable.md` §1's kind-2 OR-chain, `0x769fb7`..`0x76a022`).
/// `OnDragStop`/`OnReceiveDrag` bind a slot and trip no enable — they are deliberately absent.
const MOUSE_HANDLERS: &[&str] = &[
    "OnEnter",
    "OnLeave",
    "OnMouseDown",
    "OnMouseUp",
    "OnDragStart",
];

/// Widget kinds whose **constructor** enables the mouse, so an element of that tag needs no
/// `enableMouse` and no handler to be clickable.
///
/// This is deliberately the SAME list [`benilla_ui::widget::WidgetArena::create`] uses, and it has
/// to be: both are one claim about the client's ctors (`CSimpleButton`/`CSimpleEditBox` enable
/// input in theirs; a `Slider` thumb must be draggable; a scroll frame takes the wheel — each
/// carries its own citation there). Reading the engine's list here means a wrong entry is wrong in
/// one place rather than two, and the sweep never reports a divergence that is only the two models
/// disagreeing with each other.
///
/// It is also what answers the chat window: `ChatFrame1` is a `ScrollingMessageFrame`, so the
/// reference's takes the mouse from its ctor even though `FloatingChatFrameTemplate` writes
/// `enableMouse="false"` — which is a no-op, the XML path only ever ENABLES
/// (`scripts-auto-enable.md` §1.3). That is how a 1.12 chat link is clickable at all.
const MOUSE_BY_CTOR: &[&str] = &[
    "Button",
    "CheckButton",
    "EditBox",
    "ScrollingMessageFrame",
    "ScrollFrame",
    "Slider",
];

/// One accepted difference between benilla and the reference, with the reason it is accepted.
///
/// `frame` is benilla's name for it; `flag` is which of the four; `why` is why the difference is
/// right (or, for the handler gaps, why the honest fix is not this flag).
struct Known {
    frame: &'static str,
    flag: Flag,
    why: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flag {
    Toplevel,
    Mouse,
    Id,
    Parent,
}

impl std::fmt::Display for Flag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Flag::Toplevel => "toplevel",
            Flag::Mouse => "mouse",
            Flag::Id => "id",
            Flag::Parent => "parent",
        })
    }
}

/// The accepted differences, in four groups: **seven frames the reference makes mouse-interactive
/// through HANDLERS we do not carry**, the merchant rows whose mouse we take where it does not,
/// the faux scroll panes we build from a different widget kind, and the ids the reference sets
/// from Lua. Every entry is a judgement someone made; none of them is a tolerance.
const KNOWN: &[Known] = &[
    // ── The reference has it, we do not: all seven are handler gaps, not flag gaps ─────────────
    //
    // In each of these the reference's mouse comes from an `<OnEnter>`/`<OnLeave>` pair whose body
    // we have not built, and the interaction the player would notice is the TOOLTIP those handlers
    // show — not the click-blocking the flag gives. Declaring `enableMouse="true"` here would make
    // the frame swallow the click and hand back nothing, which is worse than the gap.
    Known {
        frame: "PetPaperDollFrameExpBar",
        flag: Flag::Mouse,
        why:
            "the reference bar inherits TextStatusBar, whose OnEnter/OnLeave show the value text; \
              ours is a plain StatusBar. Wants the status-text arc, not the flag.",
    },
    Known {
        frame: "TargetofTargetHealthBar",
        flag: Flag::Mouse,
        why: "same as PetPaperDollFrameExpBar — a plain StatusBar where the reference inherits \
              TextStatusBar.",
    },
    Known {
        frame: "TargetofTargetManaBar",
        flag: Flag::Mouse,
        why: "same as PetPaperDollFrameExpBar — a plain StatusBar where the reference inherits \
              TextStatusBar.",
    },
    Known {
        frame: "TradePlayerItem7",
        flag: Flag::Mouse,
        why:
            "the enchant slot: ours inherits BenillaTradeEnchantItemTemplate, the reference's the \
              ordinary PlayerTradeItemTemplate. A structural difference in our own trade window.",
    },
    Known {
        frame: "WhoFrameDropDown",
        flag: Flag::Mouse,
        why: "the reference's /who sort dropdown carries its own handlers over the shared \
              UIDropDownMenuTemplate; ours takes the template alone.",
    },
    Known {
        frame: "WorldStateAlwaysUpFrame",
        flag: Flag::Mouse,
        why:
            "the reference's PvP objective banner has OnEnter/OnLeave for its tooltip. Wants that \
              handler, not the flag.",
    },
    // ── We take the mouse where the reference does not ─────────────────────────────────────────
    //
    // The merchant rows: ours are one mouse-taking row each; the reference splits every row into
    // an inert container plus a `$parentItemButton` that takes the mouse on its behalf. Same
    // clicks, a different frame under them.
    Known {
        frame: "MerchantItem1",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem2",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem3",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem4",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem5",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem6",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem7",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem8",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem9",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem10",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem11",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantItem12",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "MerchantBuyBackItem",
        flag: Flag::Mouse,
        why: "inherits BenillaMerchantItemTemplate",
    },
    Known {
        frame: "HonorFrameProgressBar",
        flag: Flag::Mouse,
        why: "our honour bar carries its own hover tooltip; the reference's page has none",
    },
    Known {
        frame: "WorldMapFrame",
        flag: Flag::Mouse,
        why: "our map body takes the mouse so a click on it cannot reach the world behind a \
              FULLSCREEN_DIALOG window; the reference relies on WorldMapButton alone",
    },
    // ── The faux scroll panes: a different WIDGET KIND, not a missing flag ─────────────────────
    //
    // The reference declares each of these `<ScrollFrame …inherits="FauxScrollFrameTemplate">` and
    // takes the mouse from that kind's constructor, even though a faux pane never really scrolls —
    // the kind is chosen for the wheel and the scrollbar plumbing. benilla's `FauxScrollFrameTemplate`
    // is a plain `<Frame>` and wires the wheel explicitly, with an `<OnMouseWheel>` on the pane
    // routed through `BenillaFauxScrollFrame_OnMouseWheel` (FriendsFrame.xml's own note at the
    // friends pane). Same behaviour — the list scrolls under the wheel — reached a different way,
    // and the reference's own faux panes use nothing else the mouse flag gives.
    //
    // The two mail panes are a further step out: ours are flat art with a FontString body, a
    // render approximation MailFrame.xml names at the site, so neither side scrolls them.
    Known {
        frame: "FriendsFrameFriendsScrollFrame",
        flag: Flag::Mouse,
        why: "our faux scroll pane is a Frame with an explicit <OnMouseWheel>, not a ScrollFrame",
    },
    Known {
        frame: "FriendsFrameIgnoreScrollFrame",
        flag: Flag::Mouse,
        why: "as FriendsFrameFriendsScrollFrame",
    },
    Known {
        frame: "GuildListScrollFrame",
        flag: Flag::Mouse,
        why: "as FriendsFrameFriendsScrollFrame",
    },
    Known {
        frame: "WhoListScrollFrame",
        flag: Flag::Mouse,
        why: "as FriendsFrameFriendsScrollFrame",
    },
    Known {
        frame: "SkillListScrollFrame",
        flag: Flag::Mouse,
        why: "as FriendsFrameFriendsScrollFrame",
    },
    Known {
        frame: "ReputationListScrollFrame",
        flag: Flag::Mouse,
        why: "as FriendsFrameFriendsScrollFrame",
    },
    Known {
        frame: "QuestLogListScrollFrame",
        flag: Flag::Mouse,
        why: "as FriendsFrameFriendsScrollFrame",
    },
    // ── id ─────────────────────────────────────────────────────────────────────────────────────
    //
    // Empty, and it is worth saying why rather than leaving a bare header. The reference side of
    // this comparison is XML only: the sweep cannot see a `SetID` the reference makes from Lua at
    // `OnLoad`, so a frame the reference numbers *there* used to read as 0 here while ours
    // declared the number in XML. The five entries that lived here were all one case — the bag
    // bar's `CharacterBag0..3Slot` and `KeyRingButton`.
    //
    // 1751's third window deleted them by making the difference not exist: the bar IS
    // `Interface\FrameXML\MainMenuBarBagButtons.xml` now, so those ids come from
    // `PaperDollItemSlotButton_OnLoad`'s `GetInventorySlotInfo` and from `this:SetID(
    // KEYRING_CONTAINER)` — the reference's own Lua, on both sides. This gate reported them the
    // frame it was built to report (1751 §5: the drift instruments retire with the copies).
    // ── parent ─────────────────────────────────────────────────────────────────────────────────
    //
    // Three, and each is a seat inside the SAME tree the reference seats it in — which is the
    // question this flag exists to ask (decision 1757). A frame whose seat crosses the boundary
    // between UIParent's tree and the top level is a defect, because `SetFullScreenFrame` hides
    // `UIParent` and everything below it; a frame seated one rung along inside that tree is not.
    Known {
        frame: "FriendsDropDown",
        flag: Flag::Parent,
        why: "the reference nests this menu HOST in FriendsFrame; ours sits on UIParent in \
              ItemRef.xml, beside the SetItemRef branch that is its only caller — and it has to, \
              because FriendsFrame.xml loads 262 lines further down the manifest and a forward \
              parent reference resolves to nothing. Both seats are inside UIParent's tree, and a \
              host frame is never shown: ToggleDropDownMenu seats the visible DropDownList1, \
              which is top-level on both sides.",
    },
    Known {
        frame: "SendMailBodyEditBox",
        flag: Flag::Parent,
        why: "the reference interposes SendMailScrollChildFrame between the pane and its content; \
              our mail panes are the render approximation MailFrame.xml names at the site (flat \
              art, no live scrollbar), so the body hangs off SendMailScrollFrame directly. Wants \
              the real scroll child, not a re-seat.",
    },
    Known {
        frame: "OpenMailInvoiceFrame",
        flag: Flag::Parent,
        why: "as SendMailBodyEditBox — OpenMailScrollChildFrame is the scroll child we do not \
              build",
    },
];

/// The extracted reference FrameXML directory, or `None` when the install isn't there.
fn reference_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../WoW/_extracted_framexml");
    dir.is_dir().then_some(dir)
}

/// Every named *frame* element in the reference corpus, keyed by name — templates and instances
/// alike, nested `<Frames>` included, `$parent`-relative names excluded (they repeat across
/// templates and name nothing on their own).
fn reference_frames() -> Option<HashMap<String, Element>> {
    let dir = reference_dir()?;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml")))
        .collect();
    paths.sort();

    let mut out: HashMap<String, Element> = HashMap::new();
    for path in paths {
        // Blizzard ships a UTF-8 BOM on some of these and stray high bytes in comments; the parse
        // is what matters, so read lossily rather than refusing the file.
        let bytes = std::fs::read(&path).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let Ok(doc) = framexml::parse(text.trim_start_matches('\u{feff}')) else {
            continue;
        };
        for item in &doc.items {
            if let TopLevel::Template(el) | TopLevel::Instance(el) = item {
                collect_named(el, &mut out);
            }
        }
    }
    Some(out)
}

/// Recurse an element tree, publishing every named frame. First name wins, matching the client's
/// auto-publish rule.
fn collect_named(el: &Element, out: &mut HashMap<String, Element>) {
    if FRAME_TAGS.iter().any(|t| t.eq_ignore_ascii_case(&el.tag)) {
        if let Some(name) = el.attr("name") {
            if !name.contains("$parent") {
                out.entry(name.to_string()).or_insert_with(|| el.clone());
            }
        }
    }
    for child in &el.children {
        collect_named(child, out);
    }
}

/// Every named reference frame's **enclosing frame**, if it is nested inside one.
///
/// A frame acquires its parent one of two ways, and the XML gives no hint that they are the same
/// question: `parent="X"` (on the element, or on a template it inherits — see [`resolved_attr`]),
/// or *nesting* inside another frame's `<Frames>`. The reference uses both heavily —
/// `DropDownList1` is top-level with neither, `ScreenshotStatus` carries no attribute at all and
/// is nested in `WorldFrame` — so reading only one of them is worse than reading neither. This
/// half is the nesting; `resolved_attr` is the other, and the attribute wins where both exist.
/// `enclosing` is the nearest named frame ancestor, threaded down the walk.
fn collect_nesting(
    el: &Element,
    enclosing: Option<&str>,
    out: &mut HashMap<String, Option<String>>,
) {
    let mut inner = enclosing;
    if FRAME_TAGS.iter().any(|t| t.eq_ignore_ascii_case(&el.tag)) {
        if let Some(name) = el.attr("name") {
            if !name.contains("$parent") {
                out.entry(name.to_string())
                    .or_insert_with(|| enclosing.map(str::to_string));
                inner = Some(name);
            }
        }
    }
    for child in &el.children {
        collect_nesting(child, inner, out);
    }
}

/// The reference's nesting table, built over the same corpus [`reference_frames`] reads.
fn reference_nesting() -> Option<HashMap<String, Option<String>>> {
    let dir = reference_dir()?;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml")))
        .collect();
    paths.sort();

    let mut out: HashMap<String, Option<String>> = HashMap::new();
    for path in paths {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let Ok(doc) = framexml::parse(text.trim_start_matches('\u{feff}')) else {
            continue;
        };
        for item in &doc.items {
            if let TopLevel::Template(el) | TopLevel::Instance(el) = item {
                collect_nesting(el, None, &mut out);
            }
        }
    }
    Some(out)
}

/// The templates `el` inherits, **right to left** — a later name in the list overrides an earlier
/// one, so the search for an inherited value has to try the last one first.
fn inherits(el: &Element) -> impl Iterator<Item = &str> {
    el.attr("inherits")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
}

/// An attribute's value on `name`, following `inherits=`; the element's own value wins over any
/// template's. `depth` guards a malformed cycle (the shipped corpus has none).
fn resolved_attr<'a>(
    name: &str,
    frames: &'a HashMap<String, Element>,
    attr: &str,
    depth: u32,
) -> Option<&'a str> {
    if depth > 16 {
        return None;
    }
    let el = frames.get(name)?;
    if let Some(v) = el.attr(attr) {
        return Some(v);
    }
    inherits(el).find_map(|t| resolved_attr(t, frames, attr, depth + 1))
}

/// Whether any element in `name`'s inherits chain declares one of `handlers` inside `<Scripts>`.
fn declares_handler(
    name: &str,
    frames: &HashMap<String, Element>,
    handlers: &[&str],
    depth: u32,
) -> bool {
    if depth > 16 {
        return false;
    }
    let Some(el) = frames.get(name) else {
        return false;
    };
    let own = el
        .children
        .iter()
        .filter(|c| c.tag.eq_ignore_ascii_case("Scripts"))
        .flat_map(|s| s.children.iter())
        .any(|h| handlers.iter().any(|w| w.eq_ignore_ascii_case(&h.tag)));
    own || inherits(el).any(|t| declares_handler(t, frames, handlers, depth + 1))
}

/// Whether the reference's frame of this name **takes the mouse** once loaded — the three ways
/// `0x76af00(2, -1)` is reached, per `scripts-auto-enable.md` §1.3: the widget's own ctor, the
/// `enableMouse` attribute, or an auto-enabling `<Scripts>` handler.
fn reference_takes_mouse(name: &str, frames: &HashMap<String, Element>) -> bool {
    let Some(el) = frames.get(name) else {
        return false;
    };
    if MOUSE_BY_CTOR
        .iter()
        .any(|t| t.eq_ignore_ascii_case(&el.tag))
    {
        return true;
    }
    if resolved_attr(name, frames, "enableMouse", 0).is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    declares_handler(name, frames, MOUSE_HANDLERS, 0)
}

/// The reference name for one of ours: the same name, or the one behind our `Benilla` prefix
/// (`BenillaChatFrameTemplate` is the reference's `ChatFrameTemplate` — the rename that made a
/// text diff report a gap of zero on a window missing every flag).
fn reference_name<'a>(ours: &str, frames: &'a HashMap<String, Element>) -> Option<&'a str> {
    if let Some((k, _)) = frames.get_key_value(ours) {
        return Some(k);
    }
    let bare = ours.strip_prefix("Benilla")?;
    frames.get_key_value(bare).map(|(k, _)| k.as_str())
}

/// One divergence found by the sweep, rendered for the failure message.
fn describe(frame: &str, flag: Flag, ours: &str, theirs: &str) -> String {
    format!("  {frame} — {flag}: ours {ours}, reference {theirs}")
}

/// **Every frame both UIs name carries the reference's `toplevel`, mouse enable, `id` and
/// parent** — read off the loaded engine, with [`KNOWN`] the only accepted differences.
///
/// Verified to fail: delete `toplevel="true"` from `CharacterFrame.xml` and this names
/// `CharacterFrame`; delete `id="1"` from `ActionButton1` and it names that; put
/// `parent="UIParent"` back on `BlackoutWorld` and it names that.
#[test]
fn the_shipped_frames_carry_the_references_flags() {
    let Some(reference) = reference_frames() else {
        return; // no install — the same skip every client-data test here takes
    };
    let nesting = reference_nesting().expect("the same corpus the frames came from");
    assert!(
        reference.len() > 500,
        "only {} reference frames parsed — the corpus scan broke, and a sweep over nothing \
         passes whatever we did",
        reference.len()
    );

    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "manifest load errors: {failures:#?}");
    s.resolve();

    // The loaded frame's own parent, by name — `GetParent()`, not the `parent=` attribute, so a
    // frame that acquires its parent by NESTING reads the same as one that declares it. An empty
    // string is a top-level frame.
    let parent_of = |n: &str| -> Option<String> {
        s.eval::<String>(&format!(
            "local f = getglobal(\"{n}\") if not f or not f.GetParent then return \"\" end \
             local p = f:GetParent() return (p and p:GetName()) or \"\""
        ))
        .ok()
        .filter(|v| !v.is_empty())
    };

    // Read the flags off the ENGINE, not off our XML — the whole point of the module.
    let flags = |n: &str| -> Option<(bool, bool, i64)> {
        s.eval::<i64>(&format!(
            "local f = getglobal(\"{n}\") \
             if not f or not f.IsToplevel then return -1 end \
             return (f:IsToplevel() and 1 or 0) + (f:IsMouseEnabled() and 2 or 0)"
        ))
        .ok()
        .filter(|v| *v >= 0)
        .map(|v| {
            let id = s
                .eval::<i64>(&format!("return getglobal(\"{n}\"):GetID()"))
                .unwrap_or(0);
            (v & 1 == 1, v & 2 == 2, id)
        })
    };

    let mut divergences: Vec<String> = Vec::new();
    let mut compared = 0usize;
    // Every KNOWN entry starts unclaimed; a real divergence claims it. What is left at the end
    // is an entry describing a difference that no longer exists.
    let mut unused: Vec<&Known> = KNOWN.iter().collect();

    let mut ours: Vec<String> = super::shipped_xml_tests::shipped_frame_names();
    ours.sort();
    for name in &ours {
        let Some(theirs) = reference_name(name, &reference) else {
            continue;
        };
        let Some((toplevel, mouse, id)) = flags(name) else {
            continue; // not a frame in the loaded tree (a template, or a region name)
        };
        compared += 1;

        let want_toplevel = resolved_attr(theirs, &reference, "toplevel", 0)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let want_mouse = reference_takes_mouse(theirs, &reference);
        let want_id: i64 = resolved_attr(theirs, &reference, "id", 0)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        // The declared parent wins over the nesting, and `resolved_attr` follows `inherits=` —
        // which is not a detail: the reference puts the chat frames' `parent="UIParent"` on
        // `FloatingChatFrameTemplate`/`ChatTabTemplate`, and reading instances alone would report
        // all fourteen as top-level. That template blind spot is the one 1734's own gap analysis
        // fell into (commit "the chat templates carry parent=UIParent").
        //
        // Parent names are then compared through the same rename map the frames themselves are: a
        // benilla-only prefix on the PARENT would otherwise report every child of it as diverged.
        let want_parent = resolved_attr(theirs, &reference, "parent", 0)
            .map(str::to_string)
            .or_else(|| nesting.get(theirs).cloned().flatten());
        let parent = parent_of(name);
        let parent_matches = match (&parent, &want_parent) {
            (None, None) => true,
            (Some(p), Some(w)) => p == w || p.strip_prefix("Benilla") == Some(w.as_str()),
            _ => false,
        };
        let show = |p: &Option<String>| p.clone().unwrap_or_else(|| "(top-level)".into());

        for (flag, differs, ours_s, theirs_s) in [
            (
                Flag::Toplevel,
                toplevel != want_toplevel,
                toplevel.to_string(),
                want_toplevel.to_string(),
            ),
            (
                Flag::Mouse,
                mouse != want_mouse,
                mouse.to_string(),
                want_mouse.to_string(),
            ),
            (Flag::Id, id != want_id, id.to_string(), want_id.to_string()),
            (
                Flag::Parent,
                !parent_matches,
                show(&parent),
                show(&want_parent),
            ),
        ] {
            if !differs {
                continue;
            }
            if KNOWN.iter().any(|k| k.frame == name && k.flag == flag) {
                unused.retain(|u| !(u.frame == name && u.flag == flag));
                continue;
            }
            divergences.push(describe(name, flag, &ours_s, &theirs_s));
        }
    }

    assert!(
        compared > 400,
        "only {compared} frames compared — the pairing broke, and the sweep guards nothing"
    );
    assert!(
        divergences.is_empty(),
        "{} frame flag(s) diverge from the reference. Each is either a defect to fix or an entry \
         to add to KNOWN with the reason it is right — never a tolerance:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
    // A KNOWN entry that no longer describes a real difference is stale documentation claiming a
    // divergence that has been fixed — the exact rot this module exists to prevent elsewhere.
    let stale: Vec<String> = unused
        .iter()
        .map(|k| format!("  {} ({}) — claimed: {}", k.frame, k.flag, k.why))
        .collect();
    assert!(
        stale.is_empty(),
        "{} KNOWN entr(y/ies) no longer describe a real difference — delete them, an accepted \
         divergence that has been fixed is documentation claiming a defect we do not have:\n{}",
        stale.len(),
        stale.join("\n")
    );
}
