import { vat } from './tax.js';

// Total of a cart in cents, VAT included. `discountCents` comes off the order.
export function total(items, { discountCents = 0 } = {}) {
  const gross = items.reduce((sum, i) => sum + i.cents + vat(i.cents, i.country), 0);
  return gross - discountCents;
}
