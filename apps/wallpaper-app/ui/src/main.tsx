import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "./styles.css";
import { render } from "preact";
import { App } from "./App";
import { start } from "./store";

import { setMockPictures } from "./api";

async function main() {
  // Opened in a plain browser: a made-up backend (mock.ts).
  if (!("__TAURI_INTERNALS__" in window)) {
    const mock = await import("./mock");
    mock.installMock();
    setMockPictures(mock.mockPicture);
  }
  render(<App />, document.getElementById("app")!);
  start();
}

main();
