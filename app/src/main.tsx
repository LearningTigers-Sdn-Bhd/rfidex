import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { installPreviewBridge } from "./preview";
import "./style.css";

// Styling preview: ?preview=1 on the dev server poses as the Tauri bridge so
// the real screens render in a plain browser. import.meta.env.DEV is a
// compile-time constant, so this module drops out of production builds.
if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("preview")) {
  installPreviewBridge();
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
