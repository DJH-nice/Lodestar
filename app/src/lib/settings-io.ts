/** 设置的读写 —— 【串行化的读-改-写】。
 *
 *  【为什么【前端的】写入都要走 patchSettings】`set_settings` 是**整体替换**：
 *  一次写入必须带着**完整的**设置对象，否则没带上的字段会被清空
 *  （最严重的是把**索引目录**设置写没 —— 引擎还在跑，但设置里目录空了，
 *  下次重启就悄悄改成索引家目录了）。
 *
 *  （Rust 侧在【关窗】时也会写一次设置 —— 存窗口位置。
 *    那是一次独立的读-改-写，不走这条队列：
 *    它只发生在关窗那一刻，而前端的写入都在运行中，
 *    两边撞上的窗口极小。）
 *
 *  【为什么写入前要【重新读】一遍】调用方手里那份 settings 可能是**旧的**：
 *  设置面板改了索引目录之后，书签面板里那份拷贝就没索引目录了 ——
 *  拿它写回去就是把索引目录抹掉。
 *  所以每次写之前都重新读盘上的真值，在它上面改一个字段，再**整体**写回。
 *
 *  【为什么要【串行化】】读-改-写不是原子的：
 *  两个写入同时进来，都读到同一份旧值，各自改一个字段，
 *  后写的那个会把先写的那个覆盖掉（丢书签 / 丢最近搜索）。
 *  一条 Promise 链把写入排成队 —— 后一个一定看得到前一个的结果。
 *
 *  【为什么最后一步是 Rust 的 `patch_settings` 而不是「整体写回」】（终审 M-10）
 *  前端的「读 → 算差量 → 写」是**两次 IPC**：
 *  读完之后、写之前，Rust 侧可能自己写了一次设置
 *  （改快捷键 / 改开机自启 / 关窗时存窗口位置）→
 *  前端那份**已经旧了**的整体对象一写回去，
 *  Rust 那边的改动就被**抹掉**了（典型症状：刚改完快捷键，
 *  自动刷新开关的改动没了）。
 *  → 把差量交给 Rust：`patch_settings` 在**进程内的写锁**里
 *    「读盘上的真值 → 合并差量 → 写回」，
 *    两个写者就真正串行了（没提到的字段一律保留）。
 */

import { invoke } from "@tauri-apps/api/core";
import type { Settings } from "./types";

/** 读全部设置。 */
export function loadSettings(): Promise<Settings> {
  return invoke<Settings>("get_settings");
}

/** 写入队列 —— 同一时刻只允许一个读-改-写在飞。 */
let queue: Promise<unknown> = Promise.resolve();

/** 改设置：拿当前（盘上的真值）算一个差量。
 *
 *  返回 `null` = 这次【没有要改的】→ 不写盘（省一次 IO，也避免无谓的覆盖）。 */
export type SettingsPatch = (current: Settings) => Partial<Settings> | null;

/** 在**完整的**设置对象上改一处，然后写回。返回写回后的对象。
 *
 *  写入是串行的 —— 并发调用不会互相覆盖。
 *  最后一步走 Rust 的 `patch_settings`（锁内合并）——
 *  这样 Rust 侧自己的写入不会被前端这份**可能已旧**的数据覆盖掉。
 *  失败会 reject 给调用方（调用方要负责把错误告诉用户）。 */
export function patchSettings(patch: SettingsPatch): Promise<Settings> {
  const run = queue.then(async () => {
    // 【重新读】—— 不能用调用方手里那份（可能是旧的，见文件头的说明）
    const current = await loadSettings();
    const delta = patch(current);
    // 没有变化 → 不写
    if (delta === null) return current;
    // 【差量交给 Rust 合并】—— 它在锁内读**盘上的真值**再写，
    // 不会把我们这份可能已旧的数据整个盖回去。
    return invoke<Settings>("patch_settings", { delta });
  });
  /* 队列本身不能被一次失败打断 ——
     否则后面排队的写入全都卡住，设置再也写不进去了。
     （run 本身仍然会把错误抛给调用方 —— 这里只是不让队列断链。） */
  queue = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}
