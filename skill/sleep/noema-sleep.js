#!/usr/bin/env node
// NOEMA · Sleep 掃描器（2026-11-11）
// 用法：node noema-sleep.js <phase> <memoryDir> [--budget N]
//   phase = sleep | scan | propose | apply | wake | purge
//   purge：不帶參數＝列出 archive/ 的銷毀候選；--files a.md,b.md＝永久刪除這些 archive 檔（只限 archive/）
// 每個階段都會更新報告檔 %LOCALAPPDATA%\Noema\sleep\<專案>.json，Noema 讀它來演動畫、標警示。
// 指令字串本身（含 noema-sleep.js 與 phase）也是 Noema 從 transcript 辨識階段的標記。
// 只讀記憶，不改記憶；唯一的寫入是報告檔與 sleep 階段的備份。
'use strict';
const fs = require('fs');
const path = require('path');
const os = require('os');

const [phase, memArg, ...rest] = process.argv.slice(2);
const PHASES = ['sleep', 'scan', 'propose', 'apply', 'wake', 'purge'];
if (!PHASES.includes(phase) || !memArg) {
  console.error('usage: node noema-sleep.js <' + PHASES.join('|') + '> <memoryDir> [--budget N]');
  process.exit(2);
}
const memDir = path.resolve(memArg);
if (!fs.existsSync(memDir)) { console.error('memory dir not found: ' + memDir); process.exit(2); }
const projDir = path.dirname(memDir);
const project = path.basename(projDir);
const budgetIdx = rest.indexOf('--budget');
const HOME = os.homedir();
const outDir = path.join(process.env.LOCALAPPDATA || path.join(HOME, 'AppData', 'Local'), 'Noema', 'sleep');
const reportPath = path.join(outDir, project + '.json');
fs.mkdirSync(outDir, { recursive: true });

const prev = (() => { try { return JSON.parse(fs.readFileSync(reportPath, 'utf8')); } catch { return {}; } })();
const BUDGET = budgetIdx >= 0 ? +rest[budgetIdx + 1] : (prev.budget || 5000);
const DAY = 86400000, now = Date.now();
const norm = p => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();

// ---------- token 估算：CJK 約 1 字 1 token，其餘約 3.5 字元 1 token ----------
function tokens(s) {
  const cjk = (s.match(/[　-〿㐀-鿿豈-﫿＀-￯]/g) || []).length;
  return Math.round(cjk + (s.length - cjk) / 3.5);
}
const read = p => { try { return fs.readFileSync(p, 'utf8'); } catch { return null; } };
const mdIn = d => { try { return fs.readdirSync(d).filter(f => f.toLowerCase().endsWith('.md')).map(f => path.join(d, f)); } catch { return []; } };

function bootStats() {
  const claude = read(path.join(HOME, '.claude', 'CLAUDE.md')) || '';
  const index = read(path.join(memDir, 'MEMORY.md')) || '';
  const t = { claudeMd: tokens(claude), memoryMd: tokens(index), memoryMdLines: index.split(/\r?\n/).length };
  t.total = t.claudeMd + t.memoryMd;
  return t;
}
function memStats() {
  const files = mdIn(memDir).filter(f => path.basename(f).toLowerCase() !== 'memory.md');
  return { files: files.length, tokens: files.reduce((s, f) => s + tokens(read(f) || ''), 0) };
}

// ---------- 讀取紀錄：從 transcript 撈 Read 工具碰到記憶檔的次數 ----------
function readHistory() {
  const jsonl = [];
  for (const e of fs.readdirSync(projDir, { withFileTypes: true })) {
    const p = path.join(projDir, e.name);
    if (e.isFile() && e.name.endsWith('.jsonl')) jsonl.push(p);
    else if (e.isDirectory() && e.name !== 'memory') {
      try { for (const f of fs.readdirSync(path.join(p, 'subagents'))) if (f.endsWith('.jsonl')) jsonl.push(path.join(p, 'subagents', f)); } catch {}
    }
  }
  const memN = norm(memDir) + '/';
  const stats = {};   // lowercased file name -> { reads, last, sessions:Set }
  let oldest = now, sessions = 0;
  for (const f of jsonl) {
    let text; try { text = fs.readFileSync(f, 'utf8'); } catch { continue; }
    if (!f.includes('subagents')) sessions++;
    oldest = Math.min(oldest, fs.statSync(f).mtimeMs);
    const sid = path.basename(f, '.jsonl');
    for (const line of text.split('\n')) {
      if (!line.includes('"tool_use"') || !line.includes('"Read"')) continue;
      let v; try { v = JSON.parse(line); } catch { continue; }
      const ts = Date.parse(v.timestamp || '') || 0;
      if (ts) oldest = Math.min(oldest, ts);
      for (const c of (v.message && v.message.content) || []) {
        if (c.type !== 'tool_use' || c.name !== 'Read' || !c.input) continue;
        const fp = norm(String(c.input.file_path || ''));
        if (!fp.startsWith(memN)) continue;
        const key = fp.slice(memN.length);
        const s = stats[key] || (stats[key] = { reads: 0, last: 0, sessions: new Set() });
        s.reads++; s.last = Math.max(s.last, ts); s.sessions.add(sid);
      }
    }
  }
  return { stats, windowDays: Math.max(1, Math.round((now - oldest) / DAY)), sessions };
}

// ---------- 規則判斷 ----------
const PATH_RE = /(?<![A-Za-z0-9\/])(?:[A-Za-z]:[\\/]|~[\\/])[^\s`'"<>|*?，。、；：（）「」『』【】！？)\]]+/g;
function missingPaths(text) {
  const out = new Set();
  for (let m of text.match(PATH_RE) || []) {
    m = m.replace(/[.,:;]+$/, '');
    if (/[<>{}*$%]|\.\.\./.test(m)) continue;              // 佔位符、萬用字元、變數
    const abs = m.startsWith('~') ? path.join(HOME, m.slice(2)) : m;
    if (!fs.existsSync(abs)) out.add(m);
  }
  return [...out];
}
function frontmatter(text) {
  const m = text.match(/^---\r?\n([\s\S]*?)\r?\n---/);
  return m ? m[1] : '';
}
function links(text) {
  const out = new Set();
  for (const m of text.matchAll(/\[\[([^\]|#]+)/g)) out.add(m[1].trim().toLowerCase());
  for (const m of text.matchAll(/\]\(([^)]+\.md)\)/g)) out.add(path.basename(m[1]).replace(/\.md$/i, '').toLowerCase());
  return out;
}

function scan() {
  const hist = readHistory();
  const files = mdIn(memDir).filter(f => path.basename(f).toLowerCase() !== 'memory.md');
  const docs = files.map(f => {
    const text = read(f) || '', fm = frontmatter(text);
    const name = (fm.match(/^name:\s*(.+)$/m) || [])[1];
    return { file: path.basename(f), stem: path.basename(f, '.md').toLowerCase(), name: (name || '').trim().toLowerCase(),
      text, fm, mtime: fs.statSync(f).mtimeMs, out: links(text) };
  });
  const incoming = {};
  for (const d of docs) for (const l of d.out) incoming[l] = (incoming[l] || 0) + 1;
  const index = read(path.join(memDir, 'MEMORY.md')) || '';
  const results = docs.map(d => {
    const h = hist.stats[d.file.toLowerCase()] || { reads: 0, last: 0, sessions: new Set() };
    const review = (d.fm.match(/^\s*review:\s*(\d{4}-\d{2}-\d{2})/m) || [])[1] || null;
    const flags = [];
    const miss = missingPaths(d.text);
    if (miss.length) flags.push('path');
    if (review && Date.parse(review) < now) flags.push('review');
    const ageDays = Math.round((now - d.mtime) / DAY);
    const cold = h.reads === 0 && ageDays >= Math.min(14, hist.windowDays);
    if (cold) flags.push('cold');
    const inc = (incoming[d.stem] || 0) + (d.name ? incoming[d.name] || 0 : 0);
    const linkedOut = [...d.out].some(l => l !== d.stem && l !== d.name);
    if (cold && !inc && !linkedOut) flags.push('orphan');
    if (!index.toLowerCase().includes(d.file.toLowerCase())) flags.push('unindexed');
    const hot = h.reads >= 3 || h.sessions.size >= 2;
    const suspicious = flags.includes('path') || flags.includes('review');
    return { file: d.file, tokens: tokens(d.text), reads: h.reads, sessions: h.sessions.size,
      lastRead: h.last ? new Date(h.last).toISOString() : null, ageDays, review, missing: miss,
      flags, hot, warn: hot && suspicious };
  });
  results.sort((a, b) => b.tokens - a.tokens);
  return { windowDays: hist.windowDays, sessions: hist.sessions, files: results };
}

// ---------- 階段 ----------
const report = { ...prev, project, memoryDir: memDir, budget: BUDGET, phase, phaseAt: new Date().toISOString() };
if (phase === 'sleep') {
  // 入睡：備份整個記憶目錄（含 archive/），再記下入睡前的 token
  const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
  const bak = path.join(outDir, 'backup', project, stamp);
  fs.cpSync(memDir, bak, { recursive: true });
  Object.assign(report, { startedAt: report.phaseAt, backup: bak, before: { boot: bootStats(), memory: memStats() }, after: null, files: prev.files || [] });
  console.log('[noema-sleep] 入睡 · 備份 → ' + bak);
  console.log('[noema-sleep] 開機注入 ≈' + report.before.boot.total + ' tok（CLAUDE.md ' + report.before.boot.claudeMd + ' + MEMORY.md ' + report.before.boot.memoryMd + '），預算 ' + BUDGET);
} else if (phase === 'scan') {
  const s = scan();
  Object.assign(report, { scannedAt: report.phaseAt, windowDays: s.windowDays, sessions: s.sessions, files: s.files });
  const LABEL = { path: '路徑失效', review: '過了複查日', cold: '冷（窗口內沒被讀過）', orphan: '孤兒（冷＋無連入連出）', unindexed: '沒進 MEMORY.md 索引' };
  console.log(`[noema-sleep] 掃描 · ${s.files.length} 條記憶 · 讀取紀錄窗口 ${s.windowDays} 天 / ${s.sessions} 個 session`);
  const warn = s.files.filter(f => f.warn);
  if (warn.length) {
    console.log('\n⚠ 又熱又可疑（每次都在用、內容可能已錯）——優先看：');
    for (const f of warn) console.log(`  ${f.file} | ${f.tokens} tok | 讀 ${f.reads} 次 | ${f.flags.join(',')}${f.missing.length ? ' | 失效：' + f.missing.join(' ') : ''}`);
  }
  for (const k of Object.keys(LABEL)) {
    const g = s.files.filter(f => f.flags.includes(k));
    if (!g.length) continue;
    console.log(`\n## ${LABEL[k]}（${g.length} 條，${g.reduce((a, f) => a + f.tokens, 0)} tok）`);
    for (const f of g) console.log(`  ${f.file} | ${f.tokens} tok | 讀 ${f.reads} 次 | ${f.ageDays} 天前改${k === 'path' ? ' | ' + f.missing.join(' ') : ''}${k === 'review' ? ' | 複查日 ' + f.review : ''}`);
  }
  const clean = s.files.filter(f => !f.flags.length);
  console.log(`\n## 規則沒抓到問題（${clean.length} 條，交給 AI 判斷重複／可壓縮）`);
  for (const f of clean) console.log(`  ${f.file} | ${f.tokens} tok | 讀 ${f.reads} 次${f.hot ? ' | 熱' : ''}`);
} else if (phase === 'propose' || phase === 'apply') {
  console.log('[noema-sleep] ' + (phase === 'propose' ? '提案，等候核准' : '執行核准的動作'));
} else if (phase === 'wake') {
  report.after = { boot: bootStats(), memory: memStats() };
  report.files = scan().files;
  const b = report.before || { boot: { total: 0 }, memory: { files: 0, tokens: 0 } }, a = report.after;
  console.log(`[noema-sleep] 醒來 · 開機注入 ${b.boot.total} → ${a.boot.total} tok（預算 ${BUDGET}）`);
  console.log(`[noema-sleep] 記憶 ${b.memory.files} 條 / ${b.memory.tokens} tok → ${a.memory.files} 條 / ${a.memory.tokens} tok`);
} else if (phase === 'purge') {
  // 銷毀：只動 archive/。沒給 --files 就只列候選；給了才永久刪除（呼叫前必須已取得用戶逐條確認）
  const arcDir = path.join(memDir, 'archive');
  const filesIdx = rest.indexOf('--files');
  const all = mdIn(arcDir).filter(f => path.basename(f).toLowerCase() !== 'archive.md');
  if (filesIdx < 0) {
    const hist = readHistory();
    const rows = all.map(f => {
      const h = hist.stats['archive/' + path.basename(f).toLowerCase()] || { reads: 0 };
      return { file: path.basename(f), tokens: tokens(read(f) || ''), ageDays: Math.round((now - fs.statSync(f).mtimeMs) / DAY), reads: h.reads };
    }).sort((a, b) => b.ageDays - a.ageDays);
    console.log(`[noema-sleep] 銷毀候選 · archive/ 共 ${rows.length} 條（只列不刪）`);
    for (const r of rows) console.log(`  ${r.file} | ${r.tokens} tok | ${r.ageDays} 天前改 | 窗口內讀 ${r.reads} 次${r.ageDays >= 30 && !r.reads ? ' | 建議可銷毀' : ''}`);
    report.purgeCandidates = rows;
  } else {
    const want = String(rest[filesIdx + 1] || '').split(',').map(s => s.trim()).filter(Boolean);
    const done = [], skipped = [];
    for (const name of want) {
      const p = path.resolve(arcDir, name);
      // 只接受 archive/ 底下的單一 .md 檔名；ARCHIVE.md 索引不可銷毀
      if (path.dirname(p) !== path.resolve(arcDir) || !/\.md$/i.test(name) || name.toLowerCase() === 'archive.md' || !fs.existsSync(p)) { skipped.push(name); continue; }
      fs.rmSync(p);
      done.push({ file: name, at: new Date().toISOString() });
    }
    report.purged = [...(prev.purged || []), ...done].slice(-200);
    report.lastPurge = { at: report.phaseAt, files: done.map(d => d.file) };
    console.log(`[noema-sleep] 銷毀 ${done.length} 條：${done.map(d => d.file).join(', ') || '（無）'}`);
    if (skipped.length) console.log('[noema-sleep] 略過（不在 archive/ 或不存在）：' + skipped.join(', '));
    console.log('[noema-sleep] 提醒：Sleep 入睡時的備份仍保有這些檔案（' + path.join(outDir, 'backup', project) + '）');
  }
}
fs.writeFileSync(reportPath, JSON.stringify(report, null, 2));
console.log('[noema-sleep] 報告：' + reportPath);
