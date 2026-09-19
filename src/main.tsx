import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

// 渲染前先落定皮肤，避免首帧闪成另一种风格
document.documentElement.dataset.style = localStorage.getItem("conflux.style") ?? "ios";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
