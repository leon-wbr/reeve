export function shuffle(xs) {
  return [...xs].sort(() => Math.random() - 0.5);
}
