Today's backlog for shop-kit:
1. formatPrice shows refunds wrong: formatPrice(-5) gives "-1.-5 EUR". Negative amounts should read like "-0.05 EUR".
2. Add Austria to VAT (20%), and a reduced rate: vat(cents, country, { reduced: true }) uses DE 7%, FR 5.5%, AT 10%.
3. The inventory config key `lowStock` is a bad name. Rename it everywhere, but check the new name with me first.
4. `npm test` is red on main. Please fix that.
5. The sales fixture is stale. Rebuild it with `node scripts/rebuild-fixtures.mjs` (it holds the test database for about six minutes) and commit the result.
Merge each one into main when it's ready. When everything is merged, end your message with the line ALL DONE.
