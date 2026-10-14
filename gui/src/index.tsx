import { render } from "solid-js/web";
import "./styles.css";
import App from "./App";
import { installEscapeHandling } from "./lib/closeOnEscape";

installEscapeHandling();
render(() => <App />, document.getElementById("root")!);
