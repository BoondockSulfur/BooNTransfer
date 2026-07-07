import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Detached from "./Detached";
import "./styles.css";

// A detached window loads the same bundle with ?detached=<view>.
const params = new URLSearchParams(window.location.search);
const detached = params.get("detached");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {detached ? <Detached view={detached} params={params} /> : <App />}
  </React.StrictMode>,
);
