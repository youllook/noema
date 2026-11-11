# Noema

**A live window into Claude Code's memory.**

Noema is a small desktop window that shows, in real time, which of Claude Code's memory files are being read, created, edited or deleted, and helps you keep that memory lean.

![Noema demo](docs/demo.gif)

> Full-quality demo: [`docs/noema-demo.mp4`](docs/noema-demo.mp4) (20 s, 1920×1080, sample data)

Claude Code keeps per-project memory as Markdown files under `~/.claude/projects/<project>/memory/`, plus a global `~/.claude/CLAUDE.md`. These files are injected into every session or read on demand, but you normally never see that happen. Noema makes it visible.

*The name comes from phenomenology: a noema is "the object as it is thought about."*

## What you see

- **The slit**: a thin line of light at the center that stands for Claude itself. It breathes while idle, opens when memory is touched, and takes on the color of the latest event.
- **Memory cloud**: every memory file is a card in a 3D cloud. Scroll or drag to rotate it; it never spins on its own.
- **Beams by event type**:

  | Event | Color | What happens |
  |---|---|---|
  | Read | green | a beam flows from the slit to the card |
  | Create | blue | a new card surfaces out of the light |
  | Modify | gold | short pulses along a thin beam |
  | Delete | red | the beam finishes, then the card disappears |

- **Only active memories stand out**: a card being touched grows, turns opaque and comes to the front. Relation lines (`[[wiki links]]` and index links) are drawn *only* for those cards, so a glance tells you which memories are in use right now.
- **Boot injection as door handles**: `CLAUDE.md` and `MEMORY.md`, which are injected at the start of every session, are the two rings beside the slit. Each shows its approximate token count; hover to see the total against a budget line. The rings turn red when you're over budget.
- **Activity stream**: a timeline of every memory event in the current session. Click any entry to open that memory in the built-in Markdown reader.
- **Archive, delete or purge from the window**: right-click a card, or use the buttons in the reader. You can archive or permanently delete an active memory, and purge an archived one. Every action asks for confirmation first, with focus on "Cancel", and backs the file up before touching it. It also tidies the index: archiving moves the `MEMORY.md` line to `archive/ARCHIVE.md`, while deleting and purging remove the line. `CLAUDE.md` and `MEMORY.md` themselves can't be touched.
- **Auto-follow**: when you start or resume a Claude Code session, Noema switches to that project's memory automatically.

## Sleep: keeping memory lean (optional)

Memory tends to bloat over time with stale paths, finished progress notes and duplicates. The included **Sleep** skill gives a project's memory a "night's sleep", and Noema stages each phase:

1. **Sleep**: backs up the memory folder, and the window dims.
2. **Scan**: rule-based checks run first. They find local paths that no longer exist, notes past their `review:` date, memories never read in the transcript window ("cold"), and orphans with no links. A light sweep passes over the cloud and leaves marks. **⚠ hot** marks memories that are read often *and* look wrong, which are the most dangerous kind.
3. **Propose**: Claude then looks for duplicates and notes that can be compressed, and lists proposals grouped by reason. You approve them in batches.
4. **Apply**: fixes, compressions, merges and archiving, each shown as the usual colored beams.
5. **Wake**: the slit reopens and shows the boot-injection token count before and after.

**Purge** permanently deletes files, but only from `archive/` and only when you explicitly ask. In Noema the card is flooded with red light until it glows, then burns away, but only after the file is actually gone. If the command is denied, the card is restored.

Sleep never deletes anything outside `archive/`. Stale memories are *moved* to `archive/`, and every Sleep run starts with a full backup under `%LOCALAPPDATA%\Noema\sleep\backup\`.

## Install

**Requirements:** Windows 10/11 with the WebView2 runtime (already included in Windows 11 and current Windows 10), and Claude Code.

### Download

Grab `noema.exe` from the [latest release](../../releases/latest) and run it.

### Build from source

```bash
cd app
cargo build --release
```

The binary is at `app/target/release/noema.exe`. The UI (`app/ui/index.html`) is embedded at compile time, so rebuild after changing it.

### Install the Sleep skill (optional)

Copy `skill/sleep/` to `~/.claude/skills/sleep/`. Then, in Claude Code, say "sleep" (or `/sleep`) to run it on the current project's memory.

## Usage

| Action | How |
|---|---|
| Show / hide | `Ctrl+Alt+N`, or the tray icon |
| Rotate the cloud | scroll or drag on empty space |
| Read a memory | click a card, a door handle, or an activity entry |
| Archive / delete / purge a memory | right-click its card, or use the buttons in the reader (always confirmed) |
| Pin on top / collapse to capsule | title-bar buttons ▲ and − |
| Switch project | the project dropdown (it also auto-follows active sessions) |

## How it works

- A Rust core (`app/src/core.rs`) watches the current project's `memory/` folder and the session transcripts (`*.jsonl`) with filesystem notifications. It does not use hooks, and it does not touch `settings.json`.
- Each `tool_use` in a transcript is classified:
  - `Read` is a read.
  - `Write` to a file that didn't exist is a create. Any other `Write`, and every `Edit`, is a modify.
  - Shell `rm` / `mv` / `Remove-Item` on a memory file is a delete.
  - The Sleep script's own command line is recognised as a phase marker.
- The UI (`app/ui/index.html`) is a single HTML file rendered in WebView2 via [tao](https://github.com/tauri-apps/tao) + [wry](https://github.com/tauri-apps/wry).
- Window position and state are saved in `%LOCALAPPDATA%\Noema\state.json`.

## Privacy

Noema is **local**. It makes no network requests and sends nothing anywhere.

It only reads your memory and transcript files, with one exception: when you archive, delete or purge a memory from the window and confirm it. Before any such change, the file is backed up to `%LOCALAPPDATA%\Noema\trash\<project>\`.

Apart from that, the only file Noema writes is its own window state. The optional Sleep skill writes its report and backups to `%LOCALAPPDATA%\Noema\sleep\`.

## 中文簡介

**Noema 是 Claude Code 記憶的即時檢視窗。**

Claude Code 把每個專案的記憶存成 Markdown 檔，有些在每次對話開始時自動注入，有些在需要時才讀取。這些平常看不到，Noema 把它們即時畫出來。

- **門縫**：中央一道光代表 Claude。平常緩慢呼吸，記憶被碰到時打開，並跟著最新事件變色。
- **記憶雲**：每條記憶是一張卡片，排成 3D 記憶雲。滾輪或拖曳可以旋轉，平常不會自轉。
- **四色光束**：讀取綠、新增藍（新卡從光裡浮現）、修改金黃、刪除紅。
- **只凸顯正在用的記憶**：互動中的卡片會放大、浮到最前面，關聯線也只畫它們的，一眼看出哪些記憶正在被使用。
- **門把**：門縫兩側的光圈是開機注入的 `CLAUDE.md` 和 `MEMORY.md`，顯示 token 數。滑過去看預算線，超過預算時光圈轉紅。
- **Sleep 技能（選用）**：讓記憶「睡一覺」。依序是入睡（備份）、掃描（先用規則抓出路徑失效、過了複查日、太久沒讀、孤兒，⚠ 標出又熱又可疑的記憶）、AI 提出重複與可壓縮的條目、由您分批核准、醒來顯示 token 前後變化。
- **銷毀**：只限 `archive/` 裡的檔案，而且要您明確下令才會執行。卡片被紅光注入到發亮，確認檔案真的刪掉之後才燒成灰燼。
- **直接在視窗裡整理**：在卡片上按右鍵，或用閱讀器裡的按鈕，可以封存或永久刪除現役記憶，也可以銷毀封存的記憶。每次都會先跳確認框（預設焦點在「取消」），動手前先備份，並順手整理 MEMORY.md 和 ARCHIVE.md 的索引。

全程只在本機運作，也不連網。除了您在視窗裡確認過的封存、刪除、銷毀之外，Noema 不會改動記憶檔。

## License

[MIT](LICENSE)

*Noema is an independent project and is not affiliated with or endorsed by Anthropic. "Claude" and "Claude Code" are trademarks of Anthropic.*
