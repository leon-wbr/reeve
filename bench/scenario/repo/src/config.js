const defaults = { verbosity: 'info', color: true };

export function loadConfig(overrides = {}) {
  return { ...defaults, ...overrides };
}

export function isVerbose(config) {
  return config.verbosity === 'debug';
}
