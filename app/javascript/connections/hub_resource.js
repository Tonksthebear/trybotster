function identity(value) {
  return value;
}

export class HubResource {
  constructor({
    initialValue = null,
    load = null,
    normalize = identity,
  } = {}) {
    this._initialValue = initialValue;
    this._value = initialValue;
    this._load = load;
    this._normalize = normalize;
    this._loaded = false;
    this._pending = null;
    this._subscribers = new Set();
  }

  current() {
    return this._value;
  }

  value() {
    return this.current();
  }

  toJSON() {
    return this.current();
  }

  isLoaded() {
    return this._loaded;
  }

  invalidate() {
    this._loaded = false;
  }

  reset() {
    this._value = this._initialValue;
    this._loaded = false;
    this._pending = null;
  }

  set(value) {
    this._value = this._normalize(value);
    this._loaded = true;
    this.#emit(this._value);
    return this._value;
  }

  async load(options = {}) {
    const { force = false } = options;
    if (!force && this._loaded) return this._value;
    if (!force && this._pending) return this._pending;

    if (!this._load) {
      this._loaded = true;
      return this._value;
    }

    const pending = Promise.resolve(this._load(options))
      .then(() => this._value)
      .finally(() => {
        if (this._pending === pending) {
          this._pending = null;
        }
      });

    this._pending = pending;
    return pending;
  }

  onChange(callback) {
    this._subscribers.add(callback);
    if (this._loaded) {
      try {
        callback(this._value);
      } catch (error) {
        console.error("[HubResource] Subscriber error:", error);
      }
    }
    return () => {
      this._subscribers.delete(callback);
    };
  }

  #emit(value) {
    for (const callback of this._subscribers) {
      try {
        callback(value);
      } catch (error) {
        console.error("[HubResource] Subscriber error:", error);
      }
    }
  }
}

export class HubCollection extends HubResource {
  constructor(options = {}) {
    super({
      initialValue: [],
      normalize: (value) => (Array.isArray(value) ? value : []),
      ...options,
    });
  }

  async all(options = {}) {
    return this.load(options);
  }

  first() {
    return this.current()[0] || null;
  }

  find(id, key = "id") {
    return this.current().find((record) => record?.[key] === id) || null;
  }

  filter(callback) {
    return this.current().filter(callback);
  }

  map(callback) {
    return this.current().map(callback);
  }

  forEach(callback) {
    return this.current().forEach(callback);
  }
}

export class HubScopedResource {
  constructor({
    createEntry,
  }) {
    this._createEntry = createEntry;
    this._entries = new Map();
  }

  forTarget(targetId = null) {
    const key = this.#keyFor(targetId);
    if (!this._entries.has(key)) {
      this._entries.set(key, this._createEntry(targetId));
    }
    return this._entries.get(key);
  }

  current(targetId = null) {
    return this.forTarget(targetId).current();
  }

  isLoaded(targetId = null) {
    return this.forTarget(targetId).isLoaded();
  }

  load(targetId = null, options = {}) {
    return this.forTarget(targetId).load(options);
  }

  set(targetId = null, value) {
    return this.forTarget(targetId).set(value);
  }

  invalidate(targetId = null) {
    this.forTarget(targetId).invalidate();
  }

  retain(targetIds = []) {
    const keep = new Set(
      (Array.isArray(targetIds) ? targetIds : [])
        .filter(Boolean)
        .map((targetId) => this.#keyFor(targetId)),
    );
    keep.add(this.#keyFor(null));

    for (const key of this._entries.keys()) {
      if (!keep.has(key)) {
        this._entries.delete(key);
      }
    }
  }

  #keyFor(targetId) {
    return targetId || "__default__";
  }
}
