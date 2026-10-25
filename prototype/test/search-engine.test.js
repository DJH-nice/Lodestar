/* ============================================================
   FSearch 原型 —— 搜索引擎回归测试
   ============================================================
   用法:  node prototype/test/search-engine.test.js

   设计要点：
   - 每次都从 index.html 现抽核心逻辑再跑，保证测的是【当前文件】而不是副本
     （踩过的坑：手工抽出的 core.js 是旧快照，测出来的结论全是假的）
   - 覆盖：路径 vs 正则的分词、~ 展开、name: 语义、命中高亮的 HTML 合法性、
           高亮与匹配分词的一致性
   ============================================================ */
const fs = require('fs');
const path = require('path');

const SRC = path.join(__dirname, '..', 'index.html');

function loadCore() {
  const h = fs.readFileSync(SRC, 'utf8');
  const start = h.indexOf('function mulberry32');
  const end = h.indexOf('/* ============================================================\n   Vue 应用');
  if (start < 0 || end < 0) throw new Error('无法定位核心逻辑边界，index.html 结构可能变了');
  const core = h.slice(start, end);
  const out = path.join(require('os').tmpdir(), '_fsearch_core_test.js');
  fs.writeFileSync(out, core + '\nmodule.exports={buildDataset,DATA,tokenize,tokenizeQuery,parseQuery,parseSize,parseDate,globToRe,wildcardMatch,matchToken,runSearch,markName,esc,fmtSize,fmtTime,expandHome};');
  delete require.cache[out];
  return require(out);
}

let pass = 0, fail = 0;
const failures = [];
function t(name, fn) {
  try { fn(); pass++; console.log('  ✅ ' + name); }
  catch (e) { fail++; failures.push({ name, msg: e.message }); console.log('  ❌ ' + name + '\n       → ' + e.message); }
}
function trunc(s) { s = String(s); return s.length > 220 ? s.slice(0, 220) + '…(共' + s.length + '字符)' : s; }
function eq(a, b, msg) { if (a !== b) throw new Error(trunc((msg || '') + ' 期望 ' + JSON.stringify(b) + '，实际 ' + JSON.stringify(a))); }
function gt(a, b, msg) { if (!(a > b)) throw new Error(trunc((msg || '') + ' 期望 > ' + b + '，实际 ' + a)); }
function truthy(v, msg) { if (!v) throw new Error(msg || '期望为真'); }

const c = loadCore();
const R = c.runSearch;

console.log('\n【A】路径分词 —— 绝对路径不得被误判成正则');
t('A1 "~/java" 不应产生正则 token', () => {
  eq(c.parseQuery('~/java').filter(x => x.type === 're').length, 0, '正则 token 数:');
});
t('A2 "/Volumes/Work/Code" 不应产生正则 token', () => {
  eq(c.parseQuery('/Volumes/Work/Code').filter(x => x.type === 're').length, 0);
});
t('A3 "/usr/local/bin" 不应产生正则 token', () => {
  eq(c.parseQuery('/usr/local/bin').filter(x => x.type === 're').length, 0);
});
t('A4 真正的正则仍应被识别为正则', () => {
  const toks = c.parseQuery('/^log.*\\.txt$/');
  eq(toks.filter(x => x.type === 're').length, 1, '应识别 1 个正则:');
  eq(toks[0].val, '^log.*\\.txt$');
});
t('A5 "~/" 应判为路径而非正则', () => {
  const toks = c.parseQuery('~/');
  eq(toks.filter(x => x.type === 're').length, 0);
  truthy(toks.some(x => x.type === 'path'), '应含 path token');
});
t('A6 绝对路径应能匹配到该目录下的条目', () => {
  gt(R('/Users/turan/Documents/').length, 100, '应命中该目录下的条目数:');
});

console.log('\n【B】文档与实现必须一致');
t('B1 path:~/... 应展开 ~ 并命中（帮助面板如此承诺）', () => {
  const withTilde = R('path:~/Documents').length;
  const withAbs = R('path:/Users/turan/Documents').length;
  gt(withAbs, 0, '绝对路径基准命中数:');
  eq(withTilde, withAbs, '~ 展开后应与绝对路径等价:');
});
t('B2 name:index 应为「包含」语义，与帮助面板一致', () => {
  const bare = R('index').length;
  const named = R('name:index').length;
  gt(named, 0, 'name:index 命中数:');
  truthy(named >= bare * 0.5, `name:index(${named}) 应与裸词 index(${bare}) 同量级（包含语义）`);
});
t('B3 name:index* 通配仍应工作', () => { gt(R('name:index*').length, 0); });
t('B4 name:*.ts 通配仍应工作', () => { gt(R('name:*.ts').length, 0); });

console.log('\n【C】高亮渲染不得产生非法 HTML');
t('C1 重叠 needle 不得产生嵌套 <mark>', () => {
  const html = c.markName('index.ts', 'index ndex');
  truthy(!/<mark>[^<]*<mark/.test(html), '出现嵌套 mark: ' + html);
});
t('C2 多个不重叠 needle 应各自高亮', () => {
  eq((c.markName('index.ts', 'index ts').match(/<mark>/g) || []).length, 2, 'mark 数量:');
});
t('C3 同一 needle 重复出现应各自高亮', () => {
  eq((c.markName('index-index.ts', 'index').match(/<mark>/g) || []).length, 2, 'mark 数量:');
});
t('C4 高亮后必须仍是合法标签结构', () => {
  const html = c.markName('index.ts', 'index ndex');
  eq((html.match(/<mark>/g) || []).length, (html.match(/<\/mark>/g) || []).length, '开闭 tag 数:');
});

console.log('\n【D】深链解析');
t('D1 "#q=java" 应解析出 java', () => {
  const m = /(?:^|[#&])q=([^&#]*)/.exec('#q=java');
  eq(m ? decodeURIComponent(m[1]) : '', 'java');
});
t('D2 "?v=2#q=java" 中的查询参数不能混进查询内容', () => {
  const m = /(?:^|[#&])q=([^&#]*)/.exec('?v=2#q=java');
  eq(m ? decodeURIComponent(m[1]) : '', 'java');
});

console.log('\n【E】分词器单一事实来源');
t('E1 高亮分词与匹配分词必须覆盖同一段文本', () => {
  const qs = ['java', 'ext:ts size:>1M', '/Users/x/java', '/^re$/', 'a OR b', 'name:foo', '"多 词"'];
  for (const q of qs) {
    const a = c.tokenizeQuery(q).filter(x => x.cls !== 'tok-ws').map(x => x.text).join('');
    const b = c.parseQuery(q).map(x => String(x.src !== undefined ? x.src : x.val)).join('');
    eq(a, b, 'query=' + JSON.stringify(q) + ' 两套分词覆盖的文本不一致:');
  }
});

console.log('\n' + '='.repeat(56));
console.log(`  通过 ${pass}  失败 ${fail}`);
console.log('='.repeat(56));
if (fail) {
  console.log('\n失败明细:');
  failures.forEach(f => console.log('  · ' + f.name + '\n    ' + f.msg));
  process.exit(1);
}
