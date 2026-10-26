import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./ui/primitives";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {/* The last resort, for a throw above the view boundary — a store selector or
        the chrome. A reload is the only recovery that does not depend on the
        broken tree, and everything durable is on disk or in the backend, which
        keeps running either way. */}
    <ErrorBoundary onBack={() => window.location.reload()} backLabel="Reload the window">
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
