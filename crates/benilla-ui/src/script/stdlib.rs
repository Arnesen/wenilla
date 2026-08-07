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

    // A `debugstack` stub: real addons call it in error paths; returning "" keeps them running.
    g.set(
        "debugstack",
        lua.create_function(|_, _: Variadic<Value>| Ok(String::new()))?,
    )?;

    // Text-only `loadstring`: compile a *source* string (never bytecode). Returns `function` on
    // success or `(nil, errmsg)` on failure — the stock `loadstring` contract. `set_mode(Text)`
    // makes mlua reject a binary chunk (the `\27Lua` signature), the "loadstring of bytecode
    // rejected" guarantee.
    let loadstring = lua.create_function(
        |lua, (src, chunkname): (mlua::String, Option<mlua::String>)| {
            let bytes = src.as_bytes().to_vec();
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
    lua.load(WOW_STDLIB)
        .set_name("=[benilla wow stdlib]")
        .set_mode(mlua::ChunkMode::Text)
        .exec()?;
    Ok(())
}

/// The stdlib layer, in Lua. One place, so each alias is the one-liner the task calls for and the
/// `format`/`strsplit` logic is readable. Runs once at construction.
const WOW_STDLIB: &str = r#"
-- ── the nine probe-found bare-global aliases (+ strlower/strrep) ────────────────────────────────
strlen  = string.len
strsub  = string.sub
strupper = string.upper
strlower = string.lower
strrep  = string.rep
strfind = string.find
gsub    = string.gsub
tinsert = table.insert
tremove = table.remove
getn    = table.getn or function(t) return #t end

-- ── the bare math aliases (the same 1.12 global family; the reference FrameXML uses them
-- unqualified — WorldMapFrame's overlay pool calls ceil/mod, CooldownFrame floor, …). `mod` is
-- fmod under a 5.0-era name (stock 5.1 dropped math.mod).
ceil  = math.ceil
floor = math.floor
abs   = math.abs
max   = math.max
min   = math.min
sqrt  = math.sqrt
mod   = math.fmod
random = math.random
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
