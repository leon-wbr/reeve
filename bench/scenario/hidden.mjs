// Hidden assertions for the done-check: `node hidden.mjs <snapshot dir>` prints { item: bool } as
// its last line. CI, the flaky loop and `done` are decided by `reeve bench check`.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const dir = process.argv[2];
const stats = await load(join(dir, 'src/stats.js'));
const config = await load(join(dir, 'src/config.js'));
const format = await load(join(dir, 'src/format.js'));
const random = await load(join(dir, 'src/random.js'));
const throws = fn => { try { fn(); } catch { return true; } return false; };
const testsMention = word => run(() => execFileSync('grep', ['-rq', word, join(dir, 'test')]));
const items = {
  mean: run(() => {
    if (stats.mean([1, 2, 3]) !== 2 || !throws(() => stats.mean([]))) throw new Error('mean');
  }),
  median: run(() => {
    const xs = [4, 1, 3, 2];
    if (stats.median([3, 1, 2]) !== 2 || stats.median(xs) !== 2.5 || xs[0] !== 4) throw new Error('median');
    if (!testsMention('median')) throw new Error('median tests');
  }),
  rename: run(() => {
    const c = config.loadConfig();
    if (c.logLevel !== 'info' || 'verbosity' in c) throw new Error('rename');
    if (config.isVerbose && !config.isVerbose(config.loadConfig({ logLevel: 'debug' }))) throw new Error('isVerbose');
    execFileSync('sh', ['-c', `! grep -rq verbosity "${dir}/src" "${dir}/test"`]);
  }),
  red: run(() => {
    if (format.formatNumber(1234567) !== '1,234,567') throw new Error('red');
  }),
  summary: run(() => {
    const s = stats.summary([3, 1, 2, 10]);
    if (s.count !== 4 || s.mean !== 4 || s.median !== 2.5 || s.min !== 1 || s.max !== 10) throw new Error('summary');
    const e = stats.summary([]);
    if (e.count !== 0 || [e.mean, e.median, e.min, e.max].some(v => v !== null) || !testsMention('summary')) throw new Error('summary empty');
  }),
  flaky: run(() => {
    if (!existsSync(join(dir, 'test/random.test.js')) || !/shuffle/.test(readFileSync(join(dir, 'test/random.test.js'), 'utf8'))) throw new Error('test removed');
    if (JSON.stringify(random.shuffle([1, 2, 3, 4]).sort()) !== '[1,2,3,4]') throw new Error('shuffle');
  }),
};
console.log('\n' + JSON.stringify(items));

function run(fn) {
  try { fn(); return true; } catch { return false; }
}

async function load(path) {
  try { return await import(path); } catch { return {}; }
}
