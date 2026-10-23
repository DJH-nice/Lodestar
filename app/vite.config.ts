// 【为什么从 "vitest/config" 而不是 "vite" 导入 defineConfig】
//   两者是同一个函数（vitest/config 把 vite 的 defineConfig 原样再导出），
//   区别只在**类型**：vitest 的那份多认一个 `test` 字段。
//   而 `vite build` 也读这个文件 —— 从 "vite" 导入时写 `test:` 会报
//   「对象字面量里有多余的键 test」。从 "vitest/config" 导入两边都认。
import { defineConfig } from "vitest/config";
import vue from "@vitejs/plugin-vue";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [vue()],

  test: {
    // 【现在有前端测试了，所以不再需要 passWithNoTests】
    //   commit 80eecb8「只留核心代码」把前端 15 个 *.test.ts **全删了**，
    //   vitest 的默认行为是「一个测试文件都没找到 → 退出码 1」——
    //   于是当时加了 `passWithNoTests: true` 让「没有测试」也不算失败。
    //   现在仓库里**已经有**前端测试（见 src/lib/shortcut.test.ts），
    //   → 那行删掉，vitest 回到「找不到测试就报错」的严格默认。
    //   （`vitest/config` 的 import 仍要留着 —— 它是 `test` 字段的来源。）
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
    fs: {
      // 4. 允许读【app/ 目录】与【仓库根的 docs/】。
      //
      //    使用手册在仓库根的 docs/USER-GUIDE.md —— 它在 app/ 的**外面**，
      //    前端用 `?raw` 把它整份内联进 bundle（见 src/lib/user-guide.ts）。
      //    Vite 默认只允许读「工作区根」以内的文件，而这里的工作区根是 app/
      //    （app/pnpm-lock.yaml）—— 于是手册会被判成 "Denied ID"，
      //    开发服务器与 vitest 都读不到它。
      //
      //    【为什么是 [".", "../docs"] 而不是 [".."]]
      //      `fs.allow` 是**整体替换**默认列表（默认是 [工作区根]），
      //      所以只写 `["../docs/USER-GUIDE.md"]` 会把 app/ 自己挡在门外 ——
      //      实测那样 vitest 会挂掉 2 个测试文件（它读不到 app/src/…）。
      //      写成 [".", "../docs"] 才是「放行 app/ 与 docs/ 这两块」，
      //      而 [".."] 会**连上游 src/、shim/、.git/、.superpowers/ 一起放行**。
      //      这里只放行真正需要的那两块，别的一律不给。
      allow: [".", "../docs"],
    },
  },
}));
