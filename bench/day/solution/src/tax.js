const RATES = { DE: 19, FR: 20, AT: 20 };
const REDUCED = { DE: 7, FR: 5.5, AT: 10 };

// VAT on an amount in cents, rounded to whole cents.
export function vat(cents, country, { reduced = false } = {}) {
  const rate = (reduced ? REDUCED : RATES)[country];
  if (rate === undefined) throw new Error(`no VAT rate for ${country}`);
  return Math.round((cents * rate) / 100);
}
