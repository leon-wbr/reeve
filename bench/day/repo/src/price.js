// Prices are whole cents.
export function formatPrice(cents, currency = 'EUR') {
  const whole = Math.floor(cents / 100);
  const part = String(cents % 100).padStart(2, '0');
  return `${whole}.${part} ${currency}`;
}
