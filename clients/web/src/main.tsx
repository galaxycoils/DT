import React from "react";
import { createRoot } from "react-dom/client";
import "./tokens.css";
import { MuseV1Client } from "./api";
import { App } from "./App";

const token = window.localStorage.getItem("grok_muse_token") ?? "";
const client = new MuseV1Client({ token });

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App client={client} />
  </React.StrictMode>,
);
