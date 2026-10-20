/// <reference types="vitest" />
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// 独立于 vite.config.ts：生产构建的端口/插件设置不该被测试环境污染。
// 放在一起会让 vitest 去抢 1420 端口，而测试根本不需要 dev server。
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/__tests__/setup.ts"],
    include: ["src/__tests__/**/*.test.ts", "src/__tests__/**/*.test.tsx"],
    // 兜底：绝不允许静默通过
    passWithNoTests: false,
    reporters: ["default"],
  },
});
