//! The sandbox + the WoW stdlib layer — the global aliases and helpers FrameXML/addon Lua assumes
//! exist on top of stock Lua 5.1 (decision 0068).
//!
//! - **Sandbox** ([`sandbox`]): remove `io`/`os`/`package`/`require`/`dofile`/`loadfile`/`debug`
//!   (keeping a `debugstack` stub returning `""`), and replace chunk loading with a **text-only**
//!   `loadstring` (bytecode rejected). Decision 0068's threat model is "addon-observable behavior",
//!   not anti-automation, but running untrusted addon Lua still means no filesystem/OS/native reach.
//! - **Stdlib** ([`install`]): the nine bare-global aliases probe A found in the real corpus
//!   (`format`/`strlen`/`gsub`/`strsub`/`strupper`/`tinsert`/`getn`/`tremove`/`strfind`), plus
//!   `strlower`/`strrep`, `getglobal`/`setglobal`, the `strsplit`/`strjoin`/`strtrim`/`strconcat`
//!   family, `wipe`, `tostringall`, and — the one non-trivial piece — a `string.format` replacement
//!   that supports Blizzard's positional `%N$` extension (probe A confirmed stock 5.1 rejects it).
//!
//! ## The positional `format` (`%N$`)
//!
//! Blizzard patched `string.format` to accept `printf`-style positional specs (`"%2$s %1$s"`) for
//! localization. Stock Lua 5.1 does not (probe A, `semantics.rs` check (b)). We reimplement the
//! reordering in Lua: scan the format, and if any spec carries `N$`, rewrite it into a plain
//! sequential format with the arguments reordered, then delegate to the real `string.format`.
//! **Mixing** positional and sequential specs in one string is an error — and it is in Blizzard's
//! build too (their patch tracks a single "arg cursor" that a positional spec desyncs), so we match
//! by erroring rather than guessing a blend. The wrapper is installed as both `string.format` and
//! the bare global `format`.

use mlua::{Lua, Value, Variadic};

/// Sandbox the VM: strip filesystem/OS/native reach, keep a `debugstack` stub, and make chunk
/// loading text-only.
pub(super) fn sandbox(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // Remove the dangerous surface. (Some may already be absent under mlua's safe stdlib; setting
    // nil is idempotent and makes the guarantee explicit rather than mlua-version-dependent.)
    for name in [
        "io", "os", "package", "require", "dofile", "loadfile", "load", "module", "newproxy",
        "debug",
    ] {
        g.set(name, Value::Nil)?;
    }

    // `debugstack([start [, count1 [, count2]]])` — a REAL traceback, not the `""` stub this used
    // to be.
    //
    // The stub was justified as "real addons call it in error paths; returning \"\" keeps them
    // running", and that was true of the addons that only ever DISPLAY it. It is false of the ones
    // that PARSE it, and the biggest family in the corpus is one of those: `FuBarPlugin-2.0.lua:752`
    // derives every plugin's own folder with
    // `string.find(debugstack(6, 1, 0), "\\AddOns\\(.*)\\")`, then formats the capture into an
    // icon path. Against `""` the find returns nil, `folderName` stays nil, and the plugin dies in
    // `format` — 20 corpus addons, every one of them a FuBar plugin, and the error surfaced as a
    // `format` complaint three frames away from the cause.
    //
    // `start` is the stack level to begin at and defaults to 1, matching the client's own callers
    // (`debugstack(6, 1, 0)` walks past FuBar's own wrapper frames to the plugin file). The two
    // count arguments bound how many frames are shown at the top and bottom; mlua's traceback does
    // not take them, so they are accepted and ignored — stated rather than silently dropped, and
    // harmless because every corpus caller passes a shape whose whole purpose is "give me the
    // frames", never an exact line count.
    g.set(
        "debugstack",
        lua.create_function(|lua, args: Variadic<Value>| {
            let start = match args.first() {
                Some(Value::Integer(i)) => *i as usize,
                Some(Value::Number(n)) => *n as usize,
                _ => 1,
            };
            // A level past the top of the stack is not an error here, it is an empty trace — the
            // same shape the old stub returned, so a caller that guessed too deep still gets a
            // string rather than a raise.
            Ok(lua
                .traceback(None, start)
                .and_then(|t| t.to_str().map(|s| s.to_owned()))
                .unwrap_or_default())
        })?,
    )?;

    // Text-only `loadstring`: compile a *source* string (never bytecode). Returns `function` on
    // success or `(nil, errmsg)` on failure — the stock `loadstring` contract. `set_mode(Text)`
    // makes mlua reject a binary chunk (the `\27Lua` signature), the "loadstring of bytecode
    // rejected" guarantee.
    // The BOM strip and the `#`-line skip ride along, because in the reference they live *inside*
    // `luaL_loadbuffer`/`luaX_setinput` — the same door `loadstring` goes through (decision 1193).
    let loadstring = lua.create_function(
        |lua, (src, chunkname): (mlua::String, Option<mlua::String>)| {
            let bytes = crate::source::chunk(&src.as_bytes()).to_vec();
            let name = match &chunkname {
                Some(n) => format!("={}", n.to_str()?),
                None => "=(loadstring)".to_string(),
            };
            let chunk = lua
                .load(bytes)
                .set_name(name)
                .set_mode(mlua::ChunkMode::Text);
            match chunk.into_function() {
                Ok(f) => Ok((Value::Function(f), Value::Nil)),
                Err(e) => Ok((Value::Nil, Value::String(lua.create_string(e.to_string())?))),
            }
        },
    )?;
    g.set("loadstring", loadstring)?;

    Ok(())
}

/// Install the WoW stdlib layer (aliases, `strsplit` family, positional `format`, …).
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    // The default `geterrorhandler()` reports into the same channel a failed handler uses, so an
    // addon's `geterrorhandler()(msg)` surfaces where every other script error does rather than
    // vanishing (decision 1195). Named with the `__benilla_` prefix because it is host plumbing,
    // not a 1.12 global — the reference's default handler is FrameXML's `_ERRORMESSAGE`.
    lua.globals().set(
        "__benilla_script_error",
        lua.create_function(|lua, msg: mlua::Value| {
            let text = match &msg {
                mlua::Value::String(s) => s.to_string_lossy(),
                other => format!("{other:?}"),
            };
            lua.app_data_mut::<super::Model>()
                .expect("model app_data")
                .errors
                .push(text);
            Ok(())
        })?,
    )?;

    install_time(lua)?;
    lua.load(WOW_STDLIB)
        .set_name("=[benilla wow stdlib]")
        .set_mode(mlua::ChunkMode::Text)
        .exec()?;
    // Remember the default handler BY IDENTITY, right after installing it — the engine-side
    // dispatch (decision 1305) skips it (it reports into the host channel, where every dispatched
    // message already is) and fires only a handler somebody *chose*: FrameXML's `_ERRORMESSAGE`
    // or an addon's own.
    let default: mlua::Function = lua.load("return geterrorhandler()").eval()?;
    lua.set_named_registry_value(super::REG_DEFAULT_ERRORHANDLER, default)?;
    Ok(())
}

/// The stdlib layer, in Lua. One place, so each alias is the one-liner the task calls for and the
/// `format`/`strsplit` logic is readable. Runs once at construction.
const WOW_STDLIB: &str = r#"
-- ── the bare string family ─────────────────────────────────────────────────────────────────────
-- Every name here is present in the REAL 1.12 client's global table — the in-world `_G` captured
-- from the running reference client (wow-5875-re's item-13 fixture, 19,572 entries), which is the
-- authority this file now answers to (decision 1189).
--
-- 1187 also installed `strmatch`, `strrev`, `gmatch`, `strlenutf8` and `strcmputf8i` from
-- Blizzard's *Era* enumeration, to get a Classic Era addon further. **None of them exist in
-- 1.12** — `string.match`/`gmatch` are Lua 5.1 additions the 5.0 client never had — and Era is
-- not our target. They are gone; adding a global the client does not have is how an addon that
-- feature-detects gets sent down a path we cannot honour.
strlen  = string.len
strsub  = string.sub
strupper = string.upper
strlower = string.lower
strrep  = string.rep
strfind = string.find
strbyte = string.byte
strchar = string.char
gsub    = string.gsub
tinsert = table.insert
tremove = table.remove
-- `getn`, `sort`, `foreach` and `foreachi` are bound by `lua50` (decision 1194), which owns the
-- 5.0 shapes of `table.getn`/`setn` and must not be shadowed by a second implementation here.

-- ── the bare math aliases (the same 1.12 global family; the reference FrameXML uses them
-- unqualified — WorldMapFrame's overlay pool calls ceil/mod, CooldownFrame floor, …). `mod` is
-- fmod under a 5.0-era name (stock 5.1 dropped math.mod).
ceil  = math.ceil
floor = math.floor
abs   = math.abs
max   = math.max
min   = math.min
sqrt  = math.sqrt
mod   = math.mod        -- 5.0's name for fmod; `math.fmod` is removed by `lua50` (decision 1194)
random = math.random
-- `randomseed` is an ENGINE global in 1.12 exactly like `random` beside it (the captured `_G` types
-- both `function engine`), and it is the half that was missing: `IgniteStatus` calls it at file
-- scope in its OnLoad and dies on `attempt to call global 'randomseed'`. Seeding a PRNG is the one
-- thing an addon does that has no FrameXML counterpart to copy, so the bare name is all it has.
randomseed = math.randomseed
-- `PI` is an ENGINE global in 1.12, not a FrameXML one: the reference reads it bare (UIParent.lua's
-- `elapsedTime * 2 * PI * ROTATIONS_PER_SECOND`, TabardFrame.lua l.61-68) and no shipped FrameXML
-- file ever assigns it (grepped over the whole 1.12 extraction) — so the client supplies it, here,
-- beside the rest of the bare math family.
PI = math.pi
-- The 1.12 bare trig globals are DEGREE-based (the reference CombatText's fountain scroll calls
-- cos/sin with degree arguments); rad/deg ride along as the same family.
function sin(d) return math.sin(math.rad(d)) end
function cos(d) return math.cos(math.rad(d)) end
rad = math.rad
deg = math.deg
-- The rest of the bare math family, each verified present in the real 1.12 client's global
-- table (decision 1189's captured `_G`; 1187 had reached for Blizzard's Era enumeration).
-- `tan` and the inverses follow sin/cos into DEGREES — same family, same convention; that is
-- consistent-with the verified sin/cos finding rather than separately byte-verified, and it is
-- the convention every addon rotation helper assumes (`atan2` returning degrees is why
-- `atan2(dy, dx)` feeds a texture rotation directly).
function tan(d) return math.tan(math.rad(d)) end
function asin(x) return math.deg(math.asin(x)) end
function acos(x) return math.deg(math.acos(x)) end
function atan(x) return math.deg(math.atan(x)) end
function atan2(y, x) return math.deg(math.atan2(y, x)) end
exp = math.exp
log = math.log
log10 = math.log10
frexp = math.frexp
ldexp = math.ldexp

-- ── the debug* family: six verified STUBS and two real ones ────────────────────────────────────
--
-- wow-re carved all eight (`system/ui/scratch/lua-dialect.md` §3a, and the 2026-08-11 batch):
-- `debuginfo`, `debugload`, `debugprint`, `debugdump`, `debugbreak` and `debugtimestamp` are
-- **byte-identical `xor eax,eax; ret` stubs** — three bytes, no `call`, no memory write. They
-- cannot print, log, write or set a global, and they return ZERO Lua values. So these are not
-- no-ops we invented under a real name (the "absent capability" class 1203 named); they are the
-- reference's own no-ops, transcribed.
--
-- That is what lets `BasicControls.xml` stop guarding its `debuginfo()` call and transcribe
-- `_ERRORMESSAGE` verbatim, which it now does.
--
-- `debugprofilestart`/`debugprofilestop` are the two that are REAL (RDTSC via `0x4293d0`): start
-- latches, stop answers the elapsed milliseconds since it. Modelled on `GetTime`'s own clock —
-- the same monotonic session seconds the tick advances — because benilla has no cycle counter and
-- an addon uses these to time its own work, which milliseconds answer honestly.
do
    local function noop() end
    debuginfo = noop
    debugload = noop
    debugprint = noop
    debugdump = noop
    debugbreak = noop
    debugtimestamp = noop

    local profileStart = 0
    function debugprofilestart() profileStart = GetTime() end
    function debugprofilestop() return (GetTime() - profileStart) * 1000 end
end

-- ── the error handler (decision 1195) ──────────────────────────────────────────────────────────
-- `seterrorhandler(f)` / `geterrorhandler()` are ENGINE globals in 1.12 (the captured `_G` says
-- so), and `_ERRORMESSAGE` — the default handler they start out holding — is FrameXML's. That
-- split is why the pair lives here and the default is a plain function rather than a Rust binding:
-- our own transcribed UI can replace it exactly as the reference's `UIErrorsFrame` does.
--
-- The idiom this exists for is `geterrorhandler()(msg)` — an addon's pcall wrapper reporting a
-- caught error the way the client would. Without the pair that line is `attempt to call a nil
-- value` INSIDE an error path, which turns a recoverable addon fault into a dead addon.
do
    local handler = function(msg) __benilla_script_error(msg) end
    function seterrorhandler(f) handler = f end
    function geterrorhandler() return handler end
end

-- ── _G accessors (RF-0023 getglobal/setglobal: _G[name] get/set) ───────────────────────────────
function getglobal(name) return _G[name] end
function setglobal(name, value) _G[name] = value end

-- ── GetLocale: benilla ships/reads enUS data only (the 5875 MPQs + Spell.dbc enUS column) ──────
function GetLocale() return "enUS" end

-- ── GetTime: the FrameXML session clock (seconds, monotonic, arbitrary epoch — like the real
-- client's uptime-based GetTime). `__benilla_now` is advanced by `UiScript::tick` in the same
-- call that fires OnUpdate, so `GetTime()` deltas and accumulated `elapsed` agree. Reference
-- FrameXML (CastingBarFrame & co.) anchors cast windows on it.
__benilla_now = 0.0
function GetTime() return __benilla_now end

-- ── The zone-text family (decisions 0203 phase 1 + 0287, byte-pinned by its fold-back; wow-re
-- ui zonetext-pvpinfo.md). The app pushes the host globals on an area change (the same shape as
-- GetTime) and fires MINIMAP_ZONE_CHANGED / the ZONE_CHANGED family; these getters just read the
-- cached slots, like the real bindings (0x48a0a0/c0/e0/100 read BSS caches). GetZoneText = the
-- zone name, replaced by the WMO interior's name indoors; GetRealZoneText = the WMO-immune zone
-- name; GetSubZoneText = the leaf subzone, "" when the leaf IS the zone (and indoors);
-- GetMinimapZoneText = subzone-else-zone. GetZonePVPInfo returns (pvpType or nil, factionName or
-- nil, isArena) — pvpType is friendly/hostile/contested, never "arena".
__benilla_zone_text = ""
function GetMinimapZoneText() return __benilla_zone_text end
__benilla_zone_name = ""
__benilla_real_zone_name = ""
__benilla_subzone_name = ""
__benilla_pvp_type = ""
__benilla_pvp_faction = ""
__benilla_pvp_arena = false
function GetZoneText() return __benilla_zone_name end
function GetRealZoneText() return __benilla_real_zone_name end
function GetSubZoneText() return __benilla_subzone_name end
function GetZonePVPInfo()
    local t, f = __benilla_pvp_type, __benilla_pvp_faction
    if t == "" then t = nil end
    if f == "" then f = nil end
    return t, f, __benilla_pvp_arena
end

-- ── GetGameTime: the server's in-game clock (hour, minute) — the reference reads the
-- SMSG_LOGIN_SETTIMESPEED-seeded clock the client advances by its timescale. The app pushes the
-- host globals when the game minute ticks (`crate::minimap::feed_game_time` — same shape as the
-- zone-text family); minute resolution is the API's own (the binding returns no seconds).
-- 0:00 until the first time packet lands.
__benilla_game_hour = 0
__benilla_game_minute = 0
function GetGameTime() return __benilla_game_hour, __benilla_game_minute end

-- ── wipe / tostringall ─────────────────────────────────────────────────────────────────────────
function wipe(t)
    for k in pairs(t) do t[k] = nil end
    return t
end

function tostringall(...)
    local n = select('#', ...)
    local t = {}
    for i = 1, n do t[i] = tostring((select(i, ...))) end
    return unpack(t, 1, n)
end

-- ── the strsplit / strjoin / strtrim / strconcat family ────────────────────────────────────────
-- strsplit(delimiters, str [, limit]): each char of `delimiters` is a single-char delimiter;
-- returns the pieces as multiple values (empty fields preserved), matching WoW's behavior.
function strsplit(delim, str, limit)
    local set = "[" .. delim:gsub("(.)", "%%%1") .. "]"
    local result = {}
    local start = 1
    while true do
        if limit and #result >= limit - 1 then
            result[#result + 1] = str:sub(start)
            break
        end
        local s, e = str:find(set, start)
        if not s then
            result[#result + 1] = str:sub(start)
            break
        end
        result[#result + 1] = str:sub(start, s - 1)
        start = e + 1
    end
    return unpack(result)
end

-- strjoin(sep, ...): join the varargs with `sep`.
function strjoin(sep, ...)
    return table.concat({ ... }, sep)
end

-- strconcat(...): concatenate all args (WoW's join with no separator).
function strconcat(...)
    return table.concat({ ... })
end

-- strtrim(s [, chars]): trim leading/trailing chars (default whitespace).
function strtrim(s, chars)
    chars = chars or " \t\r\n"
    local set = chars:gsub("(.)", "%%%1")
    return (s:gsub("^[" .. set .. "]*(.-)[" .. set .. "]*$", "%1"))
end

-- ── Blizzard's positional string.format (%N$) ──────────────────────────────────────────────────
do
    local _format = string.format
    local CONV = "diouxXeEfgGqscp"  -- Lua 5.1 conversion letters

    local function reformat(fmt, ...)
        if type(fmt) ~= "string" then
            return _format(fmt, ...)
        end
        -- fast path: no "%<digit>" at all ⇒ definitely no positional spec.
        if not fmt:find("%%%d") then
            return _format(fmt, ...)
        end

        local args = { ... }
        local pieces = {}      -- rebuilt (sequential) format fragments
        local order = {}       -- for each real conversion: the source arg index, or false=sequential
        local seen_pos, seen_seq = false, false
        local i, len = 1, #fmt

        while i <= len do
            local c = fmt:sub(i, i)
            if c ~= "%" then
                pieces[#pieces + 1] = c
                i = i + 1
            elseif fmt:sub(i + 1, i + 1) == "%" then
                pieces[#pieces + 1] = "%%"
                i = i + 2
            else
                local j = i + 1
                local ds, de = fmt:find("^%d+%$", j)  -- optional N$
                if ds then
                    seen_pos = true
                    order[#order + 1] = tonumber(fmt:sub(ds, de - 1))
                    j = de + 1
                else
                    seen_seq = true
                    order[#order + 1] = false
                end
                -- copy flags/width/precision + the conversion letter
                local ce = fmt:find("[" .. CONV .. "]", j)
                if not ce then
                    error("invalid conversion in format string", 2)
                end
                pieces[#pieces + 1] = "%" .. fmt:sub(j, ce)
                i = ce + 1
            end
        end

        if seen_pos and seen_seq then
            error("cannot mix positional and sequential arguments in format string", 2)
        end

        local out, seq = {}, 0
        for k = 1, #order do
            local idx = order[k]
            if idx == false then
                seq = seq + 1
                out[k] = args[seq]
            else
                out[k] = args[idx]
            end
        end
        return _format(table.concat(pieces), unpack(out, 1, #order))
    end

    string.format = reformat
    format = reformat
end
"#;

/// `time()` and `date([format [, when]])` — engine globals in the 1.12 client's own `_G`, slots 34
/// and 33 of its base registry (`0x7035a0` / `0x7033a0`, wow-re `lua-dialect.md`).
///
/// They are Lua 5.0's `os.time`/`os.date` hoisted to globals, and we lacked both because the
/// sandbox strips `os` wholesale. `time` was the top name in the session-start
/// `attempt to call global` row (6 addons); `date` is a handful more. Every corpus `time()` site is
/// the same shape — an epoch stamp persisted into SavedVariables and compared across sessions
/// (`FTC_Save[k].LastCheck = time()`) — so it has to be real wall-clock seconds, not a
/// session-relative clock like `GetTime`.
///
/// **UTC, and that is a stated divergence.** `os.date` uses LOCAL time and this uses UTC, because
/// resolving a local offset needs a timezone database and this tree has no date dependency at all
/// (deliberately — the format crates here are all in-repo). The corpus's uses are a debug
/// timestamp, a "last scanned" string, and `%M:%S` over a DURATION; only the first two shift, and
/// they shift by a constant. Fixing it means a tz source, not a different algorithm.
fn install_time(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    g.set("time", lua.create_function(|_, ()| Ok(unix_seconds()))?)?;
    g.set(
        "date",
        lua.create_function(|lua, (fmt, when): (Option<String>, Option<i64>)| {
            // Bare `date()` is `%c`, as in Lua — `Recap.lua:2690` calls it with no arguments.
            let fmt = fmt.unwrap_or_else(|| "%c".to_string());
            let secs = when.unwrap_or_else(unix_seconds);
            // A leading `!` selects UTC. Every timestamp this engine holds is already UTC — there
            // is no local-time conversion anywhere here — so the flag only has to be CONSUMED, not
            // acted on. Left in the format string it would print as a literal `!`.
            let body = fmt.strip_prefix('!').unwrap_or(&fmt);
            // `date("*t")` returns a TABLE, not a string, and leaving it unimplemented was not a
            // missing feature — it was a HANG. `Accountant_WeekStart` walks a day at a time until
            // the weekday matches its stored `weekstart`:
            //
            //     dt = date("*t", ct); thisDay = dt["wday"]
            //     while thisDay ~= …weekstart do ct = ct - 86400; … end
            //
            // Against a string return, `dt["wday"]` is nil, `nil ~= 3` forever, and the addon spins
            // the client — which is exactly what it did to the survey the moment a separate fix let
            // Accountant reach this function at all.
            if body == "*t" {
                let (year, month, day, hour, min, sec, wday, yday) = civil_from_epoch(secs);
                let t = lua.create_table()?;
                t.set("year", year)?;
                t.set("month", month)?;
                t.set("day", day)?;
                t.set("hour", hour)?;
                t.set("min", min)?;
                t.set("sec", sec)?;
                // Lua counts weekdays 1..7 from SUNDAY; ours is the 0-based index into `DAYS`.
                t.set("wday", wday + 1)?;
                // `yday` is already 1-based out of the conversion, which is Lua's convention too.
                t.set("yday", yday)?;
                // No timezone and no DST rules here, and `false` is the honest answer rather than
                // the nil an absent field would give: this clock never observes daylight saving.
                t.set("isdst", false)?;
                return Ok(mlua::Value::Table(t));
            }
            Ok(mlua::Value::String(
                lua.create_string(format_epoch(secs, body))?,
            ))
        })?,
    )?;
    Ok(())
}

/// Wall-clock seconds since the Unix epoch. Before the epoch is not a state a game client is in;
/// a clock that somehow reports it clamps to 0 rather than raising inside an addon's file scope.
fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Broken-down UTC time from an epoch second: (year, month 1-12, day 1-31, hour, min, sec,
/// weekday 0=Sunday, yearday 1-366).
///
/// The civil-from-days conversion is Howard Hinnant's, shifted to a March-based year so the leap
/// day lands at the end and no month-length table is needed. Chosen over a hand-rolled loop because
/// it is a known-correct closed form with no accumulation error, which matters for the "persisted
/// last session, compared this session" use every corpus caller has.
fn civil_from_epoch(secs: i64) -> (i64, u32, u32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    // 1970-01-01 was a Thursday (4).
    let weekday = (days + 4).rem_euclid(7) as u32;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };

    // Day of the year, from the same conversion applied to Jan 1.
    let jan1 = days_from_civil(year, 1, 1);
    let yearday = (days - jan1 + 1) as u32;
    (year, month, day, hour, min, sec, weekday, yearday)
}

/// Days since the epoch for a civil date — the inverse of the above, used only for `%j`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The `strftime` subset the corpus actually uses, plus the obvious neighbours.
///
/// Bounded on purpose: the specifiers here are the ones read off real call sites —
/// `%H:%M:%S` (AceDebug), `%A, %B %d, %Y - %H:%M` (FuBar_PotHerbFu), `%M:%S` (FuBar_AnkhTimerFu),
/// `%c` (bare `date()`). An unknown specifier is emitted VERBATIM rather than swallowed, so a
/// format this does not know shows up in the output instead of silently vanishing.
fn format_epoch(secs: i64, fmt: &str) -> String {
    let (year, month, day, hour, min, sec, wday, yday) = civil_from_epoch(secs);
    let mut out = String::with_capacity(fmt.len() + 16);
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(spec) = chars.next() else {
            out.push('%');
            break;
        };
        let hour12 = match hour % 12 {
            0 => 12,
            h => h,
        };
        match spec {
            'a' => out.push_str(&DAYS[wday as usize][..3]),
            'A' => out.push_str(DAYS[wday as usize]),
            'b' | 'h' => out.push_str(&MONTHS[(month - 1) as usize][..3]),
            'B' => out.push_str(MONTHS[(month - 1) as usize]),
            'c' => out.push_str(&format!(
                "{} {} {:2} {:02}:{:02}:{:02} {}",
                &DAYS[wday as usize][..3],
                &MONTHS[(month - 1) as usize][..3],
                day,
                hour,
                min,
                sec,
                year
            )),
            'd' => out.push_str(&format!("{day:02}")),
            'H' => out.push_str(&format!("{hour:02}")),
            'I' => out.push_str(&format!("{hour12:02}")),
            'j' => out.push_str(&format!("{yday:03}")),
            'm' => out.push_str(&format!("{month:02}")),
            'M' => out.push_str(&format!("{min:02}")),
            'p' => out.push_str(if hour < 12 { "AM" } else { "PM" }),
            'S' => out.push_str(&format!("{sec:02}")),
            'w' => out.push_str(&format!("{wday}")),
            'x' => out.push_str(&format!("{month:02}/{day:02}/{:02}", year.rem_euclid(100))),
            'X' => out.push_str(&format!("{hour:02}:{min:02}:{sec:02}")),
            'y' => out.push_str(&format!("{:02}", year.rem_euclid(100))),
            'Y' => out.push_str(&format!("{year}")),
            '%' => out.push('%'),
            // Unknown: verbatim, so it is visible rather than swallowed.
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}
#[cfg(test)]
mod date_table_tests {
    use crate::script::UiScript;

    /// **`date("*t")` returns a TABLE, and the reason this test exists is that its absence HUNG the
    /// client.** `Accountant_WeekStart` (`Accountant.lua:364-375`) walks backwards a day at a time
    /// until the weekday matches its stored `weekstart`; with a string return `dt["wday"]` is nil,
    /// `nil ~= 3` never becomes false, and the loop never ends.
    #[test]
    fn date_star_t_answers_a_table_that_walks_with_its_argument() {
        let s = UiScript::new().unwrap();
        // 2026-08-12 00:00:00 UTC is a Wednesday. Lua counts wday from SUNDAY = 1, so Wednesday
        // is 4 — the off-by-one a 0-based weekday index would get wrong.
        let t: i64 = 1_786_492_800;
        assert_eq!(
            s.eval::<(i64, i64, i64, i64, bool)>(&format!(
                "local d = date(\"*t\", {t}) return d.year, d.month, d.day, d.wday, d.isdst"
            ))
            .unwrap(),
            (2026, 8, 12, 4, false)
        );
        // hour/min/sec and yday come through too — an addon reading any of them must not get nil.
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(&format!(
                "local d = date(\"*t\", {t} + 3661) return d.hour, d.min, d.sec, d.yday"
            ))
            .unwrap(),
            (1, 1, 1, 224)
        );

        // **The loop Accountant actually runs.** Stepping back a day must move `wday`, and the walk
        // must terminate — this is the hang, expressed as the addon expresses it.
        assert_eq!(
            s.eval::<i64>(&format!(
                "local ct = {t} local n = 0 \
                 while date(\"*t\", ct).wday ~= 3 and n < 100 do ct = ct - 86400 n = n + 1 end \
                 return n"
            ))
            .unwrap(),
            1,
            "Wednesday(4) back to Tuesday(3) is exactly one step; a constant wday would spin"
        );

        // A `!` prefix selects UTC, which every timestamp here already is — it must be CONSUMED
        // rather than printed, and it works on the table form as well as the string form.
        assert_eq!(
            s.eval::<i64>(&format!("return date(\"!*t\", {t}).wday"))
                .unwrap(),
            4
        );
        assert!(
            !s.eval::<String>(&format!("return date(\"!%A\", {t})"))
                .unwrap()
                .contains('!'),
            "the UTC flag is consumed, not printed"
        );
        // …and an ordinary format still answers a string.
        assert_eq!(
            s.eval::<String>(&format!("return date(\"%A\", {t})"))
                .unwrap(),
            "Wednesday"
        );
    }
}
