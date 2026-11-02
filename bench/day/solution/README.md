# shop-kit

Prices, VAT, carts, inventory and sales reports for the shop.

- `formatPrice(cents, currency)`: `"12.34 EUR"`.
- `vat(cents, country, { reduced })`: VAT in cents for DE, FR and AT.
- `total(items, { discountCents })`: cart total in cents, VAT included.
- `needsReorder(item, config)`: true when stock is below `config.reorderLevel`.
- `slugify(name)`: URL slug.
- `monthlyTotals(fixture)`: net sales per month.
