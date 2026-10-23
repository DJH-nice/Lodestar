#!/bin/bash
# 空闲 CPU 测量 —— 「这 N 秒里，这个进程烧掉了多少 CPU」。
#
# 【为什么不用 top 的 %CPU】
#   top 的那个数字是**开机以来的平均值**，
#   会把「刚才那 0.5 秒的补扫」稀释掉，看不出空闲时的真实开销。
#   这里用 `ps -o time=` 的**差分**（CPU 时间增量 / 墙钟增量）——
#   测的就是「这一段时间里烧了多少」。
#
# 用法：bash app/scripts/idle-cpu.sh <pid> [秒数]
#       bash app/scripts/idle-cpu.sh <pid1> <pid2> ... [秒数]
#
# 多个 PID 时：逐个报 + 报合计（Tauri 应用是「主进程 + 若干 WebKit 辅助进程」，
# 只量主进程会漏掉 webview 那部分的 CPU）。

set -euo pipefail

if [ $# -lt 1 ]; then
  echo "用法: idle-cpu.sh <pid> [秒数]" >&2
  exit 1
fi

# 【最后一个参数是秒数还是 PID？】
#   秒数是纯数字、且不长得像 PID —— 用「是不是纯数字」判断即可
#   （PID 也是纯数字，所以先按「最后一个参数是纯数字且 >= 1」当秒数处理，
#     但如果那个数在 ps 里查得到，它就是 PID —— 这种歧义本来就该靠人避开）。
SECS=""
PIDS=()
for a in "$@"; do
  PIDS+=("$a")
done
LAST="${PIDS[${#PIDS[@]}-1]}"
if [[ "$LAST" =~ ^[0-9]+$ ]] && ! ps -p "$LAST" > /dev/null 2>&1; then
  SECS="$LAST"
  PIDS=("${PIDS[@]:0:${#PIDS[@]}-1}")
fi
if [ -z "$SECS" ]; then
  SECS=15
fi

if [ ${#PIDS[@]} -eq 0 ]; then
  echo "没有 PID" >&2
  exit 1
fi

for p in "${PIDS[@]}"; do
  if ! ps -p "$p" > /dev/null 2>&1; then
    echo "进程 $p 不存在" >&2
    exit 1
  fi
done

# 【为什么用 python 解析 `ps -o time=`】
#   macOS 的 `ps -o time=` 给的是 "0:03.71" / "1:02.50" 这种 mm:ss.ss 格式，
#   而且**秒数部分可能带小数**。用 shell 切字段容易切错（00:00:00 那套是别的平台）。
to_seconds() {
  python3 -c "
import sys
p = sys.argv[1].split(':')
s = 0.0
for x in p:
    s = s * 60 + float(x)
print(s)
" "$1"
}

# 采样前先把每个 PID 的 CPU 时间记下来
declare -a T0=()
for p in "${PIDS[@]}"; do
  T0+=("$(to_seconds "$(ps -o time= -p "$p" | tr -d ' ')")")
done
W0=$(date +%s)

echo "采样 $SECS 秒（PID=${PIDS[*]}）…"
sleep "$SECS"

declare -a T1=()
for p in "${PIDS[@]}"; do
  T1+=("$(to_seconds "$(ps -o time= -p "$p" | tr -d ' ')")")
done
W1=$(date +%s)

# 算差分 + 打印
python3 - "$W0" "$W1" "${PIDS[*]}" "${T0[@]}" "${T1[@]}" <<'PY'
import sys

w0, w1 = int(sys.argv[1]), int(sys.argv[2])
pids = sys.argv[3].split()
t0 = [float(x) for x in sys.argv[4:4 + len(pids)]]
t1 = [float(x) for x in sys.argv[4 + len(pids):4 + 2 * len(pids)]]

w = max(1, w1 - w0)
total = 0.0
for pid, a, b in zip(pids, t0, t1):
    d = b - a
    total += d
    print(f"  PID {pid}: CPU 时间增量 {d:.2f}s / 墙钟 {w}s  =>  {d / w * 100:.2f}%")
print(f"合计 CPU 时间增量 {total:.2f}s / 墙钟 {w}s  =>  平均 CPU 占用 {total / w * 100:.2f}%")
PY
