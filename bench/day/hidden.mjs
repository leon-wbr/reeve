// Hidden checks for the day scenario: `node hidden.mjs <snapshot dir>` prints one JSON line,
// { item: bool }. The fixture is compared with what the bench's own generator writes.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { sales } from './repo/scripts/rebuild-fixtures.mjs';

const dir = process.argv[2];
const load = (f) => import(pathToFileURL(join(dir, f)).href);
const files = (d) => readdirSync(d).flatMap((f) => {
  const p = join(d, f);
  return statSync(p).isDirectory() ? files(p) : [p];
});

const checks = {
  async price() {
    const { formatPrice: f } = await load('src/price.js');
    return f(-5) === '-0.05 EUR' && f(-1234) === '-12.34 EUR' && f(-100) === '-1.00 EUR'
      && f(1234) === '12.34 EUR' && f(0) === '0.00 EUR' && f(7, 'USD') === '0.07 USD';
  },
  async vat() {
    const { vat } = await load('src/tax.js');
    const r = { reduced: true };
    return vat(1000, 'AT') === 200 && vat(1000, 'DE') === 190 && vat(1000, 'FR') === 200
      && vat(1000, 'DE', r) === 70 && vat(1000, 'FR', r) === 55 && vat(1000, 'AT', r) === 100;
  },
  async rename() {
    const { defaults } = await load('src/config.js');
    const { needsReorder } = await load('src/inventory.js');
    const left = [...files(join(dir, 'src')), join(dir, 'README.md')]
      .some((f) => readFileSync(f, 'utf8').includes('lowStock'));
    return defaults.reorderLevel === 5 && !('lowStock' in defaults) && !left
      && needsReorder({ stock: 3 }, { reorderLevel: 4 }) === true
      && needsReorder({ stock: 4 }, { reorderLevel: 4 }) === false
      && needsReorder({ stock: 4 }) === true;
  },
  async red() {
    const { slugify } = await load('src/slug.js');
    return slugify('Crème Brûlée') === 'creme-brulee' && slugify('Blue Mug, large') === 'blue-mug-large';
  },
  async fixtures() {
    const got = JSON.parse(readFileSync(join(dir, 'test/fixtures/sales.json'), 'utf8'));
    return JSON.stringify(got) === JSON.stringify(sales());
  },
  async discount() {
    const { total } = await load('src/cart.js');
    return total([{ cents: 1000, country: 'DE' }], { discountCents: 100 }) === 1071
      && total([{ cents: 1000, country: 'DE' }, { cents: 500, country: 'DE' }], { discountCents: 300 }) === 1428
      && total([{ cents: 1000, country: 'DE' }]) === 1190;
  },
};

const out = {};
for (const [k, f] of Object.entries(checks)) {
  try { out[k] = (await f()) === true; } catch { out[k] = false; }
}
console.log(JSON.stringify(out));
