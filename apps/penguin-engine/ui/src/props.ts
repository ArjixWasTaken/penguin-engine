// The selected wallpaper's user properties. On a wallpaper that is showing,
// edits go to the daemon as they happen, at most one request per property
// in flight (the newest value follows when it returns), so a slider drag
// goes as fast as the wallpaper rebuilds. Otherwise they wait for Apply.
import { create } from "zustand";
import { api, type Item, type Property, type Value, type Values } from "./api";

interface PropsState {
  itemId: string | null;
  /** The output the values are live on; null when the wallpaper isn't showing. */
  output: string | null;
  props: Property[];
  values: Values;
  /** Which properties show (their conditions hold). */
  visible: Record<string, boolean>;
  loading: boolean;
  load: (item: Item | null, output: string | null) => Promise<void>;
  set: (name: string, value: Value) => void;
  /** Several values at once (a preset): one request. */
  setMany: (values: Values) => Promise<void>;
  reset: () => Promise<void>;
  /** The daemon reports `output`'s values. */
  sync: (output: string, values: Values) => void;
  /** Values that differ from the defaults. */
  changed: () => Values;
}

const inflight = new Set<string>();
const queued = new Map<string, Value>();
let generation = 0;

export const useProps = create<PropsState>()((set, get) => {
  const send = (name: string, value: Value) => {
    const output = get().output;
    if (!output) return;
    inflight.add(name);
    api
      .daemon({ cmd: "property", output, name, value })
      .catch((e) => console.warn(`setting ${name}: ${e}`))
      .finally(() => {
        inflight.delete(name);
        if (queued.has(name)) {
          const next = queued.get(name)!;
          queued.delete(name);
          send(name, next);
        }
      });
  };

  const evaluate = () => {
    const { props, values } = get();
    const conditions: Record<string, string> = {};
    for (const p of props) if (p.condition) conditions[p.name] = p.condition;
    if (!Object.keys(conditions).length) return;
    const gen = generation;
    api.conditions(conditions, values).then((visible) => {
      if (gen === generation) set({ visible });
    });
  };

  return {
    itemId: null,
    output: null,
    props: [],
    values: {},
    visible: {},
    loading: false,

    load: async (item, output) => {
      const gen = ++generation;
      inflight.clear();
      queued.clear();
      if (!item) {
        set({ itemId: null, output: null, props: [], values: {}, visible: {}, loading: false });
        return;
      }
      set({ itemId: item.id, output, loading: true });
      try {
        const r = await api.properties(item.id, output ?? undefined);
        if (gen !== generation) return;
        const values: Values = {};
        for (const p of r.properties) values[p.name] = p.value;
        set({ props: r.properties, values, visible: {}, loading: false, output: r.output });
        evaluate();
      } catch (e) {
        if (gen === generation) set({ props: [], values: {}, loading: false });
        console.warn(`properties: ${e}`);
      }
    },

    set: (name, value) => {
      set((s) => ({ values: { ...s.values, [name]: value } }));
      evaluate();
      if (!get().output) return;
      if (inflight.has(name)) queued.set(name, value);
      else send(name, value);
    },

    setMany: async (values) => {
      set((s) => ({ values: { ...s.values, ...values } }));
      evaluate();
      const output = get().output;
      if (output) await api.daemon({ cmd: "set-properties", output, values });
    },

    reset: async () => {
      const defaults: Values = {};
      for (const p of get().props) defaults[p.name] = p.default;
      set({ values: defaults });
      evaluate();
      const output = get().output;
      if (output) await api.daemon({ cmd: "reset-properties", output });
    },

    sync: (output, values) => {
      if (output !== get().output) return;
      const next = { ...get().values };
      for (const [k, v] of Object.entries(values)) if (!inflight.has(k)) next[k] = v;
      set({ values: next });
      evaluate();
    },

    changed: () => {
      const { props, values } = get();
      const out: Values = {};
      for (const p of props) if (p.name in values && values[p.name] !== p.default) out[p.name] = values[p.name];
      return out;
    },
  };
});

/** A value dragged on a slider, sent at most one request at a time. */
export class Throttled {
  private inflight = false;
  private queued: number | null = null;
  constructor(private sendValue: (v: number) => Promise<unknown>) {}
  set(v: number) {
    if (this.inflight) {
      this.queued = v;
      return;
    }
    this.inflight = true;
    this.sendValue(v).finally(() => {
      this.inflight = false;
      if (this.queued !== null) {
        const next = this.queued;
        this.queued = null;
        this.set(next);
      }
    });
  }
}
