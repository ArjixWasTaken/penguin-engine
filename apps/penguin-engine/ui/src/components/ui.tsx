// Small controls shared by the screens.
import type { ComponentChildren } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";

export function Toggle({ on, onChange, disabled }: { on: boolean; onChange: (on: boolean) => void; disabled?: boolean }) {
  return (
    <button type="button" class={"toggle" + (on ? " on" : "")} disabled={disabled} onClick={() => onChange(!on)} aria-pressed={on}>
      <span />
    </button>
  );
}

export function Segments<T extends string>({ value, options, onChange, labels }: {
  value: T;
  options: readonly T[];
  onChange: (v: T) => void;
  labels?: Partial<Record<T, string>>;
}) {
  return (
    <div class="segments">
      {options.map((o) => (
        <button type="button" key={o} class={o === value ? "on" : ""} onClick={() => onChange(o)}>
          {labels?.[o] ?? o}
        </button>
      ))}
    </div>
  );
}

export function SectionHead({ children }: { children: ComponentChildren }) {
  return (
    <div class="section-head">
      <span class="caps">{children}</span>
      <div class="line" />
    </div>
  );
}

/** A button that opens a menu; the menu closes on a click elsewhere. */
export function MenuButton({ label, class: cls, menu, up, disabled }: {
  label: ComponentChildren;
  class?: string;
  menu: (close: () => void) => ComponentChildren;
  up?: boolean;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [open]);
  return (
    <div class="menu-anchor" ref={ref}>
      <button type="button" class={cls} disabled={disabled} onClick={() => setOpen(!open)}>
        {label}
      </button>
      {open && <div class={"menu " + (up ? "up" : "down")}>{menu(() => setOpen(false))}</div>}
    </div>
  );
}
