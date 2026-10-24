import { render } from "preact";
import { App } from "./App";
import { start } from "./store";

render(<App />, document.getElementById("app")!);
start();
