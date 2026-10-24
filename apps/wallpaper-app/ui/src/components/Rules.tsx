// The rules editor (Settings → Performance): when another app is
// fullscreen, sound plays, the laptop is on battery... the wallpaper is
// paused, muted, stopped, slowed down or thinned out.
import { Plus, Trash2 } from "lucide-preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import { api, type Condition, type Playback, type Rule, type Signals } from "../api";
import { request, useApp } from "../store";
import { Toggle } from "./ui";

type Kind = "fullscreen" | "maximized" | "focused" | "open" | "audio" | "battery" | "battery-low" | "idle";

const KINDS: [Kind, string][] = [
  ["fullscreen", "Another app is fullscreen"],
  ["maximized", "Another app is maximized"],
  ["focused", "Another app is focused"],
  ["open", "An app is running"],
  ["audio", "Another app plays sound"],
  ["battery", "On battery"],
  ["battery-low", "Battery is low"],
  ["idle", "No input for a while"],
];

const PLAYBACK: Record<Playback, string> = { run: "Keep running", mute: "Mute", pause: "Pause", stop: "Stop" };
const FPS = [60, 30, 24, 15, 10, 5];
const PARTICLES = [75, 50, 25, 0];

function kindOf(c: Condition): Kind {
  switch (c.if) {
    case "window":
      return c.state;
    case "audio":
      return "audio";
    case "battery":
      return c.below === undefined ? "battery" : "battery-low";
    case "idle":
      return "idle";
  }
}

/** A condition of `kind`, keeping what applies of `old`. */
function condition(kind: Kind, old: Condition): Condition {
  const app = "app" in old ? old.app : undefined;
  switch (kind) {
    case "audio":
      return { if: "audio", app };
    case "battery":
      return { if: "battery" };
    case "battery-low":
      return { if: "battery", below: 20 };
    case "idle":
      return { if: "idle", seconds: 600 };
    default:
      return { if: "window", state: kind, app };
  }
}

/** Names of the apps the daemon sees now, for the app field. */
function appNames(s: Signals | null): string[] {
  const names = new Set<string>();
  for (const w of s?.windows ?? []) {
    if (w.app) names.add(w.app);
    if (w.process) names.add(w.process);
  }
  for (const a of s?.audio ?? []) if (a) names.add(a);
  return [...names].sort();
}

function useSignals(): Signals | null {
  const [signals, setSignals] = useState<Signals | null>(null);
  useEffect(() => {
    const poll = () => api.daemon({ cmd: "signals" }).then((r) => setSignals(r.signals ?? null), () => {});
    poll();
    const timer = window.setInterval(poll, 3000);
    return () => clearInterval(timer);
  }, []);
  return signals;
}

export function Rules({ rules }: { rules: Rule[] }) {
  const [draft, setDraft] = useState(rules);
  const pending = useRef<number | undefined>(undefined);
  // The daemon's rules, unless an edit is on its way.
  const key = JSON.stringify(rules);
  useEffect(() => {
    if (pending.current === undefined) setDraft(rules);
  }, [key]);
  const save = (next: Rule[], wait = 0) => {
    setDraft(next);
    clearTimeout(pending.current);
    pending.current = window.setTimeout(() => {
      pending.current = undefined;
      request({ cmd: "settings", rules: next });
    }, wait);
  };
  const change = (i: number, rule: Rule, wait = 0) => save(draft.map((r, j) => (j === i ? rule : r)), wait);

  const signals = useSignals();
  const apps = useMemo(() => appNames(signals), [signals]);
  const outputs = useApp((s) => s.status?.outputs ?? []);
  const held = outputs.filter((o) => o.limits?.because?.length);

  return (
    <div class="setting-col">
      <div>Rules</div>
      <div class="hint">What happens to wallpapers while something else goes on. Of several rules holding, the strongest wins.</div>
      {held.map((o) => (
        <div class="rule-now">
          <b>{o.name}</b> {PLAYBACK[o.limits!.playback ?? "run"].toLowerCase()}
          {o.limits!.fps !== undefined && `, ${o.limits!.fps} fps`}
          {o.limits!.particles !== undefined && `, ${o.limits!.particles}% particles`}
          <span class="muted"> · {o.limits!.because!.join(", ")}</span>
        </div>
      ))}
      {signals && signals.windows === undefined && (
        <div class="hint">This compositor doesn't tell about windows, so window rules never hold.</div>
      )}
      <datalist id="rule-apps">
        {apps.map((a) => (
          <option value={a} />
        ))}
      </datalist>
      <div class="rules">
        {draft.map((rule, i) => (
          <RuleRow rule={rule} onChange={(r, wait) => change(i, r, wait)} onDelete={() => save(draft.filter((_, j) => j !== i))} />
        ))}
      </div>
      <div>
        <button
          class="btn tall"
          onClick={() => save([...draft, { when: { if: "window", state: "maximized" }, playback: "pause" }])}
        >
          <Plus size={14} /> Add rule
        </button>
      </div>
    </div>
  );
}

function RuleRow({ rule, onChange, onDelete }: {
  rule: Rule;
  onChange: (rule: Rule, wait?: number) => void;
  onDelete: () => void;
}) {
  const c = rule.when;
  const kind = kindOf(c);
  const windowRule = c.if === "window" && c.state !== "open";
  const set = (patch: Partial<Rule>, wait?: number) => onChange({ ...rule, ...patch }, wait);
  const number = (e: Event) => Math.max(0, Math.round(+(e.target as HTMLInputElement).value) || 0);
  return (
    <div class={"rule" + (rule.enabled === false ? " off" : "")}>
      <div class="line">
        <Toggle on={rule.enabled !== false} onChange={(on) => set({ enabled: on })} />
        <select class="field" value={kind} onChange={(e) => set({ when: condition((e.target as HTMLSelectElement).value as Kind, c) })}>
          {KINDS.map(([k, label]) => (
            <option value={k}>{label}</option>
          ))}
        </select>
        {(c.if === "window" || c.if === "audio") && (
          <input
            class="field app"
            list="rule-apps"
            placeholder={c.if === "window" && c.state === "open" ? "which app?" : "any app"}
            value={c.app ?? ""}
            onInput={(e) => set({ when: { ...c, app: (e.target as HTMLInputElement).value || undefined } }, 600)}
          />
        )}
        {c.if === "battery" && c.below !== undefined && (
          <label class="unit">
            below
            <input class="field num" type="number" min={1} max={100} value={c.below} onInput={(e) => set({ when: { ...c, below: number(e) } }, 600)} />%
          </label>
        )}
        {c.if === "idle" && (
          <label class="unit">
            <input
              class="field num"
              type="number"
              min={1}
              value={Math.round(c.seconds / 60)}
              onInput={(e) => set({ when: { ...c, seconds: Math.max(1, number(e)) * 60 } }, 600)}
            />
            min
          </label>
        )}
        <button class="icon-btn delete" title="Remove the rule" onClick={onDelete}>
          <Trash2 size={15} />
        </button>
      </div>
      <div class="line then">
        <select
          class="field"
          value={rule.playback ?? "run"}
          onChange={(e) => {
            const p = (e.target as HTMLSelectElement).value as Playback;
            set({ playback: p === "run" ? undefined : p });
          }}
        >
          {(Object.keys(PLAYBACK) as Playback[]).map((p) => (
            <option value={p}>{p === "stop" ? "Stop (free memory)" : PLAYBACK[p]}</option>
          ))}
        </select>
        {(rule.playback ?? "run") !== "pause" && (rule.playback ?? "run") !== "stop" && (
          <>
            <select class="field" value={rule.fps ?? ""} onChange={(e) => set({ fps: +(e.target as HTMLSelectElement).value || undefined })}>
              <option value="">Any frame rate</option>
              {FPS.map((f) => (
                <option value={f}>{f} fps at most</option>
              ))}
            </select>
            <select
              class="field"
              value={rule.particles ?? ""}
              onChange={(e) => {
                const v = (e.target as HTMLSelectElement).value;
                set({ particles: v === "" ? undefined : +v });
              }}
            >
              <option value="">All particles</option>
              {PARTICLES.map((p) => (
                <option value={p}>{p === 0 ? "No particles" : `${p}% of particles`}</option>
              ))}
            </select>
          </>
        )}
        {windowRule && (
          <label class="unit every">
            <input type="checkbox" checked={!!rule.all_outputs} onChange={(e) => set({ all_outputs: (e.target as HTMLInputElement).checked || undefined })} />
            on every display
          </label>
        )}
      </div>
    </div>
  );
}
