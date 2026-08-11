import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/app.tsx";
import "@/index.css";

const root = document.getElementById("root");

if (!root) throw new Error("缺少 #root 挂载节点");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
