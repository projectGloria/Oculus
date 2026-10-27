import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App.jsx";
import "./styles/theme.css";

// The native context menu (Back/Reload/Inspect) makes a webview feel like a web page.
if (!import.meta.env.DEV) window.addEventListener("contextmenu", (e) => e.preventDefault());

createRoot(document.getElementById("root")).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
