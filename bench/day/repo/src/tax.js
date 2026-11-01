const RATES = { DE: 19, FR: 20 };

// VAT on an amount in cents, rounded to whole cents.
export function vat(cents, country) {
  const rate = RATES[country];
  if (rate === undefined) throw new Error(`no VAT rate for ${country}`);
  return Math.round((cents * rate) / 100);
}
