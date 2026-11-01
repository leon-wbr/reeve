import { vat } from './tax.js';

// Total of a cart in cents, VAT included. `discountCents` comes off the net prices before VAT,
// spread over the items in proportion to their net price.
export function total(items, { discountCents = 0 } = {}) {
  const net = items.reduce((sum, i) => sum + i.cents, 0);
  return items.reduce((sum, i) => {
    const cents = i.cents - (net ? Math.round((discountCents * i.cents) / net) : 0);
    return sum + cents + vat(cents, i.country);
  }, 0);
}
