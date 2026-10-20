import React from "react";
import ReactDOM from "react-dom/client";
import App, { RenderBoundary } from "./App";
import "./styles.css";

// 最外层兜底：App 内部任何一处渲染抛错，都降级成一块可见的错误卡片，
// 而不是让 React 卸载整棵树、留下一个黑窗口（BUG 1 的失败模式）。
// 详情面板自身还有一层更窄的边界，见 App.tsx 的 <RenderBoundary label="详情面板">。
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <RenderBoundary label="主界面">
      <App />
    </RenderBoundary>
  </React.StrictMode>,
);
