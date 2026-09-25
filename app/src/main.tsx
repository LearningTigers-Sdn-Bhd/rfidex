import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";

// Task 8 replaces this with the typed boundary in `api.ts` and the real
// screens. Here it only proves the IPC seam and the first-run state.
interface AppView {
  configured: boolean;
  status: { stations: { name: string }[] } | null;
}

function FirstMount() {
  const [view, setView] = useState<AppView | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    invoke<AppView>("app_state")
      .then((next) => live && setView(next))
      .catch((failure) => live && setError(errorText(failure)));
    return () => {
      live = false;
    };
  }, []);

  if (error) return <p role="alert">{error}</p>;
  if (!view) return <p>Starting…</p>;
  return (
    <p>
      {view.configured
        ? `Running ${view.status?.stations.length ?? 0} stations.`
        : "Setup is needed before this station can be used."}
    </p>
  );
}

function errorText(failure: unknown): string {
  if (
    typeof failure === "object" &&
    failure !== null &&
    "message" in failure &&
    typeof (failure as { message: unknown }).message === "string"
  ) {
    return (failure as { message: string }).message;
  }
  return "The app could not finish this action.";
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <FirstMount />
  </StrictMode>,
);
