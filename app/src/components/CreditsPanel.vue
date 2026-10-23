<script setup lang="ts">
import { onMounted, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";

/**
 * **致谢与许可** —— 本软件的法务信息。
 *
 *  【为什么要有这个面板】GPL-2.0 §5(a)/(d) 要求：
 *    - 醒目标注你**修改过**原作品，并说明它按该许可发布；
 *    - 分发时附带**完整的对应源码**。
 *   写在仓库 README 里**不够** —— 软件装进 `/Applications` 之后，
 *   绝大多数用户不会再去翻仓库 → 这句话必须**在应用里也能看见**。
 *
 *  【为什么不叫「关于」】macOS 的应用菜单里已经有系统的「关于 Sextant」
 *   （显示版本号与版权）。再放一个同名项会让人以为是同一个东西。
 *   → 这里叫「致谢与许可」，指向的是**法务信息**（引擎出处 + 许可证）。
 *
 *  【内容为什么写死在这里】这些信息**不随运行状态变化** ——
 *   与其每次去查 Tauri 的版本号、构建元数据，不如写成静态事实。
 *   只有版本号是运行时取的（见下方 version）。
 */
const emit = defineEmits<{ (e: "close"): void }>();

/** 版本号 —— 从 Tauri 的 bundle 元数据里取。
 *
 *  【为什么要 fallback】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）
 *   时 `getVersion()` 会抛 → 用一个占位串，**不让面板开不起来**。 */
const version = ref("—");
onMounted(async () => {
  try {
    version.value = await getVersion();
  } catch {
    /* 不在 Tauri 环境里 —— 保持占位符。 */
  }
});
</script>

<template>
  <div class="panel-drawer-backdrop" @click.self="emit('close')">
    <section class="panel-drawer" @click.stop>
      <header class="panel-head">
        <h2>致谢与许可</h2>
        <button class="panel-close" @click="emit('close')">×</button>
      </header>

      <div class="panel-body credits-body">
        <h3>Sextant</h3>
        <p class="credits-sub">
          版本 {{ version }} · macOS 上的文件搜索器
        </p>

        <h3>引擎出处</h3>
        <p>
          本软件使用并修改了
          <strong>FSearch 0.3.2</strong> 的搜索引擎代码。<br />
          <span class="credits-mono">Copyright © 2026 Christian Boxdörfer</span><br />
          FSearch 按
          <strong>GNU GPL v2 或更高版本</strong>发布
          （<a href="https://github.com/cboxdoerfer/fsearch" target="_blank"
            rel="noreferrer">github.com/cboxdoerfer/fsearch</a
          >）。
        </p>

        <h3>我们做的修改</h3>
        <p>
          按 GPL-2.0 §5(a) 的要求，在此声明我们对 FSearch 所做的修改：
        </p>
        <ul>
          <li>
            新增 <strong>shim/</strong> 层 —— 把上游引擎包成 13 个 C 函数
            （FFI 契约见 <code>shim/fsearch_shim.h</code>），供 Rust 侧调用。
          </li>
          <li>
            平台适配 —— macOS / Apple Silicon 的编译参数、动态库打包方式，
            以及 macOS 专属的索引排除规则（<code>*/Library</code>、
            <code>*/Applications</code>、<code>*/target</code>、
            <code>*/node_modules</code>、隐藏文件等）。
          </li>
          <li>
            新增图形界面 —— Tauri v2 + Vue 3（与上游的 GTK 界面相互独立）。
          </li>
        </ul>
        <p class="credits-note">
          上游源码目录
          <code>src/</code>
          <strong>未作任何修改</strong>，与原始发布包逐字节一致 ——
          可用仓库里的 <code>shim/check-upstream-pure.sh</code> 自证。
        </p>

        <h3>许可证</h3>
        <p>
          本软件整体按
          <strong>GNU GPL v2 或更高版本</strong>发布。
          按该许可的要求，分发本软件时必须一并提供<strong
            >完整对应源码</strong
          >、保留上述版权声明，并保留本节所述的修改说明。
        </p>
        <p class="credits-note">
          许可证全文、第三方组件声明，以及<strong>完整的对应源码压缩包</strong>
          都装在本应用内：
          <code>Sextant.app/Contents/Resources/licenses/</code>
          （在访达里右键本应用 →「显示包内容」即可看到）。
        </p>

        <h3>其它组件</h3>
        <ul>
          <li>
            GLib、Pango —— LGPL-2.1
          </li>
          <li>HarfBuzz —— 旧版 MIT 许可</li>
          <li>ICU —— Unicode 许可</li>
          <li>
            搜索语法帮助文字改编自 FSearch 官方文档 —— CC BY-SA 4.0
          </li>
        </ul>
        <p class="credits-note">
          各第三方组件的许可清单见
          <code>THIRD-PARTY-NOTICES.md</code>，与应用一起打包。
        </p>
      </div>
    </section>
  </div>
</template>
