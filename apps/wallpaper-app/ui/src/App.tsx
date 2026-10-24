// A placeholder until the Penguin Engine design is implemented.
import { thumbUrl } from "./api";
import { useApp } from "./store";

export function App() {
  const { items, loading, status, daemonDown, apply } = useApp();
  return (
    <div style={{ padding: 16, fontFamily: "system-ui", color: "#e3e5e8", background: "#16181d", minHeight: "100vh" }}>
      <h1>Penguin Engine</h1>
      <p>
        {daemonDown ??
          status?.outputs.map((o) => `${o.name}: ${o.wallpaper?.title ?? "—"}`).join(" · ") ??
          "connecting…"}
      </p>
      <p>{loading ? "Loading…" : `${items.length} wallpapers`}</p>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
        {items.slice(0, 24).map((item) => (
          <img
            key={item.id}
            src={thumbUrl(item.id)}
            title={item.title}
            width={200}
            height={112}
            style={{ objectFit: "cover", cursor: "pointer" }}
            onDblClick={() => apply(item)}
          />
        ))}
      </div>
    </div>
  );
}
