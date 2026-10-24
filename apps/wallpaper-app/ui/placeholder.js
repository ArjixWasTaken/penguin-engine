// A placeholder page over the backend (see API.md), until the real
// design replaces it.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let items = [];
let selected = null;
let status = null;

const grid = document.getElementById("grid");
const detail = document.getElementById("detail");
const daemon = document.getElementById("daemon");

function render(filter = "") {
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
  grid.replaceChildren(
    ...items
      .filter((i) => words.every((w) => i.title.toLowerCase().includes(w)))
      .map((item) => {
        const tile = document.createElement("div");
        tile.className = "tile" + (item.id === selected ? " selected" : "");
        tile.innerHTML = `<img loading="lazy" alt=""><span></span>`;
        tile.querySelector("img").src = `thumb://localhost/${item.id}`;
        tile.querySelector("span").textContent = item.title;
        tile.onclick = () => select(item);
        tile.ondblclick = () => apply(item);
        return tile;
      }),
  );
}

function select(item) {
  selected = item.id;
  for (const t of grid.children) t.classList.remove("selected");
  render(document.getElementById("search").value);
  detail.innerHTML = `<img alt=""><h2></h2><p></p><button>Apply</button>`;
  detail.querySelector("img").src = `preview://localhost/${item.id}`;
  detail.querySelector("h2").textContent = item.title;
  detail.querySelector("p").textContent = `${item.kind} · ${item.rating}`;
  detail.querySelector("button").onclick = () => apply(item);
}

async function apply(item) {
  try {
    await invoke("daemon", { request: { cmd: "set", wallpaper: item.path } });
  } catch (e) {
    alert(e);
  }
}

function showStatus() {
  if (!status) return;
  const shown = status.outputs
    .map((o) => `${o.name}: ${o.wallpaper ? o.wallpaper.title : "—"}`)
    .join(" · ");
  daemon.textContent = shown || "no outputs";
}

document.getElementById("search").oninput = (e) => render(e.target.value);

listen("daemon-event", ({ payload }) => {
  if (payload.event === "status") {
    status = payload.status;
    showStatus();
  }
});
listen("daemon-down", ({ payload }) => {
  status = null;
  daemon.textContent = payload;
});

invoke("library").then((list) => {
  items = list;
  render();
});
