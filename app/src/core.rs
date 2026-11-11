//! 唯讀掃描 ~/.claude 的記憶與 session transcript，把快照與事件以 JSON 字串送出。
//! 這個模組只開檔讀取，不寫入、不刪除 ~/.claude 下任何東西。

use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub enum Cmd {
    Switch(String),
    Refresh,
    FsChanged,
}

const MAX_CONTENT: usize = 400 * 1024;
const HISTORY_KEEP: usize = 60;

fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_slash = false;
    for ch in s.chars() {
        let c = if ch == '\\' { '/' } else { ch };
        if c == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        for l in c.to_lowercase() {
            out.push(l);
        }
    }
    out
}

fn mtime_ms(p: &Path) -> u64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_text(p: &Path) -> Option<String> {
    let bytes = fs::read(p).ok()?;
    let slice = if bytes.len() > MAX_CONTENT { &bytes[..MAX_CONTENT] } else { &bytes[..] };
    Some(String::from_utf8_lossy(slice).into_owned())
}

fn file_json(p: &Path) -> Option<Value> {
    let content = read_text(p)?;
    Some(json!({
        "file": p.file_name()?.to_string_lossy(),
        "path": p.to_string_lossy(),
        "mtime": mtime_ms(p),
        "content": content,
    }))
}

fn md_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map(|e| e.eq_ignore_ascii_case("md")).unwrap_or(false))
        .collect();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    v
}

struct Tracked {
    offset: u64,
    top: bool,
    session: String,
}

pub struct Core {
    home: PathBuf,
    projects_dir: PathBuf,
    current: String,
    emit: Box<dyn Fn(String) + Send>,
    tracked: HashMap<PathBuf, Tracked>,
    seen: HashSet<String>,
    names: Vec<String>,
    history: Vec<Value>,
    session_id: String,
    cwd: String,
    /// 最近出現／消失的記憶檔（snapshot 可能比 transcript 早一步更新，用來判斷新增與刪除）
    appeared: HashMap<String, Instant>,
    vanished: HashMap<String, Instant>,
    /// Sleep 報告檔的 mtime，變了就重送
    report_mtime: u64,
}

impl Core {
    fn claude_md(&self) -> PathBuf {
        self.home.join(".claude").join("CLAUDE.md")
    }
    fn project_dir(&self) -> PathBuf {
        self.projects_dir.join(&self.current)
    }
    fn memory_dir(&self) -> PathBuf {
        self.project_dir().join("memory")
    }

    /// 所有有記憶的專案底下，頂層 session transcript 的 (路徑, 專案, mtime)
    fn session_mtimes(&self) -> Vec<(PathBuf, String, u64)> {
        let mut out = Vec::new();
        for e in fs::read_dir(&self.projects_dir).into_iter().flatten().flatten() {
            let p = e.path();
            if !p.is_dir() || !p.join("memory").is_dir() {
                continue;
            }
            let id = e.file_name().to_string_lossy().into_owned();
            for f in fs::read_dir(&p).into_iter().flatten().flatten() {
                let fp = f.path();
                if fp.extension().map(|x| x == "jsonl").unwrap_or(false) {
                    let t = mtime_ms(&fp);
                    out.push((fp, id.clone(), t));
                }
            }
        }
        out
    }

    fn list_projects(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for e in fs::read_dir(&self.projects_dir).into_iter().flatten().flatten() {
            let p = e.path();
            if !p.is_dir() || md_files(&p.join("memory")).is_empty() {
                continue;
            }
            let mut latest = 0u64;
            for f in fs::read_dir(&p).into_iter().flatten().flatten() {
                let fp = f.path();
                if fp.extension().map(|x| x == "jsonl").unwrap_or(false) {
                    let t = f
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    latest = latest.max(t);
                }
            }
            out.push((e.file_name().to_string_lossy().into_owned(), latest));
        }
        out.sort_by(|a, b| b.1.cmp(&a.1));
        out
    }

    fn snapshot(&mut self) {
        let projects = self.list_projects();
        let mem = self.memory_dir();
        let mut memories = Vec::new();
        let mut archive = Vec::new();
        let mut names = Vec::new();
        let mut index = None;
        for p in md_files(&mem) {
            let lower = p.file_name().unwrap().to_string_lossy().to_lowercase();
            names.push(lower.clone());
            if lower == "memory.md" {
                index = file_json(&p);
            } else if let Some(j) = file_json(&p) {
                memories.push(j);
            }
        }
        for p in md_files(&mem.join("archive")) {
            names.push(format!("archive/{}", p.file_name().unwrap().to_string_lossy().to_lowercase()));
            if let Some(j) = file_json(&p) {
                archive.push(j);
            }
        }
        if !self.names.is_empty() {
            let now = Instant::now();
            for n in names.iter().filter(|n| !self.names.contains(n)) {
                self.appeared.insert(n.clone(), now);
            }
            for n in self.names.iter().filter(|n| !names.contains(n)) {
                self.vanished.insert(n.clone(), now);
            }
            self.appeared.retain(|_, t| t.elapsed() < Duration::from_secs(10));
            self.vanished.retain(|_, t| t.elapsed() < Duration::from_secs(10));
        }
        self.names = names;
        let msg = json!({
            "type": "snapshot",
            "projects": projects.iter().map(|(id, t)| json!({"id": id, "active": t})).collect::<Vec<_>>(),
            "current": self.current,
            "cwd": self.cwd,
            "sessionId": self.session_id,
            "memoryDir": mem.to_string_lossy(),
            "boot": { "claudeMd": file_json(&self.claude_md()), "memoryIndex": index },
            "memories": memories,
            "archive": archive,
        });
        (self.emit)(msg.to_string());
    }

    /// 專案資料夾下所有 transcript：頂層 *.jsonl 與 <session>/subagents/*.jsonl
    fn list_transcripts(&self) -> Vec<(PathBuf, bool, String)> {
        let mut out = Vec::new();
        for e in fs::read_dir(self.project_dir()).into_iter().flatten().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if name == "memory" {
                    continue;
                }
                for s in fs::read_dir(p.join("subagents")).into_iter().flatten().flatten() {
                    let sp = s.path();
                    if sp.extension().map(|x| x == "jsonl").unwrap_or(false) {
                        out.push((sp, false, name.clone()));
                    }
                }
            } else if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
                let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
                out.push((p, true, stem));
            }
        }
        out
    }

    fn target_of_path(&self, raw: &str) -> Option<String> {
        let mut n = norm(raw.trim().trim_matches('"'));
        let home = norm(&self.home.to_string_lossy());
        if let Some(rest) = n.strip_prefix("~/") {
            n = format!("{home}/{rest}");
        }
        // Git Bash 風格 /c/Users/... → c:/users/...
        let b = n.as_bytes();
        if b.len() > 2 && b[0] == b'/' && b[2] == b'/' && b[1].is_ascii_alphabetic() {
            n = format!("{}:{}", b[1] as char, &n[2..]);
        }
        let n = n.trim_end_matches('/').to_string();
        if n == format!("{home}/.claude/claude.md") {
            return Some("CLAUDE.md".into());
        }
        let mem = norm(&self.memory_dir().to_string_lossy());
        if n == mem {
            return Some(String::new());
        }
        n.strip_prefix(&format!("{mem}/")).map(|s| s.to_string())
    }

    fn classify(&self, name: &str, input: &Value) -> Vec<(&'static str, String)> {
        let s = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let mut out = Vec::new();
        match name {
            "Read" => {
                if let Some(t) = self.target_of_path(s("file_path")) {
                    out.push(("read", t));
                }
            }
            // 新增 vs 修改：Write 到一個原本不存在（或剛冒出來）的檔＝新增，其餘＝修改
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                let p = if s("file_path").is_empty() { s("notebook_path") } else { s("file_path") };
                if let Some(t) = self.target_of_path(p) {
                    let key = t.to_lowercase();
                    let just_appeared = self.appeared.get(&key).map(|at| at.elapsed() < Duration::from_secs(10)).unwrap_or(false);
                    let fresh = !t.is_empty()
                        && name == "Write"
                        && (!self.names.contains(&key) || just_appeared)
                        && t != "CLAUDE.md";
                    out.push((if fresh { "create" } else { "modify" }, t));
                }
            }
            "Grep" | "Glob" => {
                if let Some(t) = self.target_of_path(s("path")) {
                    out.push(("mention", t));
                } else {
                    out.extend(self.mentions(s("pattern")));
                }
            }
            "Bash" | "PowerShell" => {
                let cmd = s("command");
                if let Some(ev) = self.sleep_marker(cmd) {
                    out.push(ev);
                } else {
                    out.extend(self.deletes_or_mentions(cmd));
                }
            }
            _ => {}
        }
        out
    }

    /// Sleep 技能的階段標記：`node .../noema-sleep.js <phase> "<本專案記憶目錄>" [--files a.md,b.md]`
    /// 回傳 ("sleep", phase)；purge 帶檔名時為 ("sleep", "purge:a.md,b.md")
    fn sleep_marker(&self, cmd: &str) -> Option<(&'static str, String)> {
        let c = norm(cmd);
        let at = c.find("noema-sleep.js")?;
        let own = format!("projects/{}/memory", self.current.to_lowercase());
        if !c.contains(&own) {
            return None; // 別的專案在睡，不演
        }
        let rest = c[at + "noema-sleep.js".len()..].trim_start_matches(|ch: char| ch == '"' || ch == '\'' || ch.is_whitespace());
        let phase: String = rest.chars().take_while(|ch| ch.is_ascii_alphabetic()).collect();
        if !["sleep", "scan", "propose", "apply", "wake", "purge"].contains(&phase.as_str()) {
            return None;
        }
        if phase == "purge" {
            if let Some(i) = c.find("--files") {
                let list: Vec<String> = c[i + 7..]
                    .trim_start_matches(|ch: char| ch == '"' || ch == '\'' || ch == '=' || ch.is_whitespace())
                    .split(|ch: char| ch == '"' || ch == '\'' || ch.is_whitespace() || ch == ';' || ch == '|')
                    .next()
                    .unwrap_or("")
                    .split(',')
                    .map(|f| f.trim().to_string())
                    .filter(|f| f.ends_with(".md"))
                    .collect();
                return Some(("sleep", format!("purge:{}", list.join(","))));
            }
        }
        Some(("sleep", phase))
    }

    /// 刪除／搬走記憶檔的 shell 指令算「刪除」；其餘只算「提及」
    /// 逐段判斷（; | & 換行切開）：只有「同一段裡有刪除動詞」的記憶檔才算刪除，
    /// 避免 `cat memory/a.md; rm /tmp/x` 把 a.md 誤判成刪除
    fn deletes_or_mentions(&self, cmd: &str) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        for seg in cmd.split(|ch: char| ch == ';' || ch == '|' || ch == '&' || ch == '\n') {
            let c = norm(seg);
            let words: Vec<&str> = c.split(|ch: char| ch.is_whitespace() || ch == '(').collect();
            let is_move = words.iter().any(|w| ["mv", "move", "move-item", "mi"].contains(w));
            let is_del = is_move
                || words.iter().any(|w| ["rm", "del", "erase", "remove-item", "ri", "unlink", "rmdir"].contains(w))
                || c.contains("git rm")
                || c.contains("rmsync(")
                || c.contains("unlinksync(");
            for (kind, t) in self.mentions(seg) {
                let ev = if !is_del || t.is_empty() || t == "CLAUDE.md" {
                    (kind, t)
                } else if is_move && t.starts_with("archive/") {
                    continue; // 搬進 archive 的目的地，不算刪除
                } else {
                    ("delete", t)
                };
                // 同一檔在多段出現：刪除優先，其餘去重
                if let Some(i) = out.iter().position(|(_, x)| *x == ev.1) {
                    if ev.0 == "delete" {
                        out[i] = ev;
                    }
                } else if out.len() < 5 {
                    out.push(ev);
                }
            }
        }
        out
    }

    fn sleep_report_path(&self) -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join("AppData").join("Local"));
        base.join("Noema").join("sleep").join(format!("{}.json", self.current))
    }

    /// 報告檔變了（或強制）就送給 UI
    fn send_report(&mut self, force: bool) {
        let p = self.sleep_report_path();
        let t = mtime_ms(&p);
        if !force && t == self.report_mtime {
            return;
        }
        let report = match fs::read_to_string(&p) {
            Err(_) => Value::Null, // 沒有報告檔＝沒睡過
            Ok(s) => match serde_json::from_str::<Value>(&s) {
                Ok(v) => v,
                Err(_) => return, // 寫到一半：不記 mtime，下一輪再讀
            },
        };
        self.report_mtime = t;
        (self.emit)(json!({"type": "sleep-report", "report": report}).to_string());
    }

    /// shell 指令只算「提及」：指令字串裡出現記憶目錄或 CLAUDE.md 的路徑
    fn mentions(&self, text: &str) -> Vec<(&'static str, String)> {
        let c = norm(text);
        let mut out = Vec::new();
        if c.contains(".claude/claude.md") {
            out.push(("mention", "CLAUDE.md".to_string()));
        }
        let suffix = format!(".claude/projects/{}/memory", self.current.to_lowercase());
        if c.contains(&suffix) {
            let before = out.len();
            // 剛被刪掉的檔也要認得（snapshot 可能已先更新）
            for n in self.names.iter().chain(self.vanished.keys()) {
                if c.contains(&format!("memory/{n}")) && out.len() < 5 && !out.iter().any(|(_, t)| t == n) {
                    out.push(("mention", n.clone()));
                }
            }
            if out.len() == before {
                out.push(("mention", String::new()));
            }
        }
        out
    }

    /// 從 offset 讀到最後一個換行，解析新增的行，回傳事件
    fn read_new(&mut self, path: &Path) -> Vec<Value> {
        let mut events = Vec::new();
        let Some(tr) = self.tracked.get(path) else { return events };
        let (offset, top, session) = (tr.offset, tr.top, tr.session.clone());
        let Ok(meta) = fs::metadata(path) else { return events };
        let len = meta.len();
        if len < offset {
            self.tracked.get_mut(path).unwrap().offset = len;
            return events;
        }
        if len == offset {
            return events;
        }
        let Ok(mut f) = fs::File::open(path) else { return events };
        if f.seek(SeekFrom::Start(offset)).is_err() {
            return events;
        }
        let mut buf = Vec::with_capacity((len - offset) as usize);
        if f.take(len - offset).read_to_end(&mut buf).is_err() {
            return events;
        }
        let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else { return events };
        let chunk = &buf[..=last_nl];
        self.tracked.get_mut(path).unwrap().offset = offset + last_nl as u64 + 1;

        let mut need_boot = top && offset == 0;
        for line in chunk.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let has_tool = find(line, b"\"tool_use\"") && find(line, b"\"assistant\"");
            if !has_tool && !need_boot {
                continue;
            }
            if !has_tool && line.len() > 200_000 {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
            let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("").to_string();
            if self.cwd.is_empty() {
                if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                    self.cwd = c.to_string();
                }
            }
            if need_boot && !ts.is_empty() {
                need_boot = false;
                events.push(json!({"ts": ts, "kind": "boot", "target": "", "tool": "SessionStart", "sub": false, "session": session}));
            }
            if !has_tool || v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else { continue };
            for c in content {
                if c.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                    continue;
                }
                let id = c.get("id").and_then(|t| t.as_str()).unwrap_or("");
                let name = c.get("name").and_then(|t| t.as_str()).unwrap_or("");
                let hits = self.classify(name, c.get("input").unwrap_or(&Value::Null));
                if hits.is_empty() || (!id.is_empty() && !self.seen.insert(id.to_string())) {
                    continue;
                }
                for (kind, target) in hits {
                    events.push(json!({"ts": ts, "kind": kind, "target": target, "tool": name, "sub": !top, "session": session}));
                }
            }
        }
        events
    }

    fn load_project(&mut self) {
        self.tracked.clear();
        self.seen.clear();
        self.history.clear();
        self.cwd.clear();
        self.session_id.clear();
        // 換專案時舊專案的檔名不能拿來比對「新出現」，否則整個新專案都會被當成新增
        self.names.clear();
        self.appeared.clear();
        self.vanished.clear();
        self.snapshot(); // 先填 names，classify 才認得檔名
        let list = self.list_transcripts();
        let latest = list
            .iter()
            .filter(|(_, top, _)| *top)
            .max_by_key(|(p, _, _)| mtime_ms(p))
            .map(|(_, _, s)| s.clone())
            .unwrap_or_default();
        self.session_id = latest.clone();
        let mut hist = Vec::new();
        for (p, top, session) in list {
            let is_latest = session == latest;
            let len = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            self.tracked.insert(p.clone(), Tracked { offset: if is_latest { 0 } else { len }, top, session });
            if is_latest {
                hist.extend(self.read_new(&p));
            }
        }
        hist.sort_by(|a, b| a["ts"].as_str().cmp(&b["ts"].as_str()));
        let skip = hist.len().saturating_sub(HISTORY_KEEP);
        self.history = hist.split_off(skip);
        self.snapshot(); // 帶上 cwd / sessionId
        self.send_history();
        self.send_report(true);
    }

    fn send_history(&self) {
        (self.emit)(json!({"type": "events", "history": true, "events": self.history}).to_string());
    }

    fn poll(&mut self, relist: bool) {
        if relist {
            for (p, top, session) in self.list_transcripts() {
                if !self.tracked.contains_key(&p) {
                    if top {
                        self.session_id = session.clone();
                    }
                    self.tracked.insert(p, Tracked { offset: 0, top, session });
                }
            }
        }
        let paths: Vec<PathBuf> = self.tracked.keys().cloned().collect();
        let mut live = Vec::new();
        for p in paths {
            live.extend(self.read_new(&p));
        }
        if !live.is_empty() {
            live.sort_by(|a, b| a["ts"].as_str().cmp(&b["ts"].as_str()));
            self.history.extend(live.iter().cloned());
            let skip = self.history.len().saturating_sub(HISTORY_KEEP);
            self.history.drain(..skip);
            (self.emit)(json!({"type": "events", "history": false, "events": live}).to_string());
        }
        self.send_report(false);
    }
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

pub fn run(rx: Receiver<Cmd>, tx: Sender<Cmd>, emit: Box<dyn Fn(String) + Send>) {
    let home = dirs::home_dir().or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from)).unwrap_or_default();
    let mut core = Core {
        projects_dir: home.join(".claude").join("projects"),
        home,
        current: String::new(),
        emit,
        tracked: HashMap::new(),
        seen: HashSet::new(),
        names: Vec::new(),
        history: Vec::new(),
        session_id: String::new(),
        cwd: String::new(),
        appeared: HashMap::new(),
        vanished: HashMap::new(),
        report_mtime: 0,
    };
    core.current = core.list_projects().first().map(|p| p.0.clone()).unwrap_or_default();

    let claude_md_name = std::ffi::OsString::from("CLAUDE.md");
    let home_claude = core.home.join(".claude");
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            // ~/.claude 是非遞迴監看，只在意 CLAUDE.md；記憶目錄則全收
            let relevant = ev.paths.iter().any(|p| {
                p.parent() != Some(home_claude.as_path())
                    || p.file_name().map(|n| n.eq_ignore_ascii_case(&claude_md_name)).unwrap_or(false)
            });
            if relevant {
                let _ = tx.send(Cmd::FsChanged);
            }
        }
    })
    .ok();
    if let Some(w) = watcher.as_mut() {
        let _ = w.watch(&core.home.join(".claude"), RecursiveMode::NonRecursive);
    }
    let mut watched: Option<PathBuf> = None;
    let mut rewatch = |core: &Core, watcher: &mut Option<notify::RecommendedWatcher>| {
        if let Some(w) = watcher.as_mut() {
            if let Some(old) = watched.take() {
                let _ = w.unwatch(&old);
            }
            let dir = core.memory_dir();
            if w.watch(&dir, RecursiveMode::Recursive).is_ok() {
                watched = Some(dir);
            }
        }
    };

    core.load_project();
    rewatch(&core, &mut watcher);

    // 自動跟隨：某個 session 新開、或閒置 2 分鐘以上後又動起來（喚醒），就切到它的專案。
    // 兩個對話同時在跑時互相不會搶，因為誰都不算「閒置後醒來」。
    const IDLE_MS: u64 = 120_000;
    let mut session_seen: HashMap<PathBuf, u64> =
        core.session_mtimes().into_iter().map(|(p, _, t)| (p, t)).collect();
    let mut last_follow = Instant::now();

    let mut dirty: Option<Instant> = None;
    let mut last_list = Instant::now();
    let mut last_fallback = SystemTime::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(Cmd::Switch(id)) => {
                if id != core.current && core.projects_dir.join(&id).join("memory").is_dir() {
                    core.current = id;
                    core.load_project();
                    rewatch(&core, &mut watcher);
                }
            }
            Ok(Cmd::Refresh) => {
                core.snapshot();
                core.send_history();
                core.send_report(true); // 頁面就緒後補送：啟動當下的報告可能在 __noema 定義前就送出而遺失
            }
            Ok(Cmd::FsChanged) => dirty = Some(Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if dirty.map(|t| t.elapsed() > Duration::from_millis(250)).unwrap_or(false) {
            dirty = None;
            core.snapshot();
        }
        // watcher 建不起來時的保底：每 5 秒重掃一次
        if watcher.is_none() && last_fallback.elapsed().map(|d| d.as_secs() >= 5).unwrap_or(true) {
            last_fallback = SystemTime::now();
            core.snapshot();
        }
        if last_follow.elapsed() > Duration::from_millis(1500) {
            last_follow = Instant::now();
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let mut woke: Option<(String, u64)> = None;
            for (p, id, t) in core.session_mtimes() {
                let prev = session_seen.insert(p, t);
                let changed = prev.map(|old| t > old).unwrap_or(true);
                // 新檔，或上次變動距今超過閒置門檻 → 視為新開／喚醒
                let awakened = match prev {
                    None => true,
                    Some(old) => changed && t.saturating_sub(old) > IDLE_MS,
                };
                if changed && awakened && now.saturating_sub(t) < 30_000 {
                    if woke.as_ref().map(|(_, wt)| t > *wt).unwrap_or(true) {
                        woke = Some((id, t));
                    }
                }
            }
            if let Some((id, _)) = woke {
                if id != core.current {
                    core.current = id;
                    core.load_project();
                    rewatch(&core, &mut watcher);
                } else {
                    // 同專案換了對話：重載以追上新 session
                    core.load_project();
                }
            }
        }
        let relist = last_list.elapsed() > Duration::from_secs(2);
        if relist {
            last_list = Instant::now();
        }
        core.poll(relist);
    }
}
