import { defaults } from './config.js';

// Whether an item's stock is low enough to reorder.
export function needsReorder(item, config = defaults) {
  return item.stock < config.lowStock;
}
