// examples/grind.js — a complete leveling loop on `wenilla.auto` (web/automate.js): pick the
// nearest fair fight, kill it, loot it, rest, sell and repair when the bags fill, turn in and
// pick up quests at givers it passes, train at a class trainer it has seen, and run back from
// the graveyard after a death. Run with `?bridge=grind`, or `import('./examples/grind.js')` and
// `grind.start(options)` in the console; `grind.stop()` ends it.
//
// A sketch to adapt, not a finished bot: the rotation is per class (two are given), the
// navigation is straight lines (record `routes` with `wenilla.auto.recordRoute()` for anything
// with walls), and it stays where it starts — set `home` and `leash` to move it.
import { wenilla } from '../bridge.js';

/** Per-class rotations: `ctx` has `cast`, `ready`, `hasAura`, `target`, `me` (see auto.fight). */
const ROTATIONS = {
  WARRIOR: async ({ ready, cast, me, target }) => {
    if (me.power >= 15 && (await ready('Heroic Strike'))) await cast('Heroic Strike');
    if (target.health / target.maxHealth < 0.2 && (await ready('Execute'))) await cast('Execute');
  },
  MAGE: async ({ ready, cast }) => {
    if (await ready('Frostbolt')) await cast('Frostbolt');
    else if (await ready('Fireball')) await cast('Fireball');
  },
};

const DEFAULTS = {
  /** Stay within `leash` yards of `home` (default: where it starts). */
  home: null,
  leash: 60,
  /** Fight units this many levels below/above ours. */
  below: 4,
  above: 1,
  /** Also pull neutral (yellow) units, not only hostile ones — wolves, boars, crabs. */
  neutral: false,
  /** Names (exact) or RegExps never to pull. */
  avoid: [],
  /** Rest below these fractions; eat/drink these item names if set. */
  restHp: 0.55,
  restPower: 0.35,
  food: null,
  drink: null,
  /** Sell junk and repair when free slots drop below this. */
  minFreeSlots: 2,
  /** A caster stands off at its spell range; melee classes walk in. */
  range: null,
  rotation: null,
  /** Quest givers it passes within this range get visited (turn in, accept). 0 disables. */
  questRadius: 25,
  log: (...a) => console.log('grind:', ...a),
};

export const grind = {
  running: false,
  options: { ...DEFAULTS },
  stats: { kills: 0, deaths: 0, looted: 0, sold: 0, quests: 0 },
  async start(opts = {}) {
    if (this.running) return;
    await wenilla.waitFor((s) => s.session.state === 'inworld' && s.self, 120000);
    const auto = wenilla.auto;
    this.options = { ...DEFAULTS, ...opts };
    const o = this.options;
    o.home ??= [...auto.me().pos];
    const [klass] = (await wenilla.lua("local _, c = UnitClass('player') return c").catch(() => [null])) ?? [];
    o.rotation ??= ROTATIONS[klass] ?? null;
    o.range ??= klass === 'MAGE' || klass === 'WARLOCK' || klass === 'PRIEST' ? 25 : 3.5;
    o.log(`starting: class ${klass}, home ${o.home.map((v) => v.toFixed(0)).join(',')}, leash ${o.leash}`);
    this.running = true;
    this._loop().catch((e) => {
      o.log('stopped on error', e);
      this.running = false;
    });
  },
  stop() {
    this.running = false;
    wenilla.stop();
    wenilla.releaseAll();
  },

  async _loop() {
    const auto = wenilla.auto;
    const o = this.options;
    const visited = new Set();
    while (this.running) {
      const me = auto.me();
      if (!me) {
        await wenilla.waitFor((s) => s.self, 60000);
        continue;
      }

      // Dead: release and run back.
      if (me.dead || me.ghost) {
        this.stats.deaths++;
        o.log('dead — corpse run');
        await auto.corpseRun().catch((e) => o.log('corpse run failed:', e.message));
        continue;
      }

      // Under attack: fight back whoever is on us.
      const attacker = auto.attackers()[0];
      if (attacker) {
        await this._kill(attacker);
        continue;
      }

      // Hurt or drained: rest.
      if (auto.hp() < o.restHp || (me.powerType === 0 && auto.power() < o.restPower)) {
        await auto.rest({ hp: 0.95, power: 0.95, food: o.food, drink: o.drink });
        continue;
      }

      // Bags full: the nearest known vendor (seen this session), then back.
      if ((await auto.freeSlots()) < o.minFreeSlots) {
        await this._vendor();
        continue;
      }

      // A quest giver with something for us nearby.
      if (o.questRadius) {
        const giver = auto.nearest((u) =>
          (u.questStatus === 'available' || u.questStatus === 'reward') && u.dist <= o.questRadius && !visited.has(u.guid + u.questStatus));
        if (giver) {
          visited.add(giver.guid + giver.questStatus);
          const r = await auto.questGiver(giver).catch((e) => (o.log('quest giver:', e.message), null));
          if (r) {
            this.stats.quests += r.completed.length;
            if (r.accepted.length || r.completed.length) o.log('quests', r);
          }
          continue;
        }
      }

      // A lootable body nearby.
      const body = auto.nearest({ kind: 'unit', dead: true, lootable: true, maxDist: 30 });
      if (body) {
        this.stats.looted += await auto.loot(body).catch(() => 0);
        continue;
      }

      // The next fight.
      const lvl = me.level;
      const prey = auto.nearest(
        (u) => u.kind === 'unit' && (u.hostile || (o.neutral && u.reaction === 4)) && !u.dead && !u.tapped && !u.isPlayer &&
          u.level >= lvl - o.below && u.level <= lvl + o.above &&
          (!u.targetGuid || u.targetGuid === me.guid) &&
          wenilla.dist(u.pos, o.home) <= o.leash &&
          !o.avoid.some((a) => (a instanceof RegExp ? a.test(u.name ?? '') : a === u.name)),
      );
      if (prey) {
        await this._kill(prey);
        continue;
      }

      // Nothing to do: walk home, then wait for respawns.
      if (wenilla.dist(me.pos, o.home) > 10) {
        await auto.goto(o.home, { tolerance: 5 }).catch(() => {});
      } else {
        await new Promise((r) => setTimeout(r, 2000));
      }
    }
  },

  async _kill(unit) {
    const auto = wenilla.auto;
    const o = this.options;
    o.log(`fighting ${unit.name} (${unit.level})`);
    const result = await auto.fight(unit, { range: o.range, melee: o.range <= 5, rotation: o.rotation });
    if (result === 'won') {
      this.stats.kills++;
      await new Promise((r) => setTimeout(r, 600)); // the body's lootable flag lands a moment later
      const body = auto.find(unit);
      if (body?.lootable) this.stats.looted += await auto.loot(body).catch(() => 0);
    }
    o.log(`${unit.name}: ${result}`);
  },

  async _vendor() {
    const auto = wenilla.auto;
    const o = this.options;
    const here = auto.me().pos;
    const known = [...auto.npcs.values()]
      .filter((n) => n.services.includes('vendor') && n.map === wenilla.state.map?.id)
      .sort((a, b) => wenilla.dist(a.pos, here) - wenilla.dist(b.pos, here))[0];
    if (!known) {
      o.log('bags full and no vendor seen yet — continuing');
      o.minFreeSlots = 0;
      return;
    }
    o.log(`selling at ${known.name}`);
    await auto.goto(known.pos, { tolerance: 4 }).catch((e) => o.log("to vendor:", e.message));
    if (await auto.openVendor(known.guid).catch(() => false)) {
      this.stats.sold += await auto.sellJunk();
      await auto.repair();
      await auto.closeVendor();
    }
    await auto.goto(o.home, { tolerance: 5 }).catch(() => {});
  },
};

window.grind = grind;
// `?bridge=grind` starts it with the defaults.
if (new URLSearchParams(location.search).get('bridge') === 'grind') grind.start();
