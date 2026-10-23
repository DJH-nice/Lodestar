import { marked } from "marked";
// 【路径】这个文件在 app/src/lib/ 下，仓库根的 docs/ 要往上三层：
//   ..  → app/src
//   ../..  → app
//   ../../.. → 仓库根
import raw from "../../../docs/USER-GUIDE.md?raw";

/** 手册的**原始 Markdown** —— 仓库里 `docs/USER-GUIDE.md` 的内容，构建期内联。
 *
 *  【为什么是构建期内联，而不是运行时读文件】
 *   1. 前端**没有文件系统的能力** —— 这个项目刻意不装 `@tauri-apps/plugin-fs`
 *      （装了就是给 webview 开文件系统权限，是权限面的扩大）。
 *      走 Rust 读文件要新增一条 IPC 命令 + 一条 capability，代价大、收益小。
 *   2. 打包出来的 `.app` 里**根本不带 `docs/`** —— 运行时读不到。
 *   3. 运行时读会造出「两个真相」：仓库里的 .md 与用户机器上的 .md。
 *
 *  `?raw` 让 Vite 在打包时把内容原样内联进 bundle ——
 *  **运行时零文件访问、零权限、零 IO**。 */
export const userGuideMarkdown: string = raw;

/** 手册的 HTML —— 把上面的 Markdown 渲染成 HTML，交给 `v-html`。
 *
 *  【为什么可以放心 v-html】
 *  内容是**我们自己仓库里的文件**、构建期就内联进 bundle 了，**不是用户输入**
 *  → 没有 XSS 面。
 *  （真要防的话，把 marked 的 raw HTML 透传关掉即可 —— 但手册里没有需要透传的 HTML。）
 *
 *  【为什么用 marked】
 *  手册是 Markdown，应用内要**渲染**它（表格、代码块、列表）。
 *  `marked` 是**纯字符串转换**（MIT、无任何系统能力），
 *  不增加任何能力面 —— 与 `@tauri-apps/plugin-fs` 那种「开权限」不是一回事。
 *
 *  【async: false】明确要同步结果 ——
 *  否则 `marked.parse` 的类型是 `string | Promise<string>`，
 *  塞给 `v-html` 会有类型问题。 */
export const userGuideHtml: string = marked.parse(userGuideMarkdown, {
  gfm: true,
  breaks: false,
  async: false,
});
