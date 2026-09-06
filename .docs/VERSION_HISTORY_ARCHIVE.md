# VERSION HISTORY ARCHIVE

各ソースファイル末尾の `// Version History` を横断して追える形でまとめたもの。
新しいものが上。

---

## 2026-09-06 — 文字入力のもっさり／かくかく表示の修正（v0.1.11）

ブランチ: `fix/typing-latency`

### 症状と根本原因

Claude Code ペインでの文字入力が、素の Windows Terminal + claude より明らかに
遅く、表示がかくかくする（常に一定のもっさり。サイドバー非表示・スクロールは問題なし）。

- **主因 1**: イベントループが `draw → event::poll(30ms) → drain` の単一スレッドで、
  PTY reader スレッドがループを起こす手段を持たなかった。キーを子へ書いた直後に
  poll で寝るため、エコーは次のタイムアウトまで描かれない。Windows の既定タイマー
  分解能 15.6ms で待ちが切り上げられ、実質 31〜47ms。出力中の描画も約 25〜31fps 上限
- **主因 2**: バッチ末尾が Char/Enter/Tab なら毎回 `event::poll(5ms)` でペースト
  burst を待ってから転送していた（実質 16ms／打）。単発キーは `classify_run` で
  絶対にペーストにならないのに待っていた
- 副次: 描画クロージャから毎フレーム `pane.resize`（同サイズでも ResizePseudoConsole
  + vt100 set_size×2）、無条件 33fps 描画とセルごとの String 確保、1KB LineWriter
- 計測: PowerShell / Node の両方で `Wait(5ms)`≈16ms・`Wait(30ms)`≈33ms を実測。
  4 レンズ並列監査 → 10 候補 → 2 視点の反証チェックで主因 1 だけが生存、主因 2 は加算

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/wake.rs | 0.1 | 新規。`now_us` / `OutputStamp`（計測）/ `LoopMsg` / `OutputWaker`（未処理 1 通に間引く出力通知） |
| src/event.rs | 0.6 | ループを `recv_timeout` 一本に。入力ポンプスレッド `spawn_input_pump`（read + poll(0) drain で 1 通に束ねる）、`absorb`、`should_extend_burst`（Press 2 つ以上 or Event::Paste のときだけ burst 延長）、`extend_paste_burst`（出力通知では窓を延長しない）、`sync_pane_sizes`。dirty ゲート描画（出力は 8ms で束ね、入力は即描画）。`CCNEST_LATENCY_TRACE` |
| src/app.rs | 0.2 | wake チャネルの生成・配布、`pane_visible`、計測用フィールド |
| src/pane/mod.rs | 0.1 | `waker` / `last_size` / `resize_if_changed`（純関数 `next_size`）。respawn で last_size リセット |
| src/pane/pty.rs | 0.1 | `ReaderHooks`（stamp + waker）。reader は parser 更新後・ロック外で通知。`CCNEST_PTY_DUMP` で生バイト記録 |
| src/ui/mod.rs | 0.8 | 描画クロージャから `pane.resize` を撤去。`PaneCells` のセル文字列をスクラッチ再利用 |
| src/main.rs | 0.1 | stdout を 1MB BufWriter で包む |
| src/claude/launcher.rs | 1.2 | `spawn_claude` / `spawn_shell` が `ReaderHooks` を受け取る |
| vendor/vt100-0.15.2 | - | `Cell::write_contents`、`Grid::set_size` の同サイズ早期 return |
| Cargo.toml | - | 0.1.10 → 0.1.11 |

守った不変条件: 保留矢印 flush は drain + `process_batch` 直後（v0.1.10）。
`is_paste_candidate` の Press/Release 許容。classic モード既定。

### 検証

- 単体: `should_extend_burst` 6 件、`OutputWaker` 2 件、`next_size` 3 件、
  `format_latency_line` 2 件、vt100 パッチ 2 件を追加（計 246 件緑）
- E2E: `CCNEST_LATENCY_TRACE=1` で修正前後のバイナリに同じ 30 打鍵（SendInput、
  60ms 押下 + 120ms 間隔）を送り、`%APPDATA%\ccnest\latency-trace.log` の p50/p90/p99 を比較

### 計測結果（2026-09-06、Windows 11 26200 / WT 1.24 / Claude Code 2.1.263）

| 条件（打鍵→画面 total, ms） | 修正前 p50 / p90 / p99 | 修正後 p50 / p90 / p99 |
|---|---|---|
| Claude ペイン | 14.6 / 46.5 / 55.8 | **9.1 / 12.0 / 23.7**（うち約 7ms は Claude Code 自身の描画） |
| Claude ペイン burst_wait（ペースト判定待ち） | 10.0 / 13.9 / 14.2 | 0 / 0 / 0 |
| cmd.exe ペイン（ccnest 自身の遅延） | 14.4 / 46.7 / 47.4 | **1.3 / 1.7 / 2.0** |
| アイドル CPU（5 秒平均） | 1.6〜2.5% | 0.0〜0.3% |

修正前の p90 側の山（31〜47ms）が「かくかく」の正体（30ms poll の量子化。キーを離す
イベントがループを起こした打鍵だけ速く、残りはタイムアウト待ち）。

### 計測ハーネスの教訓（scratchpad `lat/measure.ps1`）

- `System.Windows.Forms.SendKeys` はジャーナルフック経由で打鍵が数百 ms 単位に束ねて
  届く。打鍵間隔を制御したいときは `SendInput`（KEYEVENTF_UNICODE、down/up 別送）を使う
- 押下→離すを連続で送ると、離すイベントが旧ループを起こして 30ms 待ちが隠れる。
  人間と同じく 60ms 程度ホールドしてから離す
- Claude Code は初見のフォルダで「trust this folder?」ダイアログを出し、打鍵はそこに
  吸われる（エコーが 1〜1.5 秒おきにしか来ず、conhost が出力を溜めていると誤診した）。
  計測用 cwd は `~/.claude.json` で信頼済みのフォルダにする。`CCNEST_PTY_DUMP` で
  生バイトを見れば一発で分かる
- 同サイズ `ResizePseudoConsole` を送ると conhost は毎回 viewport を再送してくる
  （旧ビルドは 33 回/秒これを受け取って描画していた）

## 2026-08-11 — ホイールで Claude 履歴が開く回帰の修正（v0.1.10）

ブランチ: `fix/wheel-history-regression`

### 症状と根本原因

ペイン上のホイール回転で Claude Code のプロンプト履歴（`History N/100`）が開く。
2026-05〜06 に修正済みだった問題の再発。

- Windows ConPTY はホイール 1 ノッチを `Mouse(ScrollUp/Down)` と幻の
  `Key(Up/Down)`（ファントム矢印）の両方として順不同・別バッチで配信する
- 防御（`classify_arrow` の Drop/Defer + `PAIR_WINDOW`=70ms のフラッシュ）は
  無傷だったが、フラッシュ判定が `event::poll` の**前**にあったため、
  ループ 1 周が 70ms を超えて停滞すると対のホイールが**キューに未読のまま**
  保留矢印が実キー化され `\x1bOA` が子へ漏れていた
- 停滞源 = 2 秒 tick の重量化: v0.1.7 の毎 tick libgit2 `Repository::discover`、
  v0.1.8 の全ペイン `parser.lock()`（PTY リーダースレッドと競合）+
  `session.refresh()` JSONL パース、さらに非表示サイドバーの毎 tick
  フル git status walk（従来から）

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/event.rs | 0.5 | フラッシュを drain + process_batch 直後へ移動（停滞しても未読ホイールが先に相殺する不変条件を構造で保証）。純関数 `pending_arrow_expired` 抽出。`pending_arrow_flush` トレース追加。サイドバー非表示中は git walk スキップ + 表示遷移で即時 refresh |
| src/app.rs | 0.1 | `refresh_pane_state` を `try_lock` 化（競合時は前回値据え置き）。ブランチ探索を 10s TTL キャッシュ化（純関数 `plan_branch_refresh`） |
| Cargo.toml | - | 0.1.6 → 0.1.10（README の 0.1.7〜0.1.9 に追いつき） |

### 検証

- 単体: `pending_arrow_expired` 境界 3 件 + `plan_branch_refresh` 5 件を追加
- E2E: `CCNEST_INPUT_TRACE=1` で Claude ストリーミング中にホイール連打 →
  履歴が開かず、input-trace.log に `pending_arrow_flush` が 0 件であること

---

## 2026-07-20 — ステータスバーにモデル名 / git ブランチを常時表示

ブランチ: `feature/status-model-branch`

### きっかけ

「今このペインの Claude はどのモデルか」「今どのブランチか」が画面から分からなかった。
ブランチは `sidebar/git.rs` に実装済みだったが、サイドバーを開いて Git セクションに
切り替えたときだけ表示され、しかも参照 cwd が ccnest 起動時のものに固定されていた。

### 実装前に判明した既存バグ（本題より重い）

**`session.rs::encode_project_dir` の変換規則が実データと一致していなかった。**

```
cwd     : C:\Users\mitam\Desktop\work\90_other\ClaudeCompany
実在dir : C--Users-mitam-Desktop-work-90-other-ClaudeCompany
旧ccnest: C-Users-mitam-Desktop-work-90_other-ClaudeCompany   ← 不一致
```

正しい規則は「**英数字以外はすべて `-` に置換**」（非 ASCII も 1 文字 = `-` 1 個）。
`~/.claude/projects/` の実ディレクトリ 8 件すべてで旧規則は不一致（0/8）。

結果、`session_path()` は常に存在しないパスを返し、`claude_ctx.rs` の `.ok()` が
握り潰していたため、**サイドバーの Claude コンテキスト表示は恒久的に
`(no session yet)`** だった＝機能が動いていなかった。単体テスト
`encodes_windows_path` が誤った期待値 `"C-Users-me-proj"` を固定していたため
CI でも検出できていなかった。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| `claude/session.rs` | 0.2 | encode 規則の修正 / `message.model` 抽出（`<synthetic>` 除外）/ バイト読み + 可変ウィンドウの `SessionTailer`（差分追記読み）/ `pretty_model()` |
| `sidebar/git.rs` | — | `branch_of()`（status 全走査をしない軽量版）/ unborn ブランチを `(detached)` と誤表示していたのを修正 |
| `pane/mod.rs` | — | `Pane.session: SessionTailer`。`respawn_as_shell` で `disable()` |
| `app.rs` | — | `branch_cache` / `refresh_pane_state()` / `focused_model_label()` / `focused_branch()` |
| `event.rs` | — | 2 秒 tick から `refresh_pane_state()` を呼ぶ |
| `sidebar/claude_ctx.rs` | — | 描画パスのディスク I/O を撤去（キャッシュ参照に）。`(no session yet)` と `(read error)` を分離 |
| `ui/mod.rs` | 0.4 | ステータスバー 1 行目に `cwd │ model │ ⎇ branch`。表示幅ベースの切り詰め |
| `ui/theme.rs` | 0.3 | `status_model` / `status_branch` |

### 設計上のポイント

- **描画パスから I/O を完全に排除**。`claude_ctx::rows()` は 30ms tick の描画から
  ペインごとに `read_to_string` + 全行 JSON パースをしていた（実測: 最大 18.7MB /
  約 0.15 秒）。取得は 2 秒 tick に移し、`ui::draw` はメモリを読むだけにした。
- **tail 読みは固定ウィンドウ不可**。実測で 1 行が最大 5.1MB、非 ASCII 率 37.7%。
  64KB から倍々に広げ、`b'\n'` で切ってから文字列化するので UTF-8 境界は壊れない。
- **`<synthetic>` の除外は `model` 文字列の完全一致で判定**。`message.id` が `msg_`
  始まりかどうかで判定する案は、現データでは同じ集合を返すが因果が逆なので不採用。
- **モデル名の短縮はテーブルではなく規則**（`claude-` を剥がし、末尾 8 桁日付を落とし、
  先頭を family、残る数値を `.` で連結）。将来のモデルにも効く。
- 切り詰めは cwd → ブランチ → モデルの順に削る（今回追加した情報を最後まで守る）。

### 既知の制約

- `/model` 切替は**次の assistant 応答が返るまで**表示に反映されない
  （モデル変更専用レコードが JSONL に無いため。実測で最大 80 秒の例あり）。
- rewind でセッションが別 UUID のファイルに分岐した場合は追従しない（表示が固まる）。
  `~/.claude/sessions/<pid>.json` を使う案は、ccnest が `CCNEST_CLAUDE_BIN` シム
  経由で claude を起動しており **PTY の子はシム、claude は孫**のため pid が
  一致せず不採用。mtime 追従案は同一 cwd の別ペインを掴む危険があるため不採用。
---

## 2026-07-20 — コンテキストウィンドウの自動判定（追補）

`session.rs` ver0.3。上の作業で「200k 固定なので 1M セッションだと
`237/200k (100%)` と嘘の表示になる」という制約が残っていたのを解消した。

### 調べたこと（結論: 直接の手がかりは無い）

セッションが 200k / 1M どちらの窓で動いているかを**ディスクから知る手段は無い**。

| 候補 | 判定 |
|---|---|
| transcript の `message.model` | `[1m]` サフィックスを**落として**記録される |
| transcript の他フィールド | `context_management` は常に `null`、`context_window`/`max_tokens`/`betas` は存在しない。`effort` は thinking effort であって窓ではない |
| `~/.claude/sessions/<pid>.json` | model フィールド自体が無い |
| `~/.claude.json` の `lastModelUsage` | `[1m]` は入るが「前回セッションの複数モデル合算」。当該プロジェクトでは `{}` で空、`lastSessionId` も稼働中セッションと不一致 |
| `additionalModelOptionsCache` | モデル選択メニューの中身であって選択結果ではない（実際 Opus 稼働中に Fable を載せていた） |
| `.toolUseResult.resolvedModel` | `[1m]` 付きで存在するが、直近25セッション中**7件(28%)にしか無く**、しかも親と別モデルを指す実例あり（親 `fable-5` / resolvedModel `opus-4-8[1m]`）。使うと誤情報になる |

> 補足: 生ログに対する `grep '[1m]'` は 25/25 件ヒットするが全て偽陽性。
> 注入される skill 一覧の `claude-api` 説明文に `[1m]` が文字列として含まれるため。
> JSON をパースして `.toolUseResult.resolvedModel` を読む以外に手は無い。

### 採った方針: 片方向にだけ確実な推論

累計使用量が 200k を**超えた**なら、その窓は物理的に 200k ではありえない
（プロンプトが自分の窓に収まらない）。よって収まる最小の段階まで繰り上げる。
逆向き（200k を超えていない → 200k の窓だ）は**言えない**ので推論しない。

- `CONTEXT_WINDOW_TIERS = [200_000, 1_000_000]`
- `SessionInfo.peak_used` は単調増加。compact で使用量が落ちても窓は 1M のまま
- `CCNEST_CONTEXT_WINDOW` の明示指定は推論より優先

実データでの効果:

```
227664 / 1000000 ( 23%)  Opus 4.8     ← 従来は 227664/200000 = 100%
358647 / 1000000 ( 36%)  Opus 4.8     ← 同上
121287 /  200000 ( 61%)  Opus 4.8     ← 判断材料が無いので据え置き（正しい挙動）
```

### 残る制約

- セッション開始直後〜200k を超えるまでは 1M セッションでも 200k と表示される。
  これは「知らないことを知らないと言う」正しい挙動だが、正確さが要るなら
  `CCNEST_CONTEXT_WINDOW=1000000` を明示する。
- ccnest を再起動した直後、compact 済みで使用量が 200k を下回っているセッションは
  200k と再推定される（tail 読みの窓に過去のピークが入らないため）。
