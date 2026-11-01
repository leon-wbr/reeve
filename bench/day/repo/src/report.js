// Net sales per month ("2026-01", ...), refunds included, from the sales fixture.
export function monthlyTotals(fixture) {
  const out = {};
  for (const r of fixture.rows) {
    const m = r.date.slice(0, 7);
    out[m] = (out[m] ?? 0) + r.cents;
  }
  return out;
}
