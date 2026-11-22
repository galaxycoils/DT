import { defineConfig } from "vite";

const target = process.env.DT_API ?? "http://127.0.0.1:7433";
const proxy = Object.fromEntries(
  ["/api", "/ag-ui", "/a2a", "/bridges", "/.well-known"].map((p) => [p, { target, changeOrigin: false }]),
);

export default defineConfig(({ command }) => ({
  base: command === "build" ? "/app/" : "/",
  server: { host: "localhost", port: 5173, strictPort: true, proxy },
}));
