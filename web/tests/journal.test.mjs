import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

test('browser journal retains bounded ordered rows across stop/start and exports CSV', async () => {
  const rust = await readFile(new URL('../../crates/benilla-app/src/perf/journal_web.rs', import.meta.url), 'utf8');
  const source = rust.match(/inline_js = r#"([\s\S]*?)"#/)[1];
  const elements = new Map();
  let clicked;
  globalThis.window = {};
  globalThis.document = {
    getElementById: (id) => elements.get(id),
    createElement: (tag) => ({
      tag, style: {}, addEventListener() {}, remove() {},
      click() { clicked = this; },
    }),
    body: { appendChild(el) { if (el.id) elements.set(el.id, el); } },
  };
  const journal = await import('data:text/javascript;base64,' + Buffer.from(source).toString('base64'));
  const header = '# adapter\nt,x,mean_ms\n';
  journal.begin(header);
  journal.append('0,10,16\n');
  journal.begin('# do not reset existing journal\n');
  assert.equal(journal.journalText(), header + '0,10,16\n');
  assert.equal(elements.size, 1, 're-enabling must not duplicate the button');
  for (let i = 1; i <= 3605; i++) journal.append(`${i},10,16\n`);
  const csv = window.__wenilla_fps_journal.text();
  const lines = csv.trimEnd().split('\n');
  assert.equal(lines.length, 3602);
  assert.equal(lines[2], '6,10,16');
  assert.equal(lines.at(-1), '3605,10,16');
  window.__wenilla_fps_journal.download();
  assert.equal(clicked.download, 'fps-journal.csv');
  assert.equal(await (await fetch(clicked.href)).text(), csv);
});
