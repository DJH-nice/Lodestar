#!/usr/bin/env node
/* ============================================================
   FSearch 原型 —— 本地真索引服务
   ============================================================
   用法:
     node prototype/dev-server.mjs              # 用默认目录
     node prototype/dev-server.mjs --port 8791 --path ~/java --path ~/Downloads
     node prototype/dev-server.mjs --path ~     # 扫整个家目录（~90 万条）

   它做什么:
     1. 真实遍历磁盘目录，建立内存索引（文件名/路径/大小/修改时间）
     2. 把【同一套】搜索语法跑在真索引上
        —— 语法实现不是复制粘贴，而是从 index.html 现抽核心逻辑，
           保证服务端与界面永远用同一份分词器
     3. 通过 /api/search 暴露查询，界面改成走这个接口

   关键点: 这不是"测试桩"，它就是 FSearch 的核心功能本体。
           以后接 Tauri 时，只需把这个 Node 服务换成 C shim + Rust，
           界面一行都不用改。
   ============================================================ */

import http from 'node:http';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/* ---------------- 配置 ---------------- */
const argv = process.argv.slice(2);
function arg(name, dflt) {
  const i = argv.indexOf('--' + name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : dflt;
}
const PORT = parseInt(arg('port', '8791'), 10);
const INDEX_PATHS = (() => {
  const paths = [];
  for (let i = 0; i < argv.length; i++) if (argv[i] === '--path') paths.push(argv[i + 1]);
  return paths.length ? paths.map(p => path.resolve(p.replace(/^~(?=$|\/)/, os.homedir())))
                      : [path.join(os.homedir(), 'java'), path.join(os.homedir(), 'Downloads')];
})();

/* 默认排除：与 FSearch 的排除规则同类，避免把 .git / node_modules 灌进索引 */
const SKIP_DIRS = new Set([
  '.git', '.svn', '.hg', 'node_modules', '.Trash', '.DS_Store',
  'Library', 'Caches', '.cache', '.npm', '.pnpm-store', '.cargo', '.rustup',
  '__pycache__', '.venv', 'venv', '.gradle', '.m2', '.idea', '.vscode',
  'target', 'dist', 'build', '.next', '.nuxt', '.output',
]);
const MAX_ENTRIES = parseInt(arg('max', '1200000'), 10);

/* ---------------- 索引 ---------------- */
let INDEX = [];                 // {id, name, dir, path, ext, size, mtime, isFolder}
let INDEXED_PATHS = [...INDEX_PATHS];
let SCANNING = false;
let SCAN_STARTED = 0;
let SCAN_TOOK = 0;
let ID_SEQ = 0;                 // 全局自增 id —— 前端靠它做选中/去重，缺了会导致「每行都选中」

function extOf(name) {
  const i = name.lastIndexOf('.');
  if (i <= 0 || i === name.length - 1) return '';
  return name.slice(i + 1).toLowerCase();
}

async function scanOne(root) {
  const out = [];
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let entries;
    try { entries = await fsp.readdir(dir, { withFileTypes: true }); }
    catch (e) { continue; }              // 无权限 / 已被删除
    for (const ent of entries) {
      if (out.length >= MAX_ENTRIES) return out;
      if (ent.name === '.DS_Store') continue;
      const full = path.join(dir, ent.name);
      let isFolder = ent.isDirectory();
      if (ent.isSymbolicLink()) {
        try { isFolder = (await fsp.stat(full)).isDirectory(); } catch { continue; }
      }
      if (isFolder) {
        if (SKIP_DIRS.has(ent.name) || ent.name.startsWith('.')) continue;
        out.push({ id: ID_SEQ++, name: ent.name, dir: dir + path.sep, path: full, ext: '',
                   size: 0, mtime: 0, isFolder: true });
        stack.push(full);
      } else {
        let st;
        try { st = await fsp.stat(full); } catch { continue; }
        out.push({ id: ID_SEQ++, name: ent.name, dir: dir + path.sep, path: full, ext: extOf(ent.name),
                   size: st.size, mtime: st.mtimeMs, isFolder: false });
      }
    }
  }
  return out;
}

async function rescan() {
  if (SCANNING) return;
  SCANNING = true; SCAN_STARTED = Date.now();
  const all = [];
  for (const p of INDEXED_PATHS) {
    try { const r = await scanOne(p); all.push(...r); }
    catch (e) { console.error('扫描失败', p, e.message); }
  }
  INDEX = all;
  SCAN_TOOK = Date.now() - SCAN_STARTED;
  SCANNING = false;
  console.log(`[索引完成] ${INDEX.length.toLocaleString()} 条，用时 ${(SCAN_TOOK/1000).toFixed(1)}s`);
}

/* ---------------- 复用 index.html 里的搜索核心 ----------------
   不复制粘贴语法实现 —— 从 index.html 现抽，保证服务端与界面同源。 */
function loadCore() {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
  const start = html.indexOf('function mulberry32');
  const end = html.indexOf('/* ============================================================\n   Vue 应用');
  if (start < 0 || end < 0) throw new Error('无法定位 index.html 的核心逻辑边界');
  let core = html.slice(start, end);
  // 把假数据集换成真索引
  core = core.replace('const DATA = buildDataset(50000);', 'const DATA = __REAL_INDEX__;');
  // 把假家目录换成运行服务的真实家目录（否则 path:~/... 永远匹配不到）
  core = core.replace("let HOME_PREFIX = '/Users/turan';", 'let HOME_PREFIX = __HOME__;');
  const factory = new Function('__REAL_INDEX__', '__HOME__', core +
    '\nreturn {runSearch, parseQuery, tokenize, markName, fmtSize, fmtTime};');
  return factory;
}

const CORE_FACTORY = loadCore();
let core = null;   // 索引建好后填充

function getCore(){
  if(!core) core = CORE_FACTORY(INDEX, os.homedir());
  return core;
}

/* ---------------- HTTP ---------------- */
const MIME = { '.html':'text/html; charset=utf-8', '.js':'text/javascript; charset=utf-8',
               '.css':'text/css; charset=utf-8', '.json':'application/json; charset=utf-8',
               '.jpg':'image/jpeg', '.png':'image/png', '.svg':'image/svg+xml' };

function send(res, code, body, type = 'application/json; charset=utf-8') {
  res.writeHead(code, {
    'Content-Type': type,
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Allow-Headers': 'Content-Type',
    'Access-Control-Allow-Methods': 'GET,POST,OPTIONS',
    'Cache-Control': 'no-store',
  });
  res.end(typeof body === 'string' || Buffer.isBuffer(body) ? body : JSON.stringify(body));
}

function readBody(req) {
  return new Promise(resolve => {
    let b = '';
    req.on('data', c => { b += c; if (b.length > 1e6) req.destroy(); });
    req.on('end', () => { try { resolve(JSON.parse(b || '{}')); } catch { resolve({}); } });
  });
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const p = url.pathname;

  if (req.method === 'OPTIONS') return send(res, 204, '');

  /* --- 状态 --- */
  if (p === '/api/status') {
    return send(res, 200, {
      ok: true, scanning: SCANNING, indexed: INDEX.length,
      took: SCAN_TOOK, paths: INDEXED_PATHS, mode: 'live', home: os.homedir(),
    });
  }

  /* --- 搜索 --- */
  if (p === '/api/search') {
    if (SCANNING) return send(res, 200, { ok: true, scanning: true, items: [], total: 0, fileCount: 0, folderCount: 0 });
    const q = url.searchParams.get('q') || '';
    const filter = url.searchParams.get('filter') || 'all';
    const sort = url.searchParams.get('sort') || 'name';
    const desc = url.searchParams.get('desc') === '1';
    const limit = parseInt(url.searchParams.get('limit') || '20000', 10);
    const t0 = Date.now();

    if (!core) core = getCore();

    let items = core.runSearch(q, INDEX);
    const fileCount = items.filter(x => !x.isFolder).length;
    const folderCount = items.length - fileCount;
    const total = items.length;

    if (filter === 'file') items = items.filter(x => !x.isFolder);
    else if (filter === 'folder') items = items.filter(x => x.isFolder);

    const k = sort, d = desc ? -1 : 1;
    items.sort((a, b) => {
      let r = 0;
      if (k === 'name') r = a.name.localeCompare(b.name, 'zh');
      else if (k === 'path') r = a.path.localeCompare(b.path, 'zh');
      else if (k === 'size') r = a.size - b.size;
      else if (k === 'mtime') r = a.mtime - b.mtime;
      if (r === 0) r = a.name.localeCompare(b.name, 'zh');
      return r * d;
    });

    const returned = items.length;
    if (items.length > limit) items = items.slice(0, limit);

    return send(res, 200, {
      ok: true, scanning: false, total, fileCount, folderCount, returned,
      took: Date.now() - t0, items,
    });
  }

  /* --- 增加索引目录 --- */
  if (p === '/api/index' && req.method === 'POST') {
    const body = await readBody(req);
    const dir = path.resolve(String(body.path || '').replace(/^~(?=$|\/)/, os.homedir()));
    if (!dir || !fs.existsSync(dir)) {
      return send(res, 400, { ok: false, error: '目录不存在: ' + dir });
    }
    if (!INDEXED_PATHS.includes(dir)) INDEXED_PATHS.push(dir);
    rescan();
    return send(res, 200, { ok: true, paths: INDEXED_PATHS });
  }

  /* --- 重建索引 --- */
  if (p === '/api/rescan' && req.method === 'POST') {
    rescan();
    return send(res, 200, { ok: true, paths: INDEXED_PATHS });
  }

  /* --- 静态文件 --- */
  let file = p === '/' ? '/index.html' : p;
  const abs = path.join(__dirname, path.normalize(file).replace(/^(\.\.[/\\])+/, ''));
  if (!abs.startsWith(__dirname) || !fs.existsSync(abs) || fs.statSync(abs).isDirectory()) {
    return send(res, 404, { ok: false, error: 'not found: ' + p });
  }
  send(res, 200, fs.readFileSync(abs), MIME[path.extname(abs)] || 'application/octet-stream');
});

server.listen(PORT, '127.0.0.1', async () => {
  console.log(`\n  FSearch 原型 · 真索引服务`);
  console.log(`  ────────────────────────────────────────`);
  console.log(`  地址    http://127.0.0.1:${PORT}/`);
  console.log(`  索引目录 ${INDEXED_PATHS.join('\n          ')}`);
  console.log(`  正在扫描…\n`);
  await rescan();
  console.log(`  索引就绪：${INDEX.length.toLocaleString()} 条，用时 ${(SCAN_TOOK/1000).toFixed(1)}s\n`);
});
