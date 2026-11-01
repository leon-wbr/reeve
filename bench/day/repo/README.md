# shop-kit

Prices, VAT, carts, inventory and sales reports for the shop.

- `formatPrice(cents, currency)`: `"12.34 EUR"`.
- `vat(cents, country)`: VAT in cents for DE and FR.
- `total(items, { discountCents })`: cart total in cents, VAT included.
- `needsReorder(item, config)`: true when stock is below `config.lowStock`.
- `slugify(name)`: URL slug.
- `monthlyTotals(fixture)`: net sales per month.
