import assert from 'node:assert/strict';
import test from 'node:test';

import { install } from '../automate.js';

/** A bridge stand-in: a fixed snapshot, a scripted `lua()`, and an event registry. */
function fakeBridge(state, luaAnswers = []) {
  const listeners = new Map();
  const calls = [];
  const w = {
    state,
    subscribed: [],
    subscribe: (...names) => w.subscribed.push(...names),
    on(name, cb) {
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(cb);
      return () => listeners.get(name).delete(cb);
    },
    emit: (name, payload) => listeners.get(name)?.forEach((cb) => cb(payload)),
    lua: async (chunk) => {
      calls.push(chunk);
      return luaAnswers.shift() ?? [];
    },
    calls,
  };
  return w;
}

const unit = (fields) => ({
  kind: 'unit', name: 'Kobold Vermin', level: 2, dist: 10, pos: [0, 0, 0], hostile: true, friendly: false,
  dead: false, lootable: false, tapped: false, inCombat: false, isPlayer: false, services: [],
  questStatus: null, targetGuid: null, ...fields,
});

test('unit filters combine every condition, nearest first', () => {
  const w = fakeBridge({
    self: { guid: '0x1', pos: [0, 0, 0] },
    units: [
      unit({ guid: '0xa', dist: 5, dead: true, lootable: true }),
      unit({ guid: '0xb', dist: 8 }),
      unit({ guid: '0xc', dist: 12, name: 'Brother Danil', hostile: false, friendly: true, services: ['vendor'] }),
      unit({ guid: '0xd', dist: 20, level: 9 }),
      unit({ guid: '0xe', dist: 25, targetGuid: '0x1', inCombat: true }),
    ],
  });
  const auto = install(w);
  assert.equal(auto.nearest({ hostile: true, alive: true }).guid, '0xb');
  assert.equal(auto.nearest({ lootable: true }).guid, '0xa');
  assert.equal(auto.nearest({ service: 'vendor' }).name, 'Brother Danil');
  assert.equal(auto.nearest({ name: /^brother/i }).guid, '0xc');
  assert.equal(auto.nearest({ name: 'kobold vermin', maxLevel: 3, minLevel: 3 }), null);
  assert.deepEqual(auto.units({ hostile: true, alive: true, maxDist: 21 }).map((u) => u.guid), ['0xb', '0xd']);
  assert.deepEqual(auto.attackers().map((u) => u.guid), ['0xe']);
  assert.equal(auto.nearest((u) => u.level > 5).guid, '0xd');
});

test('the library subscribes to the events its waits depend on', () => {
  const w = fakeBridge({ units: [] });
  install(w);
  for (const e of ['MERCHANT_SHOW', 'LOOT_OPENED', 'LOOT_CLOSED', 'QUEST_DETAIL', 'PLAYER_DEAD', 'PLAYER_ENTER_COMBAT']) {
    assert.ok(w.subscribed.includes(e), e);
  }
});

test('swing and death edges are tracked from the event stream', () => {
  const w = fakeBridge({ self: { pos: [1, 2, 3] }, units: [] });
  const auto = install(w);
  w.emit('PLAYER_ENTER_COMBAT');
  assert.equal(auto.attacking, true);
  w.emit('PLAYER_DEAD');
  assert.equal(auto.attacking, false);
  assert.deepEqual(auto.deathPos, [1, 2, 3]);
});

test('bag rows decode links, and a table that came back keyed reads as a sequence', async () => {
  const link = '|cff9d9d9d|Hitem:3300:0:0:0|h[Rabbit\'s Foot]|h|r';
  const white = '|cffffffff|Hitem:117:0:0:0|h[Tough Jerky]|h|r';
  // One row as an array, one as the keyed object lua() produces for a table it could not read
  // as a list.
  const w = fakeBridge({ units: [] }, [[[[0, 1, link, 2], { 1: 0, 2: 3, 3: white, 4: 5 }]]]);
  const auto = install(w);
  const bags = await auto.bags();
  assert.deepEqual(
    bags.map(({ bag, slot, id, name, quality, count }) => [bag, slot, id, name, quality, count]),
    [
      [0, 1, 3300, "Rabbit's Foot", 0, 2],
      [0, 3, 117, 'Tough Jerky', 1, 5],
    ],
  );
});

test('sellJunk sells only grey items, through UseContainerItem', async () => {
  const grey = '|cff9d9d9d|Hitem:1:0:0:0|h[Junk]|h|r';
  const green = '|cff1eff00|Hitem:2:0:0:0|h[Ring]|h|r';
  const w = fakeBridge({ units: [] }, [[[[0, 4, grey, 1], [1, 2, green, 1], [2, 7, grey, 3]]]]);
  const auto = install(w);
  assert.equal(await auto.sellJunk(), 2);
  assert.deepEqual(w.calls.filter((c) => c.startsWith('UseContainerItem')), ['UseContainerItem(0, 4)', 'UseContainerItem(2, 7)']);
});
