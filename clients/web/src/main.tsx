import React from "react";
import { createRoot } from "react-dom/client";
import "./tokens.css";
import { DtV1Client } from "./api";
import { App } from "./App";

const client = new DtV1Client();

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App client={client} />
  </React.StrictMode>,
);
