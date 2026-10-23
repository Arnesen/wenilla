// automate.js — `wenilla.auto`, the automation library on top of the bridge (bridge.js).
//
// Everything here is built from the bridge's public surface: the frame snapshot, the event
// stream, the world verbs (`target`, `interact`, `moveTo`, `stop`), held/fired binding commands,
// and `lua()` into the UI VM — 1.12's own API (BuyMerchantItem, LootSlot, AcceptQuest, …). So a
// script can do exactly what a player at the keyboard can, through the same code paths, and
// nothing more. The docs page (`/docs/bridge` on a realm) and web/README.md describe the API;
// web/examples/grind.js is a complete leveling loop built on it.
//
// Conventions: positions are WoW `[x, y, z]` yards; a "unit" is a `state.units[]` row (or its
// guid); helpers that wait return promises and reject with an Error naming what failed.

/** Dialog/window events an interaction can open, by what they mean. */
const OPEN_EVENTS = [
  'MERCHANT_SHOW', 'GOSSIP_SHOW', 'QUEST_GREETING', 'QUEST_DETAIL', 'QUEST_PROGRESS',
  'QUEST_COMPLETE', 'TRAINER_SHOW', 'LOOT_OPENED', 'TAXIMAP_OPENED', 'BANKFRAME_OPENED',
  'MAIL_SHOW', 'AUCTION_HOUSE_SHOW', 'PETITION_VENDOR_SHOW', 'CONFIRM_BINDER', 'PET_STABLE_SHOW',
];
/** Reaches, yards: the client's NPC talk reach is 5.55, a GameObject's ~5, melee ~5 with size. */
const REACH = { talk: 4.5, use: 4, melee: 3.5 };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const dist2 = (a, b) => Math.hypot(a[0] - b[0], a[1] - b[1]);
const luaStr = (s) => JSON.stringify(String(s)); // a JSON string is a valid Lua string literal here
/**
 * A Lua sequence as a JS array. `lua()` converts a table through Lua's length operator, which
 * stops at the first nil — so every row built here substitutes a value for a nil (`x or 0`)
 * before it goes into a table, and every row read goes through this, which also accepts a table
 * that came back keyed ('1', '3') and pads a row to its arity `n`.
 */
function seq(t, n = 0) {
  if (Array.isArray(t)) return n > t.length ? [...t, ...Array(n - t.length).fill(null)] : t;
  if (!t || typeof t !== 'object') return Array(n).fill(null);
  const len = Math.max(n, ...Object.keys(t).map(Number).filter(Number.isInteger));
  return Array.from({ length: len }, (_, i) => t[i + 1] ?? null);
}

/** A link's parts: `|cffRRGGBB|Hitem:ID:…|h[Name]|h|r`. */
function parseLink(link) {
  if (typeof link !== 'string') return null;
  const color = /\|cff([0-9a-f]{6})/i.exec(link)?.[1]?.toLowerCase();
  const QUALITY = { '9d9d9d': 0, ffffff: 1, '1eff00': 2, '0070dd': 3, a335ee: 4, ff8000: 5, e6cc80: 6 };
  return {
    link,
    id: Number(/item:(\d+)/.exec(link)?.[1]) || null,
    name: /\|h\[(.*?)\]\|h/.exec(link)?.[1] ?? null,
    quality: color in QUALITY ? QUALITY[color] : null,
  };
}

export function install(w) {
  const auto = {};
  w.auto = auto;

  // Everything below listens for these; subscribing once up front means no event is missed
  // between an action and the `once()` that waits for its answer.
  w.subscribe(
    ...OPEN_EVENTS,
    'PLAYER_DEAD', 'PLAYER_ALIVE', 'PLAYER_UNGHOST', 'PLAYER_ENTER_COMBAT', 'PLAYER_LEAVE_COMBAT',
    'PLAYER_REGEN_DISABLED', 'PLAYER_REGEN_ENABLED', 'LOOT_CLOSED', 'LOOT_BIND_CONFIRM',
    'MERCHANT_CLOSED', 'GOSSIP_CLOSED', 'QUEST_FINISHED', 'TRAINER_CLOSED', 'UI_ERROR_MESSAGE',
    'SPELLCAST_START', 'SPELLCAST_STOP', 'SPELLCAST_FAILED', 'SPELLCAST_INTERRUPTED',
  );

  // ── session memory ──
  /** True while auto-attack swings (PLAYER_ENTER_COMBAT / _LEAVE_COMBAT are 1.12's swing edges). */
  auto.attacking = false;
  /** Where we died last (for the corpse run), and NPCs with a service we have seen, by guid. */
  auto.deathPos = null;
  auto.npcs = new Map();
  w.on('PLAYER_ENTER_COMBAT', () => (auto.attacking = true));
  w.on('PLAYER_LEAVE_COMBAT', () => (auto.attacking = false));
  w.on('PLAYER_DEAD', () => {
    auto.attacking = false;
    auto.deathPos = w.state?.self?.pos ?? auto.deathPos;
  });
  w.on('frame', (s) => {
    for (const u of s.units ?? []) {
      if (u.services?.length && u.kind === 'unit') {
        auto.npcs.set(u.guid, { guid: u.guid, name: u.name, entry: u.entry, services: u.services, pos: u.pos, map: s.map?.id });
      }
    }
  });

  // ── reading the world ──

  /** The player's row (`state.self`), or null out of world. */
  auto.me = () => w.state?.self ?? null;

  /**
   * Does a unit row match a filter? A function, or an object of conditions, all of which must
   * hold: `kind` ('unit'|'player'|'go'|'corpse'), `name` (exact, case-insensitive, or a RegExp),
   * `entry`, `hostile`, `friendly`, `alive`, `dead`, `lootable`, `skinnable`, `tapped`,
   * `inCombat`, `isPlayer`, `service` ('vendor', 'repair', 'trainer', 'questgiver', …),
   * `questStatus` ('available'|'reward'|'incomplete'), `goType` ('chest', 'mailbox', …),
   * `minLevel`, `maxLevel`, `maxDist`, `targetingMe`, `untargeted` (no target, or us).
   */
  auto.matches = (u, f = {}) => {
    if (!u) return false;
    if (typeof f === 'function') return !!f(u);
    const me = auto.me();
    if (f.kind && u.kind !== f.kind) return false;
    if (f.name !== undefined) {
      if (f.name instanceof RegExp ? !f.name.test(u.name ?? '') : (u.name ?? '').toLowerCase() !== String(f.name).toLowerCase()) return false;
    }
    if (f.entry !== undefined && u.entry !== f.entry) return false;
    for (const k of ['hostile', 'friendly', 'lootable', 'skinnable', 'tapped', 'inCombat', 'isPlayer', 'dead']) {
      if (f[k] !== undefined && !!u[k] !== !!f[k]) return false;
    }
    if (f.alive !== undefined && !u.dead === !f.alive) return false;
    if (f.service && !(u.services ?? []).includes(f.service)) return false;
    if (f.questStatus && u.questStatus !== f.questStatus) return false;
    if (f.goType && u.goTypeName !== f.goType) return false;
    if (f.minLevel !== undefined && (u.level ?? 0) < f.minLevel) return false;
    if (f.maxLevel !== undefined && (u.level ?? 0) > f.maxLevel) return false;
    if (f.maxDist !== undefined && u.dist > f.maxDist) return false;
    if (f.targetingMe && u.targetGuid !== me?.guid) return false;
    if (f.untargeted && u.targetGuid && u.targetGuid !== me?.guid) return false;
    return true;
  };
  /** Units in the snapshot matching `filter`, nearest first. */
  auto.units = (filter) => (w.state?.units ?? []).filter((u) => auto.matches(u, filter));
  /** The nearest unit matching `filter`, or null. */
  auto.nearest = (filter) => auto.units(filter)[0] ?? null;
  /** A unit's current row by guid (the target included even out of `radius`), or null. */
  auto.find = (unit) => {
    const guid = typeof unit === 'string' ? unit : unit?.guid;
    if (!guid) return null;
    const s = w.state;
    return s?.target?.guid === guid ? s.target : (s?.units ?? []).find((u) => u.guid === guid) ?? null;
  };
  /** Enemies attacking us (their target is us). */
  auto.attackers = () => auto.units({ alive: true, targetingMe: true, inCombat: true });

  /** Level, xp, money (copper), health and power, from the VM. */
  auto.stats = async () => {
    const [level, xp, xpMax, money, hp, hpMax, mana, manaMax, combat] = await w.lua(
      "return UnitLevel('player'), UnitXP('player'), UnitXPMax('player'), GetMoney(), UnitHealth('player'), UnitHealthMax('player'), UnitMana('player'), UnitManaMax('player'), UnitAffectingCombat('player')",
    );
    return { level, xp, xpMax, money, hp, hpMax, mana, manaMax, inCombat: !!combat };
  };
  /** Health and power as fractions 0..1, from the snapshot (no VM round trip). */
  auto.hp = (u = auto.me()) => (u?.maxHealth ? u.health / u.maxHealth : 0);
  auto.power = (u = auto.me()) => (u?.maxPower ? u.power / u.maxPower : 0);

  // ── movement ──

  /** Turn to face a unit or a position (a short turn, applied next frame). */
  auto.face = (to) => {
    const pos = Array.isArray(to) ? to : auto.find(to)?.pos ?? to?.pos;
    if (!pos) return 0;
    const b = w.bearingTo({ pos });
    if (Number.isFinite(b) && Math.abs(b) > 0.05) w.look(b);
    return b;
  };

  /** A short wiggle out of a snag: jump and strafe for half a second. */
  auto.unstick = async () => {
    const side = Math.random() < 0.5 ? 'STRAFELEFT' : 'STRAFERIGHT';
    w.hold(side);
    w.fire('JUMP');
    await sleep(500);
    w.release(side);
  };

  /**
   * Walk to `pos` and resolve on arrival (within `tolerance` yards, ignoring height). Straight
   * lines only — the client has no pathfinding — so a wall stops the walk; it then tries
   * `retries` unsticks before rejecting with "stuck". Use `followRoute` for anything but open
   * ground.
   */
  auto.moveTo = async (pos, { tolerance = 1.5, timeout, retries = 3 } = {}) => {
    const me = auto.me();
    if (!me) throw new Error('moveTo: not in world');
    if (dist2(me.pos, pos) <= tolerance) return true;
    const limit = timeout ?? 8000 + (dist2(me.pos, pos) / 5) * 1000;
    const deadline = Date.now() + limit;
    for (let attempt = 0; attempt <= retries; attempt++) {
      const r = await w.moveTo(pos);
      if (!r.ok) throw new Error(`moveTo: ${r.reason}`);
      const started = Date.now();
      const ended = await w.waitFor(
        (s) => dist2(s.self.pos, pos) <= tolerance || (Date.now() - started > 400 && !s.self.autoMove),
        Math.max(0, deadline - Date.now()),
      );
      const here = auto.me()?.pos;
      if (here && dist2(here, pos) <= tolerance) {
        await w.stop();
        return true;
      }
      if (!ended) break; // the deadline, not a stop
      await auto.unstick();
    }
    await w.stop();
    throw new Error(`moveTo: stuck ${auto.me() ? dist2(auto.me().pos, pos).toFixed(1) : '?'} yd short`);
  };

  /**
   * Get within `range` yards of a unit, following it if it moves. Resolves with its row, or
   * rejects if it vanishes or cannot be reached.
   */
  auto.approach = async (unit, range = REACH.melee, { timeout = 30000 } = {}) => {
    const deadline = Date.now() + timeout;
    let stalls = 0;
    for (;;) {
      const u = auto.find(unit);
      if (!u) throw new Error('approach: the unit is gone');
      if (u.dist <= range) {
        if (auto.me()?.autoMove) await w.stop();
        return u;
      }
      if (Date.now() > deadline) throw new Error('approach: timed out');
      const goal = u.pos;
      const r = await w.moveTo(goal);
      if (!r.ok) throw new Error(`approach: ${r.reason}`);
      const started = Date.now();
      await w.waitFor((s) => {
        const t = auto.find(unit);
        return !t || t.dist <= range || dist2(t.pos, goal) > 2 || (Date.now() - started > 400 && !s.self.autoMove);
      }, Math.min(5000, Math.max(0, deadline - Date.now())));
      const t = auto.find(unit);
      if (t && t.dist > range && !auto.me()?.autoMove && dist2(t.pos, goal) <= 2) {
        // The straight walk stalled: something is in the way. Plan around it.
        if (++stalls > 2) throw new Error('approach: stuck');
        await auto.goto(t.pos, { tolerance: range, replans: 2 }).catch(() => auto.unstick());
      }
    }
  };

  /**
   * Walk to `pos` the way a player would: plan a path around walls and over floors (`w.path`),
   * walk its waypoints, and replan from wherever a leg gets blocked (a mob in the way, a gap the
   * grid missed) up to `replans` times. Resolves true on arrival; rejects when no way is found.
   * Searches only the collision loaded around the player (a few hundred yards); for longer trips
   * walk a `recordRoute` route, or `goto` successive points along it.
   */
  auto.goto = async (pos, { tolerance = 1.5, replans = 4, budget } = {}) => {
    for (let attempt = 0; attempt <= replans; attempt++) {
      const me = auto.me();
      if (!me) throw new Error('goto: not in world');
      if (dist2(me.pos, pos) <= tolerance) return true;
      const r = await w.path(pos, { budget });
      if (!r.ok) throw new Error(`goto: ${r.reason}`);
      const legs = seq(r.points).slice(1);
      try {
        for (let k = 0; k < legs.length; k++) {
          const last = k === legs.length - 1;
          await auto.moveTo(legs[k], { tolerance: last ? tolerance : 1.2, retries: 1 });
        }
      } catch {
        continue; // blocked mid-route: plan again from here
      }
      if (dist2(auto.me().pos, pos) <= tolerance) return true;
      if (!r.partial) return true; // the goal's own cell, within the path's arrival radius
    }
    throw new Error('goto: no way found');
  };

  /** Walk a list of waypoints in order (optionally `loop`ing until `signal.stop` is set). */
  auto.followRoute = async (points, { tolerance = 2, loop = false, signal = {} } = {}) => {
    do {
      for (const p of points) {
        if (signal.stop) return;
        await auto.moveTo(p, { tolerance });
      }
    } while (loop && !signal.stop);
  };

  /**
   * Record a route while you (or a script) move: a waypoint every `spacing` yards. Returns
   * `{ points, stop() }`; `stop()` returns the points. Save them (JSON) and replay with
   * `followRoute` — the way around the lack of pathfinding.
   */
  auto.recordRoute = ({ spacing = 8 } = {}) => {
    const points = [];
    const off = w.on('frame', (s) => {
      const p = s.self?.pos;
      if (p && (!points.length || dist2(points[points.length - 1], p) >= spacing)) points.push([...p]);
    });
    return {
      points,
      stop() {
        off();
        const p = auto.me()?.pos;
        if (p) points.push([...p]);
        return points;
      },
    };
  };

  // ── targeting, interaction ──

  /** Select a unit. Resolves true when it is the target. */
  auto.target = async (unit) => {
    const guid = typeof unit === 'string' ? unit : unit?.guid;
    if (w.state?.target?.guid === guid) return true;
    const r = await w.target(guid);
    return r.ok && (await w.waitFor((s) => s.target?.guid === guid, 1500));
  };
  auto.clearTarget = () => w.lua('ClearTarget()');

  /**
   * Walk into reach of an object, face it and interact (a right-click). Resolves with the name of
   * the window event it opened (`'MERCHANT_SHOW'`, `'GOSSIP_SHOW'`, `'LOOT_OPENED'`, …), or null
   * if nothing opened in `timeout` (an attack, a gather that failed, a refused talk).
   */
  auto.interact = async (unit, { range, timeout = 3000, autoLoot = false } = {}) => {
    const u = auto.find(unit);
    if (!u) throw new Error('interact: no such object');
    const reach = range ?? (u.kind === 'go' ? REACH.use : REACH.talk);
    await auto.approach(u, reach);
    auto.face(u);
    await sleep(100);
    const opened = Promise.race(OPEN_EVENTS.map((e) => w.once(e, timeout).then((p) => (p ? e : null))));
    const r = await w.interact(u, { autoLoot });
    if (!r.ok) throw new Error(`interact: ${r.reason}`);
    return opened;
  };

  // ── combat ──

  /** Cast a spell by name (`'Fireball'`, `'Fireball(Rank 3)'`); `onSelf` for a friendly spell on us. */
  auto.cast = (spell, onSelf = false) => w.lua(`CastSpellByName(${luaStr(spell)}${onSelf ? ', 1' : ''})`);

  let spellIndex = null;
  /** The spellbook as `{name: [index of each rank]}`, read once (call `refreshSpells` after training). */
  auto.refreshSpells = async () => {
    const [list] = await w.lua(
      "local t = {} local i = 1 while true do local n, r = GetSpellName(i, 'spell') if not n then break end table.insert(t, {n, r or ''}) i = i + 1 end return t",
    );
    spellIndex = {};
    seq(list).forEach((row, i) => { const [n] = seq(row); (spellIndex[n] ??= []).push(i + 1); });
    return spellIndex;
  };
  /** Do we know a spell (any rank)? */
  auto.knows = async (spell) => !!(spellIndex ?? (await auto.refreshSpells()))[spell];
  /** Seconds until a spell's cooldown ends (0 = ready; Infinity = not known). */
  auto.cooldown = async (spell) => {
    const idx = (spellIndex ?? (await auto.refreshSpells()))[spell];
    if (!idx) return Infinity;
    const [start, dur] = await w.lua(`return GetSpellCooldown(${idx[idx.length - 1]}, 'spell')`);
    return start > 0 && dur > 0 ? Math.max(0, start + dur - (await w.lua('return GetTime()'))[0]) : 0;
  };
  /** Is a spell known and off cooldown? */
  auto.ready = async (spell) => (await auto.cooldown(spell)) === 0;
  /** Buffs (or debuffs) on a unit token, as texture paths — 1.12's UnitBuff answers icons. */
  auto.auras = async (token = 'player', debuffs = false) => {
    const fn = debuffs ? 'UnitDebuff' : 'UnitBuff';
    const [list] = await w.lua(`local t = {} for i = 1, 32 do local x = ${fn}(${luaStr(token)}, i) if not x then break end table.insert(t, x) end return t`);
    return seq(list);
  };
  auto.hasAura = async (texturePart, token = 'player', debuffs = false) =>
    (await auto.auras(token, debuffs)).some((t) => String(t).toLowerCase().includes(texturePart.toLowerCase()));

  /** Start auto-attack on a unit (interact's attack leg: select, face, swing). */
  auto.attack = async (unit) => {
    await auto.target(unit);
    auto.face(unit);
    if (!auto.attacking) await w.interact(unit);
  };

  /**
   * Fight a unit until it or we die. Walks into `range` (melee by default; a caster passes its
   * spell range), keeps facing it, auto-attacks unless `melee: false`, and runs `rotation(ctx)`
   * every `tick` ms — an async function that casts what it wants, e.g.
   * `async ({ ready, cast, target }) => { if (await ready('Heroic Strike')) await cast('Heroic Strike') }`.
   * Resolves `'won'`, `'died'`, or `'lost'` (the unit vanished or `timeout`).
   */
  auto.fight = async (unit, { range = REACH.melee, melee = true, rotation, tick = 300, timeout = 120000 } = {}) => {
    const guid = typeof unit === 'string' ? unit : unit.guid;
    const deadline = Date.now() + timeout;
    await auto.target(guid);
    const ctx = { cast: auto.cast, ready: auto.ready, cooldown: auto.cooldown, hasAura: auto.hasAura, auto, w };
    while (Date.now() < deadline) {
      const me = auto.me();
      if (!me || me.dead) return 'died';
      const t = auto.find(guid);
      if (!t) return 'lost';
      if (t.dead) {
        if (auto.attacking) w.fire('ATTACKTARGET');
        return 'won';
      }
      if (t.dist > range) {
        try {
          await auto.approach(guid, range - 0.5, { timeout: 15000 });
        } catch {
          return 'lost';
        }
      }
      if (Math.abs(w.bearingTo(t)) > 0.35) auto.face(t);
      if (w.state?.target?.guid !== guid) await auto.target(guid);
      if (melee && !auto.attacking) await w.interact(guid);
      ctx.target = auto.find(guid);
      ctx.me = auto.me();
      if (rotation && !auto.me()?.casting && !auto.me()?.channeling) {
        try {
          await rotation(ctx);
        } catch (e) {
          console.warn('wenilla.auto.fight: rotation threw', e);
        }
      }
      await sleep(tick);
    }
    return 'lost';
  };

  // ── loot ──

  /**
   * Loot a body (walk to it, open it with auto-loot, which takes everything that fits).
   * Resolves with the number of items that were in it, or 0 if it did not open. 1.12 lets no
   * script take a single row (a scripted loot-button click is refused, `LootSlot` only confirms
   * a bind-on-pickup), so what does not fit stays on the body.
   */
  auto.loot = async (unit, { timeout = 3000 } = {}) => {
    const opened = await auto.interact(unit, { range: REACH.talk, autoLoot: true, timeout });
    if (opened !== 'LOOT_OPENED') return 0;
    const [n] = await w.lua('return GetNumLootItems()');
    // The auto-loot sweep closes the window when it empties it; close what it could not.
    if (!(await w.once('LOOT_CLOSED', 2000))) await w.lua('CloseLoot()');
    return n ?? 0;
  };
  /** Gather a GameObject (herb, vein, chest): use it with auto-loot and wait out the cast. */
  auto.gather = (node) => auto.loot(node, { timeout: 8000 });

  // ── bags ──

  /** Every item in the backpack and bags: `{bag, slot, link, id, name, quality, count}`. */
  auto.bags = async () => {
    const [rows] = await w.lua(
      'local t = {} for b = 0, 4 do for s = 1, (GetContainerNumSlots(b) or 0) do local l = GetContainerItemLink(b, s) if l then local _, c = GetContainerItemInfo(b, s) table.insert(t, {b, s, l, c or 1}) end end end return t',
    );
    return seq(rows).map((r) => seq(r, 4)).map(([bag, slot, link, count]) => ({ bag, slot, count, ...parseLink(link) }));
  };
  /** Free bag slots. */
  auto.freeSlots = async () =>
    (await w.lua('local f = 0 for b = 0, 4 do for s = 1, (GetContainerNumSlots(b) or 0) do if not GetContainerItemLink(b, s) then f = f + 1 end end end return f'))[0] ?? 0;
  /** Use (right-click) the first bag item whose name matches: eat, drink, open, equip. */
  auto.useItem = async (name) => {
    const it = (await auto.bags()).find((i) => (name instanceof RegExp ? name.test(i.name ?? '') : i.name === name));
    if (!it) return false;
    await w.lua(`UseContainerItem(${it.bag}, ${it.slot})`);
    return true;
  };

  // ── vendors ──

  /** What each service's window is, and the gossip option type that leads to it. */
  const SERVICES = {
    vendor: { event: 'MERCHANT_SHOW', gossip: 'vendor' },
    repair: { event: 'MERCHANT_SHOW', gossip: 'vendor' },
    trainer: { event: 'TRAINER_SHOW', gossip: 'trainer' },
    flightmaster: { event: 'TAXIMAP_OPENED', gossip: 'taxi' },
    banker: { event: 'BANKFRAME_OPENED', gossip: 'banker' },
    innkeeper: { event: 'CONFIRM_BINDER', gossip: 'binder' },
    battlemaster: { event: 'BATTLEFIELDS_SHOW', gossip: 'battlemaster' },
    auctioneer: { event: 'AUCTION_HOUSE_SHOW', gossip: null },
  };
  /**
   * Walk to an NPC and open its `service` window ('vendor', 'repair', 'trainer', 'flightmaster',
   * 'banker', 'innkeeper', 'battlemaster', 'auctioneer'), through its gossip menu if it opens on
   * one. `npc` is a row, a guid, or omitted for the nearest NPC offering the service. Resolves
   * true when the window is open.
   */
  auto.open = async (service, npc) => {
    const spec = SERVICES[service];
    if (!spec) throw new Error(`open: unknown service ${service}`);
    const u = npc ? auto.find(npc) ?? npc : auto.nearest({ service });
    if (!u) throw new Error(`open: no ${service} in sight`);
    const shown = w.once(spec.event, 4000);
    const opened = await auto.interact(u);
    if (opened === spec.event) return true;
    if (opened === 'GOSSIP_SHOW' && spec.gossip) {
      // `GetGossipOptions` answers (text, type) pairs.
      const [opts] = await w.lua('return {GetGossipOptions()}');
      const types = seq(opts).filter((_, k) => k % 2 === 1);
      const i = types.indexOf(spec.gossip);
      if (i < 0) return false;
      const wait = w.once(spec.event, 3000);
      await w.lua(`SelectGossipOption(${i + 1})`);
      return !!(await wait);
    }
    return !!(await shown);
  };
  /** Walk to a vendor and open it (a row, a guid, or the nearest). */
  auto.openVendor = (vendor) => auto.open('vendor', vendor?.service ? null : vendor);

  /**
   * The open vendor's wares: `{index, name, price, quantity, available}`. A name the client's item
   * cache has not answered yet reads empty; this waits up to ~2 s for the cache to fill.
   */
  auto.wares = async () => {
    let wares = [];
    for (let i = 0; i < 6; i++) {
      const [rows] = await w.lua(
        'local t = {} for i = 1, GetMerchantNumItems() do local n, _, p, q, a = GetMerchantItemInfo(i) table.insert(t, {i, n or "", p or 0, q or 1, a or -1}) end return t',
      );
      wares = seq(rows).map((r) => seq(r, 5)).map(([index, name, price, quantity, available]) => ({ index, name, price, quantity, available }));
      if (wares.every((x) => x.name)) break;
      await sleep(400);
    }
    return wares;
  };
  /** Buy `count` of a ware by name (or RegExp) from the open vendor. */
  auto.buy = async (name, count = 1) => {
    const it = (await auto.wares()).find((i) => (name instanceof RegExp ? name.test(i.name ?? '') : i.name === name));
    if (!it) return false;
    await w.lua(`BuyMerchantItem(${it.index}, ${count})`);
    return true;
  };
  /** Sell bag items matching `filter(item)` to the open vendor. Resolves with how many. */
  auto.sell = async (filter) => {
    const items = (await auto.bags()).filter(filter);
    for (const it of items) {
      await w.lua(`UseContainerItem(${it.bag}, ${it.slot})`);
      await sleep(120);
    }
    return items.length;
  };
  /** Sell every grey (poor-quality) item. */
  auto.sellJunk = () => auto.sell((i) => i.quality === 0);
  /** Repair everything at the open vendor if it repairs. Resolves with the cost in copper. */
  auto.repair = async () => {
    const [cost, can] = await w.lua('local c, n = GetRepairAllCost() return c or 0, n')
    if (can && cost > 0) await w.lua('RepairAllItems()');
    return can ? cost : 0;
  };
  auto.closeVendor = () => w.lua('CloseMerchant()');
  auto.closeTrainer = () => w.lua('CloseTrainer()');

  // ── quests ──

  /** The quest log: `{index, title, level, complete, header, objectives: [{text, done}]}`. */
  auto.questLog = async () => {
    const [rows] = await w.lua(`local t = {}
      for i = 1, GetNumQuestLogEntries() do
        local title, level, tag, header, collapsed, complete = GetQuestLogTitle(i)
        local obj = {}
        if not header then
          SelectQuestLogEntry(i)
          for j = 1, (GetNumQuestLeaderBoards and GetNumQuestLeaderBoards() or 0) do
            local text, kind, done = GetQuestLogLeaderBoard(j)
            table.insert(obj, {text or '', done and true or false})
          end
        end
        table.insert(t, {i, title or '', level or 0, complete or 0, header and true or false, obj})
      end
      return t`);
    return seq(rows).map((r) => seq(r, 6)).map(([index, title, level, complete, header, obj]) => ({
      index, title, level, header, complete: complete === 1,
      failed: complete === -1,
      objectives: seq(obj).map((o) => seq(o, 2)).map(([text, done]) => ({ text, done: !!done })),
    }));
  };
  /** Abandon a quest by title. */
  auto.abandon = (title) =>
    w.lua(`for i = 1, GetNumQuestLogEntries() do if GetQuestLogTitle(i) == ${luaStr(title)} then SelectQuestLogEntry(i) SetAbandonQuest() AbandonQuest() return true end end`);

  /**
   * Talk to a quest giver and do everything its dialogs offer: hand in every active quest it will
   * take (choosing reward `reward(choices)` → index, default the first), then accept every
   * available quest (unless `accept: false`). Each quest is tried once; the dialog itself says
   * whether a hand-in is ready (talk-to quests never show complete in the log). Resolves with
   * `{ accepted: [titles], completed: [titles] }`.
   */
  auto.questGiver = async (unit, { accept = true, reward } = {}) => {
    const done = { accepted: [], completed: [] };
    const tried = new Set();
    const DIALOGS = ['QUEST_GREETING', 'GOSSIP_SHOW', 'QUEST_DETAIL', 'QUEST_PROGRESS', 'QUEST_COMPLETE'];
    // Listen before acting: the answer can arrive within the same frame as the call.
    const step = (ms = 3000) => Promise.race(DIALOGS.map((e) => w.once(e, ms).then((p) => (p ? e : null))));
    let opened = await auto.interact(unit);
    let reopens = 0;
    for (let guard = 0; guard < 40; guard++) {
      if (!opened) {
        // An accept or a hand-in closes the dialog; open it again while there may be more.
        if (reopens++ >= 6) break;
        opened = await auto.interact(unit).catch(() => null);
        if (!opened) break;
      }
      let next = null;
      if (opened === 'GOSSIP_SHOW' || opened === 'QUEST_GREETING') {
        const gossip = opened === 'GOSSIP_SHOW';
        const [active, avail] = await w.lua(gossip
          ? 'return {GetGossipActiveQuests()}, {GetGossipAvailableQuests()}'
          : 'local a, v = {}, {} for i = 1, GetNumActiveQuests() do table.insert(a, GetActiveTitle(i)) table.insert(a, 0) end for i = 1, GetNumAvailableQuests() do table.insert(v, GetAvailableTitle(i)) table.insert(v, 0) end return a, v');
        // Both lists are (title, level) pairs.
        const titles = (list) => seq(list).filter((_, k) => k % 2 === 0);
        const a = titles(active).findIndex((t) => !tried.has(`a:${t}`));
        const v = accept ? titles(avail).findIndex((t) => !tried.has(`v:${t}`)) : -1;
        if (a >= 0) {
          tried.add(`a:${titles(active)[a]}`);
          next = step();
          await w.lua(gossip ? `SelectGossipActiveQuest(${a + 1})` : `SelectActiveQuest(${a + 1})`);
        } else if (v >= 0) {
          tried.add(`v:${titles(avail)[v]}`);
          next = step();
          await w.lua(gossip ? `SelectGossipAvailableQuest(${v + 1})` : `SelectAvailableQuest(${v + 1})`);
        } else {
          await w.lua(gossip ? 'CloseGossip()' : 'DeclineQuest()');
          break;
        }
      } else if (opened === 'QUEST_DETAIL') {
        const [title] = await w.lua('return GetTitleText()');
        tried.add(`v:${title}`);
        next = step(1500);
        if (accept) {
          await w.lua('AcceptQuest()');
          done.accepted.push(title);
        } else {
          await w.lua('DeclineQuest()');
        }
      } else if (opened === 'QUEST_PROGRESS') {
        const [title, ok] = await w.lua('return GetTitleText(), IsQuestCompletable()');
        tried.add(`a:${title}`);
        next = step(1500);
        await w.lua(ok ? 'CompleteQuest()' : 'DeclineQuest()');
      } else if (opened === 'QUEST_COMPLETE') {
        const [title, n] = await w.lua('return GetTitleText(), GetNumQuestChoices()');
        tried.add(`a:${title}`);
        let choice = 1;
        if (n > 1 && reward) {
          const [names] = await w.lua(`local t = {} for i = 1, ${n} do table.insert(t, (GetQuestItemInfo('choice', i)) or "") end return t`);
          choice = Math.min(n, Math.max(1, (await reward(seq(names))) + 1 || 1));
        }
        next = step(1500);
        await w.lua(`GetQuestReward(${n > 0 ? choice : 'nil'})`);
        done.completed.push(title);
      }
      opened = await next;
    }
    return done;
  };

  // ── trainers ──

  /** Walk to a trainer (a row, a guid, or the nearest) and open its list. */
  auto.openTrainer = (trainer) => auto.open('trainer', trainer);
  /** Learn everything the open trainer offers that we can afford. Resolves with the names. */
  auto.trainAll = async () => {
    const [learned] = await w.lua(`local t = {}
      for i = GetNumTrainerServices(), 1, -1 do
        local name, rank, kind = GetTrainerServiceInfo(i)
        if kind == 'available' and (GetTrainerServiceCost(i) or 0) <= GetMoney() then
          BuyTrainerService(i) table.insert(t, name .. (rank and rank ~= '' and (' (' .. rank .. ')') or ''))
        end
      end
      return t`);
    spellIndex = null;
    return seq(learned);
  };

  // ── rest, death ──

  /**
   * Recover out of combat: eat/drink the named items if given (else sit), and wait until health
   * and power are above `hp`/`power` (fractions), combat starts, or `timeout`. Resolves true if
   * recovered.
   */
  auto.rest = async ({ hp = 0.8, power = 0.8, food, drink, timeout = 40000 } = {}) => {
    const me = auto.me();
    if (!me || me.inCombat) return false;
    const needHp = auto.hp() < hp;
    const needPower = me.powerType === 0 && auto.power() < power;
    if (!needHp && !needPower) return true;
    let ate = false;
    if (needHp && food) ate = await auto.useItem(food);
    if (needPower && drink) ate = (await auto.useItem(drink)) || ate;
    if (!ate && me.standState === 0) w.fire('SITORSTAND');
    const ok = await w.waitFor(
      (s) => s.self.inCombat || (auto.hp(s.self) >= hp && (s.self.powerType !== 0 || auto.power(s.self) >= power)),
      timeout,
    );
    if (auto.me()?.standState !== 0) w.fire('SITORSTAND');
    return ok && !auto.me()?.inCombat;
  };

  /** Release the spirit (to the graveyard). */
  auto.release = () => w.lua('RepopMe()');
  /**
   * As a ghost, run back to where we died and resurrect there. Pass `route` (waypoints from the
   * graveyard) where a straight line would hit a wall. Resolves when alive.
   */
  auto.corpseRun = async ({ route, timeout = 300000 } = {}) => {
    if (!auto.me()?.ghost) {
      if (auto.me()?.dead) {
        await auto.release();
        await w.waitFor((s) => s.self?.ghost, 15000);
      } else return true;
    }
    if (!auto.deathPos) throw new Error('corpseRun: no recorded death position');
    if (route) await auto.followRoute(route);
    await auto.moveTo(auto.deathPos, { tolerance: 20, timeout });
    await w.waitFor(() => true, 500);
    for (let i = 0; i < 40; i++) {
      const [delay] = await w.lua('return GetCorpseRecoveryDelay()');
      if (!delay) break;
      await sleep(1000);
    }
    await w.lua('RetrieveCorpse()');
    return w.waitFor((s) => s.self && !s.self.dead && !s.self.ghost, 10000);
  };

  return auto;
}
