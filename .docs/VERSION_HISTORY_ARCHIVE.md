# VERSION HISTORY ARCHIVE

各ソースファイル末尾の `// Version History` を横断して追える形でまとめたもの。
新しいものが上。

---

## 2026-11-23 — スクロール中も色を判定する・裏が動いていても見るまで 🟨（v0.1.53）

ブランチ: `fix/status-scroll-and-unseen`

### 背景
ゆうさん（2026-11-23）:
1. 「CC の回答がスクロールの途中だと緑色のタブになる。手動でスクロールして最後まで見たらオレンジになった」
2. 報告で返答が終わっていても、フッターに `1 shell` があると 🟢 のまま。ゆうさんの判断待ちなのに分からない（シェルが本当に動いているときもあるので壁打ち）→ 案 A「見るまでオレンジ」に決定

### 原因
1. `App::refresh_pane_state` は `screen.scrollback() == 0` のときしか判定せず、履歴を見ている間は前回の状態（返答中の 🟢）を据え置いていた
2. 判定は「裏の仕事の札あり → Background（🟢）」だけで、返答を読んだかどうかを見ていなかった

### 変更
- `src/app.rs` `live_screen_text`: 判定のときだけ同じロックの中で `set_scrollback(0)` → 行を読む → 元の位置へ戻す
- `src/pane/status.rs` `unseen_turn_end`／`displayed_status`（純粋関数）: Busy → Background に変わった tick にそのタブを見ていなければ目印を立て、立っている間は NeedsAttention で出す。今見ているタブのペインは毎 tick で下ろす。Background 以外になったら下ろす
- `src/pane/mod.rs` `Pane.detected`（画面から読んだそのままの判定）・`Pane.unseen_turn_end`
- `CCNEST_STATUS_TRACE` の見出しに `detected`・`unseen`・`scroll`
- README のタブの節・`Cargo.toml` 0.1.53

### 決めたこと（ゆうさんの言葉に無かった所）
- 「見た」は今見ているタブ（`active_tab`）にそのペインがあること。2 秒 tick で判定するので、開いてから 🟢 になるまで最大 2 秒
- 窓を開いた直後・`--resume` で開いたタブ（前回の判定が Busy でない Background）は目印を立てない＝今までどおり 🟢
- 裏のエージェント／workflow 待ち（`Waiting for … to finish`）も同じ扱い

### テスト
- 単体: `turn_end_with_background_while_away_is_unseen` ほか 6 件（status.rs）・`live_screen_text_reads_bottom_and_keeps_scroll_position`（app.rs）
- E2E: `scripts/e2e/unseen-turn-end.ps1`（窓なし・偽 claude `turn-end-shim.ps1`・11 項目 PASS）。回帰に `idle-compaction-waiting.ps1`・`park-working-tabs.ps1`・`hold-sleep.ps1` も PASS

---

## 2026-11-21 — 止まっている保留タブの案内に「Alt+C で再開」と書く（v0.1.50）

ブランチ: `feature/hold-wake-hint`

### 背景
ゆうさん（2026-11-21）「キーを打ったまま良いね。何のキーを打てばよいか？ もういっかい ALT＋c だと分かりやすいかも」。
v0.1.49 の案内は「キーを押すと続きから再開」で、どのキーかが書かれていなかった。

### 変更（文言だけ・動きは変えていない）
- 枠のタイトル: 「保留中: Claude 停止・キーを押すと続きから再開」→「保留中: Claude 停止・Alt+C で再開」（`src/ui/mod.rs` `ASLEEP_TITLE_NOTE`）
- ステータスバーのお知らせ: 「(開いてキーを押すと続きから再開)」→「(もう一度 Alt+C で続きから再開)」（`src/app.rs` `hold_notice`）
- プロセスなしで開いた保留のセッションの画面: 「Alt+C で保留を外すと、会話の続きから再開します (文字を打っても再開します)。」（`src/pane/mod.rs` `DORMANT_HINT`）
- README のタブの節・`Cargo.toml` 0.1.50

### 動き（v0.1.49 のまま）
- 止まっているタブで Alt+C → 保留が外れて Claude が `--resume` で起きる
- 止まっているペインに文字・Enter・貼り付けを送っても起きる（保留は付いたまま）
- 開く・Alt+←→ で通るだけでは起きない

### テスト
- 単体: `asleep_pane_title_says_how_to_wake`（「Alt+C で再開」）・`hold_notice_says_what_happened`（「Alt+C」）
- E2E: `scripts/e2e/hold-sleep.ps1`（窓を作らない起こし方）

---

## 2026-11-21 — 保留にしたタブの Claude を止めてメモリを空ける（v0.1.49）

ブランチ: `feature/hold-sleep`

### 背景

ゆうさん「CCNEST ってタブ開いてるだけでメモリ食ってる？ 保留にしたやつはメモリ解放とかできるのかな？」。実測で claude.exe は 1 タブ 620〜849 MB（8 タブで約 6.0 GB・PC の空き 1.8 GB）。v0.1.48 の保留は見た目だけで、プロセスは動いたままだった。

レイナの案「対応待ち 🟨 のタブを保留にしたときだけ落とす。動いているタブは落とさない」→ ゆうさん「これお願い」。

### 動き

- Alt+C で保留にした瞬間、そのタブの claude のペインを 1 つずつ見て、**止めても失う物が無いと言えるペインだけ**プロセスを木ごと終了する
- 止めたペインは最後の画面をそのまま残す。枠のタイトルに「保留中: Claude 停止・キーを押すと続きから再開」
- **開くだけ・Alt+←→ で通り過ぎるだけでは起こさない**（メモリを使わない）
- 眠っているペインにキー（文字・Enter・矢印・貼り付け）を打つと、同じペインで `claude --resume <セッション id>` を起こし直す。そのキーは捨てる。保留を外したとき（Alt+C）も起こす。Ctrl+C は何もしない（シェル化もしない）
- ステータスバーの下段に 8 秒、結果を出す:「Claude を止めてメモリを空けました」／「作業中か入力の途中なので Claude は動かしたままです」
- `--resume`／`--add` で開く保留のセッションは、プロセスを起こさずに開く（再起動後の復元で保留のタブにメモリを使わない）。最初の一言（`--resume-prompt-file`）があるセッションは起こす
- `CCNEST_HOLD_SLEEP=off` で v0.1.48 と同じ（保留は見た目だけ）

### 「止めても失う物が無い」の決め方（ここが要）

`status::is_safe_to_stop`。次の全部を満たすときだけ真。読めない画面は偽＝止めない側に倒す。

- 実行中でない（スピナー行・`esc to interrupt` が無い）
- 裏の仕事が無い（フッターの札 `1 shell` など・`Waiting for … to finish`）。**タブの色（🟨）では決めない**: `detect_status` は「判断待ち」の節があれば裏でシェルが動いていても 🟨 を返すので、色で決めると裏の仕事を止めてしまう
- 入力欄が空で出ている（`at_empty_prompt`: 罫線 2 本のあいだが `❯` だけ）。許可・質問・計画承認のダイアログの途中（入力欄が無い）と、書きかけの入力がある画面は偽

加えて `app::sleep_check`:

- 画面は Alt+C を押したその場で読み直す（2 秒 tick の状態は古いことがある。履歴を見ている途中なら最下部へ戻してから読む）
- 最後にキーを送ってから 5 秒以内のペインは止めない（送った直後は、まだスピナー行が出ていないことがある）
- 画面のロックが取れなかったら止めない

止めるのは保留にしたその時だけ。保留中にあとから作業が終わったタブを自動で止めることはしない（見ていないときに、画面の読み違いでプロセスを止める事故を避ける）。

### 実装

- `src/pane/status.rs`: `at_empty_prompt`・`is_safe_to_stop`
- `src/pane/mod.rs`: `Pane.pty` を `Option` に（None = 眠っている）。`sleep()`・`dormant()`・`asleep()`・`since_last_write()`。`write()` は眠っていれば何もしない
- `src/app.rs`: 純粋関数 `sleep_check`・`hold_notice`・`opens_dormant`、`App::sleep_held_tab`・`wake_pane`・`wake_tab`・`notice`（時間で消えるお知らせ）。`refresh_pane_state` は眠っているペインを読み飛ばす。`spawn_next_resume` は保留のセッションを `Pane::dormant` で開く
- `src/event.rs`: `Action::ToggleHoldTab` のあとで `sleep_held_tab`／`wake_tab`。眠っているペインにフォーカスがあるときのキー（`is_wake_action` = 素通しと貼り付け）で `wake_pane`。2 秒 tick でお知らせを片付ける
- `src/ui/mod.rs`: `pane_title_asleep`。お知らせがあるあいだ、ステータスバーの下段（操作ヒントの行）をお知らせに替える（上段の cwd の枠は幅が足りないと真っ先に落ちるので使わない）

### 確認

- `cargo test`: 497 件（新規 10 件: `status` 5 件・`sleep_check_stops_only_when_it_is_known_to_be_safe`・`hold_notice_says_what_happened`・`opens_dormant_only_for_a_held_session_without_a_prompt`・`only_input_meant_for_the_pane_wakes_it`・`asleep_pane_title_says_how_to_wake`）。`cargo fmt --all --check`・`cargo clippy --all-targets -- -D warnings` 警告なし
- E2E `scripts/e2e/hold-sleep.ps1`: **PASS**（38 項目）。偽 claude（`hold-sleep-shim.ps1`）が Claude Code の画面の形を描く: 空の入力欄／スピナー行／書きかけの入力
  - 空の入力欄のタブを保留 → プロセスが消える・お知らせ／開く → 最後の画面と「保留中」・何も起きない／Alt+←→ で通る → 何も起きない
  - Enter → `--resume <同じ id>` で起動／保留を外す → 起きたまま／もう一度保留 → 止まる／眠ったまま外す → 起きる
  - スピナー行のタブ・書きかけのあるタブ・キーを送った直後のタブ → 止めない（キーの 6 秒後に保留し直すと止まる）
  - 別の窓を `--resume` で起こす → 保留のセッションはプロセスなしで開く・Enter で起きる
- 同 `-Off`: **PASS**（`CCNEST_HOLD_SLEEP=off` では止めない）
- **本物の Claude Code 2.1.330**（`hold-sleep-real.ps1`・sonnet）: **PASS**（14 項目）
  - 1 ターン打って返事が終わった画面（`❯` だけの入力欄）で Alt+C → `stopped=1 kept=0`・claude.exe 0 個・**650 MB 空いた**（残りはペインの擬似コンソールのホスト 1 個・約 1 MB）
  - Enter → `claude.exe --resume <同じ id>` が起動し、会話（`● mango`）が画面に戻る
  - 起こし直したセッションも、保留し直すと止まる
- 既存 E2E `hold-tab.ps1`（28 項目）・`park-working-tabs.ps1`・`drag-reorder-tab.ps1`・`dblclick-rename-tab.ps1`・`reopen-tab.ps1` は PASS のまま（これらの偽 claude は入力欄を描かないので、保留にしても止まらない = 今までどおり）
- E2E はどれも窓を作らない起こし方（CREATE_NO_WINDOW）で回した

### 残る制限

- 止めたペインを起こすと、モードは窓の既定（plan）に戻る（Ctrl+Shift+T と同じ）。入力欄の書きかけは、あれば止めないので消えない
- 質問・許可のダイアログが出ているタブは止めない（止めるとダイアログが消えるため）。保留にしてもメモリは空かない
- 本体の版が変わって入力欄の形（罫線 2 本と `❯`）が変わると、止めなくなる（お知らせは「動かしたまま」になる）。そのときは `CCNEST_STATUS_TRACE` で実物を取って `at_empty_prompt` を直す
- 止めたあと、ペインの擬似コンソールのホスト（約 1 MB）は ccnest がペインを閉じるまで残る。閉じたタブでも同じ（前からの動き）
- 保留中にあとから作業が終わったタブは、止まらない。空けたいときは一度外して保留し直す

---

## 2026-11-21 — タブの「保留」（Alt+C で灰色の丸 + 番号だけ・v0.1.48）

ブランチ: `feature/tab-hold`

### 背景

ゆうさん「一旦保留のタブもあることに気づいたから、タブ開いてる状態で Ctrl+C でアーカイブみたいな感じにして、灰色アイコンに変えて、アイコン + 数字だけのタブにできないかな？」。🟨（対応が必要）の中に「いまは手を付けない」タブが混ざると、本当に見るべき 🟨 が埋もれる。

壁打ちで決めたこと:

- キーは **Alt+C**。Ctrl+C はすでに 3 役（選択のコピー／Claude の中断／2 連打で Claude を落としてシェル化）で、保留のつもりで作業を止める事故になる。「タブをクリックした直後の Ctrl+C」案も、暴走しているタブを止めに行く操作（クリック → すぐ Ctrl+C）が保留になるのでやめた
- 戻すのは **手で**（もう一度 Alt+C）。状態が 🟢／🟨 に変わっても灰色のまま（ゆうさん「手で戻すまで灰色」）
- Claude Code 本体（2.1.330 の exe）に `alt+c`／`meta+c` の割り当ては無い。`meta+a` はあるので、代わりの候補だった Alt+A は選ばなくて正解だった

### 動き

- 前面のタブで Alt+C → そのタブはタブバーのいちばん右（2 本目の仕切り `│` の後ろ）へ行き `●5` だけになる。前面は隣のタブへ移る（メールのアーカイブと同じ）
  - 隣の決め方: 同じ組の右隣 → 同じ組の左隣 → 保留でない右のタブ → 保留でない左のタブ → 無ければそのまま
- 保留のタブをクリックすると、その場で名前つき（`● 5:名前`）で開く。そこで Alt+C → 名前つきの組の元いた位置へ戻る（前面はそのまま）
- 保留のタブは状態を出さない。実行中でも、終わって対応待ちになっても `●番号` のまま
- 緑を寄せるとき（2 秒 tick だけ）と違い、押した瞬間に位置が変わる。自分で押したキーなので、動くことが返事になる
- 保留はセッション id ごとに `%APPDATA%\ccnest\tab-holds.json` に保存。Ctrl+Shift+T・窓を閉じたあと・`--resume`／`--add` でも保留のまま。`CCNEST_TAB_HOLDS=<path>` で置き場所を変える
- ドラッグの並べ替えは同じ組の中だけ（組が 3 つになっただけで決まりは同じ）。`Alt+←→` は保留のタブも順に通る
- `CCNEST_TAB_GROUP=off` では位置を動かさず、その場で `●番号` にする（仕切りなし）

### 実装

- `src/app.rs`
  - `Tab.held`・`Tab::group()`・`enum TabGroup { Left, Working, Held }`・純粋関数 `tab_group(held, parked)`（保留が勝つ）
  - `display_order` は組の列を受ける（組の順に安定ソート）。純粋関数 `active_after_hold(order, groups, idx)`
  - `App::toggle_hold_tab(idx)`: 反転 → 途中のすべり・引いている最中の並べ替え・ダブルクリックの 1 回目を捨てる → 保留にしたときだけ前面を隣へ → 保存
  - `Tab.parked` は保留の間も裏で更新し続ける（外した瞬間に正しい組へ入る）
  - `ClosedTab.held`（開き直しても保留）・`saved_tab_hold`（`--resume`／`--add` で開くタブ）
- `src/tab_holds.rs`（新規）: `tab_names` と同じ型（書く前に読み直す・一時ファイル → rename・500 件まで）。中身は「セッション id → 保留にした時刻」
- `src/keymap.rs`: `Action::ToggleHoldTab`。`alt && !ctrl` の `c`／`C`（Ctrl+C と Ctrl+Alt+C は取らない）
- `src/event.rs`: `Action::ToggleHoldTab` → `toggle_hold_tab(active_tab)`。`same_group_rects` は組の型を問わない形に（`Tab::group` を渡す）。トレース `hold_tab idx=N held=… active=M`
- `src/ui/mod.rs`: `HELD_MARKER`（文字の `●`・1 桁）・純粋関数 `tab_marker`（保留は状態を見ない）。仕切りは組の境ごとに 1 本（最大 2 本）。ステータスバーのヒントに `Alt+C:hold`
- `src/ui/theme.rs`: `tab_held`（暗い灰色。丸も番号もこの色）

### マーカーの選び方

丸の絵文字に灰色は無い（⚪ 白／⚫ 黒）。Windows Terminal の実物に 3 案を並べて撮った:

- 文字の `●` を灰色で塗る: 小さく控えめ。「今は見なくていい」が伝わる
- ⚪: 🟢 と同じ大きさで白く光り、保留なのにいちばん目立つ
- ⚫: 黒地に溶けてほぼ見えない

初期値は `●`。替えるなら `HELD_MARKER` の 1 か所。

### 確認

- `cargo test`: 487 件（新規 14 件: `tab_holds` 6 件・`display_order_puts_held_tabs_last`・`tab_group_held_wins_over_parked`・`active_after_hold_*` 2 件・`alt_c_toggles_hold`・`ctrl_c_is_not_hold`・`held_tab_label_is_grey_dot_and_number`・`tab_marker_held_ignores_the_status`。`display_order_keeps_each_group_in_tab_order` は組の列に書き直し）。`cargo fmt --all --check`・`cargo clippy --all-targets -- -D warnings` 警告なし
- E2E `scripts/e2e/hold-tab.ps1`: **PASS**（28 項目）。偽 claude 5 本（bravo だけ実行中）
  - Alt+C → `alpha cc delta │ 🟢2 │ ●5`・前面は delta・保存 1 件／保留のタブをクリック → `● 5:echo`／もう一度 Alt+C → 元の位置・保存 0 件
  - alpha を保留 → 前面は右隣の cc（緑の bravo ではない）
  - 実行中の bravo を保留 → 実行をやめて対応待ちになっても `●2` のまま
  - 保留のタブを中クリックで閉じる → Ctrl+Shift+T → 保留のまま開き直る
  - Ctrl+C は保留にしない（`hold_tab` が出ない・キーは ccnest に届いている）
  - 別の窓を `--resume` で起こす → 保存にあるセッションのタブだけ `●2`
- 同 `-Off`: **PASS**（5 項目。その場で `●5`・仕切りなし）
- 同 `-Wt`: **PASS**。実物の Windows Terminal のスクショで `●1 ●4` が 1 桁ずつ・灰色で出ることを確認
- 既存 E2E `park-working-tabs.ps1`（21 項目）・`drag-reorder-tab.ps1`・`dblclick-rename-tab.ps1`・`reopen-tab.ps1` は PASS のまま

### E2E は窓を作らずに回す（CREATE_NO_WINDOW）

ゆうさん「テストはデスクトップ 2 でやって」。テストの窓が今の画面に出て、打っている文字（「もて」）がそちらへ入っていた。3 つ試して、最後の形に落ち着いた。

1. **別のデスクトップへ移して見せる（取り下げ）**: conhost を隠して起こし、最初のプロセスにしたスクリプトが自分の窓を `IVirtualDesktopManager.MoveWindowToDesktop` で移してから出す形（`on-desktop2.ps1`→`on-other-desktop.ps1`）。移すこと自体はできた（`move_hr=0`）。しかし
   - この日、ゆうさんの窓は全部 Win+Tab の並びで 2 番目のデスクトップにあった。「デスクトップ 2」へ移すと目の前に出る
   - 「今いない方」へ移す形に変えて試したら、その窓が前面になり、**Windows が表示ごとそのデスクトップへ切り替えた**。ゆうさんから見ると窓が全部消えた（実害: 作業が止まり、別のセッションで原因を調べてもらうことになった。何も閉じてはいない）
2. **隠したまま回す（不採用）**: `Start-Process conhost.exe -WindowStyle Hidden`。窓は 1 度も見えないが、計測すると**隠れた ConsoleWindowClass の窓が前面の窓になる**（5 本を回す間のサンプル 1,055 回のうち 770 回）。ゆうさんのキー入力が見えない窓へ吸われる
3. **窓を作らない（採用）**: `ProcessStartInfo.CreateNoWindow = true`（`CREATE_NO_WINDOW`）で `cmd.exe /c <run.cmd>` を起こす。コンソールはあるが窓が無いので、出るものも前面になるものも無い。画面の読み取り（`ReadConsoleOutputW`）と入力（`WriteConsoleInputW`）は窓が無くても通る
   - 計測: 5 本を回す間のサンプル 1,046 回で、見えるテストの窓 0 回・テストの窓が前面 0 回。5 本とも PASS

- 入れたのはタブ系の 5 本（`hold-tab`・`park-working-tabs`・`drag-reorder-tab`・`dblclick-rename-tab`・`reopen-tab`）。ほかの E2E は、次に回す前に起こす行を同じ形へ替える
- **Windows Terminal の窓（`-Wt`・`*-wt.ps1`）はこの形にできない**（実物の窓を撮るのが目的）。今の画面に出て前面を取る。絵文字や色の見た目を撮るときだけ、回数を最小にして使う
- 窓が見える形（1 のとき）は、ゆうさんの本物のマウスがテストの窓の上を通ると本物のマウス入力が混ざり、ドラッグの手順が途中で切れた（`park-working-tabs.ps1` が 5 回中 2 回落ちた）。窓を作らない形では起きていない

### 残る制限

- conhost は `●` を 2 桁で描く（WT は 1 桁）。conhost で保留のタブより右の桁がずれ、閉じたあとに字が残って見えることがある。ccnest は WT 前提（`…` や `│` と同じ扱い）
- 保留にしてもプロセス（claude.exe）は動いたまま = メモリは空かない。実測で 1 タブの claude.exe は 620〜849 MB（8 タブで約 6.0 GB）。保留のタブのプロセスを落として、開いたときに `--resume` で戻す形は別の変更

---

## 2026-11-21 — 緑のタブは右へ小さく寄せ、オレンジを左に固める（v0.1.47）

ブランチ: `feature/tab-park-working`

※ 版番号: 作業中に別のセッションが v0.1.46（`--resume-prompt-file`）を main へ入れたので、こちらは v0.1.47 に振り直した。衝突は文書と末尾の履歴だけで、コードは自動で合わさった（合わせたあとに全テストと E2E を回し直した）。

### 背景

ゆうさん「緑アイコンのタブは、グルーピングしてまとめられないかな？基本オレンジアイコンのタブしか私みないので」。壁打ちで 3 案（細くする／1 つにまとめる／オレンジを左に寄せる）を出し、ゆうさんの答えは「B（1 つにまとめる）でいってみよう」→ 続けて「オレンジは左に固めたかった」「小さくなれば十分」。

読み取り: 欲しいのは「オレンジが左にまとまっていること」で、緑は消えなくてよく小さくなれば足りる。そこで「1 つのチップにまとめて開閉する」のではなく、**緑を 1 枚ずつ小さく（🟢番号）して右へ寄せる**形にした。まとめるチップが無いので開閉の操作が要らず、緑のタブも 1 クリックで行ける。

### 動き

- 緑（実行中／裏で実行中）が 4 秒続いたタブは、タブバーの右側へ移り `🟢3` のように「マーカー + 番号」だけになる
- 対応が必要（🟨）のタブと、claude の無いタブは左に名前つきで残る。左右の境に仕切り `│`
- どちらの組の中でも元の並び順のまま。🟨 に戻ったタブは、待たずに、名前つきの組の元いた位置へ戻る
- 前面のタブは、寄せてあっても名前つきで出す（小さいタブをクリックすると、その場で名前つきに広がる）
- 次／前のタブ（`Alt+←→`・`Ctrl+Tab`）は、タブバーに見えている並びの順
- ドラッグの並べ替えは同じ組の中だけ（組は状態で決まるので、引いても越えられない）
- タブの番号は本当の並び順のまま（画面の上では連番にならない）
- `CCNEST_TAB_GROUP=off` で今までどおり

### 位置が勝手に動かない工夫

壁打ちで挙げた弱点（タブの位置が勝手にずれる → 押し間違い）への対策。

- 組が変わるのは 2 秒 tick（`App::refresh_tab_parking`）だけ。クリックした瞬間には何も動かない
- タブを引いている最中・名前の入力中・タブをクリックしてから 600 ms（ダブルクリックの途中かもしれない）は組を変えず、次の tick へ回す
- 前面にしたタブを左へ動かさない: 組は状態だけで決め、前面かどうかは「名前を出すか」にだけ効く。小さいタブをダブルクリックしても 2 回目が同じタブに当たる（マーカーの桁は変わらない）
- 緑になってすぐは寄せない（4 秒の猶予）。すぐ終わるターンでタブが右へ行って戻るのを避ける

### 実装

- `src/app.rs`
  - `Tab.parked`／`Tab.working_since`、`PARK_AFTER`（4 秒）、`tab_group_enabled()`（`CCNEST_TAB_GROUP`）
  - 純粋関数 `park_step(prev, working, now)`・`display_order(parked)`・`neighbor_in_order(order, active, dir)`
  - `App::tab_status`（マーカーと寄せる判定で共用）・`tab_order`・`refresh_tab_parking`・`cancel_slides_outside_group`
  - `next_tab`／`prev_tab` は `tab_order` の上で動く
- `src/event.rs`: 2 秒 tick で `refresh_pane_state` の直後に `refresh_tab_parking`。ドラッグは `same_group_rects` で同じ組の矩形だけを `tab_drop_target` に渡す（組の中はタブバーの並び = タブ番号の順なので、判定は今までどおり）
- `src/ui/mod.rs`: `draw_tabbar` は `tab_order` の順に描く。寄せたタブは `tab_label_small`、境に `TAB_GROUP_DIVIDER`。浮かせるタブはタブ番号から items の位置へ直す

### 確認

- `cargo test`: 473 件（先に main に入った v0.1.46 の分を合わせた数。新規 5 件: `park_step_parks_after_the_grace_and_unparks_at_once`・`display_order_keeps_each_group_in_tab_order`・`neighbor_in_order_follows_the_tab_bar`・`tab_label_small_is_marker_and_number_only`・`drag_targets_stay_inside_the_dragged_tabs_group`）。`cargo fmt --all --check`・`cargo clippy --all-targets -- -D warnings` 警告なし
- E2E `scripts/e2e/park-working-tabs.ps1`: **PASS**（21 項目）。偽 claude 5 本（alpha・delta = 対応待ち／bravo・cc・echo = 実行中。`idle<N>` のファイルを置くと実行中をやめる）
  - 猶予のあと `alpha delta │ 🟢2 🟢3 echo(前面)` → alpha をクリック → `🟢2 🟢3 🟢5`（名前つきの組は動かない）
  - 小さい 3 をクリック → その場で `3:cc`（マーカーの桁は同じ）／小さい 2 をダブルクリック → 名前の入力が開く
  - cc が実行中をやめる → `alpha cc delta │ …`（元の並びの位置へ戻る）
  - alpha を cc の上へ引く → `move_tab from=0 to=2` の 1 回（間の bravo は別の組）／寄せた組の上まで引く → 名前つきの組の右端で止まる
  - Alt+→: 名前つきの組の右端 → 寄せた組の先頭 → … → 名前つきの組の先頭へ回る
- 同 `-Off`: **PASS**（`CCNEST_TAB_GROUP=off` で 5 枚とも名前つき・並び順どおり・仕切りなし）
- 同 `-Wt`: **PASS**。実物の Windows Terminal のスクショで、仕切り `│` と `🟢2` が 1 桁ずつずれずに出ることを確認
- 既存 E2E `dblclick-rename-tab.ps1`（27 項目）・`drag-reorder-tab.ps1`（19 項目）は PASS のまま（偽 claude が対応待ちなので寄せられない）

### 残る制限

- 判定が一瞬だけ 🟨 に振れると、そのタブは左へ出て 4 秒後に右へ戻る。実物で起きるかは未確認（起きたら `CCNEST_STATUS_TRACE` で取る）
- 寄せたタブに手で付けた名前・タスク名は、前面にするまで見えない

---

## 2026-11-21 — 再開したセッションに最初の一言とモードを渡す（`--resume-prompt-file`／`--resume-permission-mode`・v0.1.46）

ブランチ: `feature/resume-prompt-file`

### 背景

メイン PC が Windows Update で夜中に自動で再起動し、動いていた Claude セッションが全部止まる（11/12 0:35・11/21 0:12）。ゆうさん「セッションの控えを自動でとって、再起動したら、そのセッションを再開させて、各セッションで、続きを行える？」。

`ccnest --resume` は開き直すだけで、最初の一言を渡す口が無かった。また、開き直したタブは全部 plan モードで始まるので、続きを指示しても計画を出し直して承認待ちで止まる。ゆうさんの決定は「直前のモードに戻す」。

### 動き

- `--resume <id>[=<名前>]` の後ろに `--resume-prompt-file <パス>` → そのセッションだけ、ファイルの文面を最初の一言として送る（`claude --resume <id> "<文面>"`）。文面は BOM を外して 1 行に畳む
- `--resume-permission-mode <モード>` → そのセッションだけ、そのモードで始める
  - `bypassPermissions` → `--dangerously-skip-permissions`（`CCNEST_CLAUDE_PERMISSION_MODE=off` と同じ渡し方）
  - それ以外 → `--permission-mode <モード>`
  - 安全弁 `CCNEST_CLAUDE_NO_SKIP_PERMISSIONS` が効いている窓では bypass に上げない
- どちらも直前の `--resume` 1 つにだけ効く。付けていないセッションは今までと 1 文字も変わらない
- `--add`（動いている窓への依頼）でも同じ指定が届く（`ResumeSpec` に載る）
- 誤用（`--resume` より前・読めないファイル・空のファイル・英字でないモード）は窓を開く前にエラー終了

### 触った所

- `src/cli.rs`: `ResumeSpec.prompt`／`.permission_mode`・`parse_args_with`（ファイル読みを注入）・`normalize_prompt`
- `src/claude/launcher.rs`: `ResumeExtras`・`opts_for_resume`・`build_claude_args_with`・`spawn_claude` が extras を受け取る
- `src/pane/mod.rs`: `Pane::spawn_with`（`Pane::spawn` はそのまま）
- `src/app.rs`: `with_resumes`／`spawn_next_resume` が extras を渡す

### 確かめ

- 単体テスト 468 本（追加 10 本）・clippy
- `scripts/e2e/resume-prompt.ps1`: 使い捨ての ccnest＋偽 claude。文面（日本語・空白・`;` 入り）が 1 つの引数で届く／指定の無いセッションは窓の既定のまま／誤用 3 種はエラー → PASS
- `scripts/e2e/resume-prompt-real.ps1`: 本物の claude（sonnet）。再開 3 秒後に文面がユーザー発話として会話ログに入り、返事が返り、`permissionMode` は `bypassPermissions` → PASS。本物のシム（`ccnest-claude-launcher.exe`）越しの引数は `--settings … --effort high --resume <id> --dangerously-skip-permissions "<文面>"`

### 使う側

`~/.claude/scripts/claude-sessions-restore.ps1 -AutoContinue`（ver1.3）と `~/.claude/scripts/wu-restart-notify/`（Windows Update の通知・控え・再起動後の復元）。
## 2026-11-21 — タブをダブルクリックして名前を変える（v0.1.45）

ブランチ: `feature/tab-dblclick-rename`

### 背景

ゆうさん「ccnest でタブをダブルクリックするとセッション名を変えられるようにして」。

読んで分かったこと:

- 名前変更の入口は `Alt+F` だけで、マウスの入口が無かった
- **`Alt+F` で付けた名前は、Claude のタブでは表示に出ていなかった**。付けた名前は `Tab.title`（フォルダ名の所）に入るが、タブの表示は Claude のタスク名（`/rename` > 日本語名 > 英語タイトル > 最後の発話）があればそちらを出す。ほぼ全部のタブが Claude なので、名前変更は実質効いていなかった
- 名前の入力中の貼り付け（端末の貼り付け・Enter を含むかたまり）は、名前ではなく裏のペインへ流れていた
- 付けた名前はメモリの中だけで、窓を閉じると消える

入口だけ足しても名前が変わって見えないので、入口・表示・貼り付け・保存をそろえた。

### 動き

- タブをダブルクリック（同じタブを 500 ms 以内に 2 回・間に並べ替えなし）→ そのタブが名前の入力になる。1 回目のクリックは今までどおり即タブ切替
- 入力は Explorer の名前変更と同じ型
  - 今の名前が丸ごと選ばれた状態（青い帯に白文字）で始まる。最初の 1 文字で丸ごと置き換え、Backspace で全部消える
  - Enter で確定、Esc で取り消し。入力中のタブ以外を左クリックしても確定（そのクリックは飲む＝タブは切り替わらず、選択も始まらない）
  - 何も打たずに Enter → 何も変えない（今出ている自動の名前を、手で付けた名前として固めない）
  - 空にして Enter → 手で付けた名前を外し、自動の名前に戻る
  - Ctrl+V と端末の貼り付けは名前の欄へ（最初の空でない 1 行・制御文字は落とす）
  - 入力中はマウスでタブを切り替え・閉じる・並べ替えできない
- 手で付けた名前は、自動の名前より優先して `N:名前` で出る（16 セルで切るのは今までどおり）
- `Alt+F` も同じ入力に入る
- 名前は `%APPDATA%\ccnest\tab-names.json`（セッション id → 名前）に覚える。Ctrl+Shift+T の開き直し・窓を閉じたあと・`--resume <id>`・`--add` で同じ名前が出る

### 決めたこと

- **名前は ccnest の中だけ**。Claude 本体の `/rename`（`/resume` の一覧に出る名前）には書かない。動いている claude の会話ログへ外から行を足す・入力欄へ `/rename` を打ち込む、はどちらも返答中に壊す恐れがある
- 分割しているタブは、全ペインのセッション id に同じ名前を覚えさせる（どのペインにフォーカスがあっても、どのペインのセッションを後で開いても消えない）
- 配布（v タグ）は今回しない

### 実装

- `src/tab_names.rs`（新規）: `load`／`set`／`remove`／`lookup`。書く前に読み直す（複数の窓が同じファイルを使う）・一時ファイル → rename・500 件を超えたら付けた時刻の古い順に落とす・`CCNEST_TAB_NAMES=<path>` で置き場所を変える
- `src/app.rs`
  - `Tab.user_title`／`ClosedTab.user_title`、`App::rename_fresh`（まだ何も打っていない）、`App::last_tab_click`（直前のタブクリック）
  - `shown_tab_name(user_title, task)`（描画と入力の初期値で共用）、`tab_display_name`／`begin_rename_tab`／`end_rename_tab`／`set_tab_user_title`
  - `--resume`／`--add` でタブを作るとき `saved_tab_name` で保存した名前を引く
  - タブの並びが変わる所（`move_tab`／`remove_tab`／`reopen_closed_tab`）で `last_tab_click` を捨てる。`remove_tab` は名前の入力も取り消す（前面のタブがずれて別のタブに名前が付くのを防ぐ）
- `src/event.rs`
  - `is_tab_double_click(last, now, hit)`（純粋）と `TabMouseAction::Rename`。`classify_tab_mouse` に「2 回目か」を渡す
  - `RenameOutcome`・`rename_key`・`rename_finish`・`rename_paste`（純粋）。確定の反映は `apply_rename_outcome` 1 つで、Enter と外クリックが共用
  - `handle_mouse` (0.7): 入力中の左 Down は `try_forward_mouse` より先に取る（マウスモードの子へ流さない）。対の Up は `mouse_local_drag` でローカルに消化（コンテキストメニューと同じ型）
  - `process_batch`: 入力中は貼り付けのかたまりを名前の欄へ
  - トレース: `begin_rename_tab idx=N`・`rename_tab idx=N name=Some("…")`／`name=None`
- `src/ui/mod.rs`・`src/ui/theme.rs`: `user_title` をタスク名より先に出す。まだ何も打っていない入力は `tab_rename_selected`（青い帯に白文字）

計画から変えた点: 計画には「入力中に日本語を打つと Claude の入力欄に入る見立て（未実測）」と書いたが、読み違いだった。貼り付け扱いになるのは Enter を含むかたまり（`classify_run`）と `Event::Paste` だけで、IME の確定（Enter なしの複数文字）は 1 文字ずつ名前の欄に入る。直したのは貼り付けの行き先だけ。

### 確認

- `cargo test`: 459 件（新規 17 件: `tab_names` 8 件・`is_tab_double_click_needs_same_tab_within_500ms`・`rename_*` 7 件・`user_title_wins_over_the_task_name`。`classify_tab_mouse_table` は Rename の行を追加）。`cargo fmt --all --check`・`cargo clippy --all-targets -- -D warnings` 警告なし
- E2E `scripts/e2e/dblclick-rename-tab.ps1`: **PASS**（27 項目）。使い捨て conhost に `WriteConsoleInputW` でマウスとキーを入れ、タブバーの文字・トレース・`tab-names.json` を読む
  - ダブルクリック → 入力（今の名前＋カーソル・選択色）→ `pay` + Enter → `2:pay`・2 秒 tick 2 回のあとも同じ・保存 1 件
  - 日本語 2 文字を 1 かたまりで → 名前の欄に入り `2:決済`
  - 何も打たずに Enter → 変わらない／700 ms 空けた 2 回 → 入力は開かない
  - 外（ペイン）をクリック → 確定／入力中にタブを引く → `move_tab` なし／別のタブをクリック → 入力は閉じ、前面は変わらない
  - 文字＋Enter を 1 かたまり（貼り付け扱い）→ 名前の欄に入り、入力は開いたまま
  - Backspace + Enter → 自動の名前に戻り、保存からも消える
  - 中クリックで閉じる → Ctrl+Shift+T → 名前が残る／終了 → `--resume <id>` で開き直す → 同じ名前
- E2E `scripts/e2e/dblclick-rename-tab-wt.ps1`: **PASS**（実物の Windows Terminal・本物のマウス入力）。ダブルクリックの 2 回とも `Mouse(Down(Left)@5,0)` で届き `begin_rename_tab idx=0`。スクショで「名前が青い帯で選ばれた状態」と確定後の `🟨 1:pay` を確認
- 既存 E2E `drag-reorder-tab.ps1` は PASS のまま（19 項目）。`mmb-close-tab.ps1` は SendInput 方式（ゆうさんの操作とぶつかる）なので回していない。中クリックで閉じる動きは新しい E2E の J で見ている

### 分かったこと

- WT はダブルクリックの 2 回目の押下もアプリへ渡す（マウスモードの間は WT 自身の単語選択にならない）
- WT の画面の桁・行とピクセルの対応は、ペインの中を 2 か所クリックして `CCNEST_INPUT_TRACE` に出たセルから割り出せる（タイトルバーの高さやフォントを当て推量しない）。`dblclick-rename-tab-wt.ps1` の probe
- PowerShell の関数が 1 要素の配列を返すと文字列に展開され、`[0]` が先頭の 1 文字になる。`@(関数)` で受ける
- トレースの照合で `rename_tab idx=` は `begin_rename_tab idx=` にも当たる。`[^_]rename_tab` で区別する
- conhost の読み取りでは、入力カーソル `▎`（U+258E）も全角の文字も 1 セル目に文字・2 セル目は属性 0x200。名前とカーソルは「名前の直後にカーソル」で照合する

---

## 2026-11-18 — クリックした所まで選択を伸ばす（Alt+クリック／Shift+クリック・v0.1.44）

ブランチ: `feature/click-extend-selection`

### 背景

ゆうさん「CC の回答をコピーする際に、Shift+マウス左クリックで、元位置からクリック位置全部反転出来たりする？一般的なブラウザ等々でできるやつ」。

それまではできなかった。理由は 2 つ。

- ccnest に機能が無かった。左クリックは毎回そこから新しい選択を始め、引かずに離すと選択は消える
- Windows Terminal は Shift 付きのマウス入力をアプリへ渡さない。WT の `ControlInteractivity::_canSendVTMouseInput` は Shift が押されていると false を返し、WT 自身の選択に回す（設定では変えられない）

そこで **WT の中では Alt+クリック**を合図にした（Ctrl+クリックは URL を開くのに使用中）。Shift を渡す端末（conhost 直など）では Shift+クリックでも同じ動き。

### 動き

- 選択が残っている → クリックが選択の中なら、始点はそのままで終点がクリック位置に動く。選択の外なら、遠い側の端を始点にして選択を丸ごと含むように伸びる（下へ引いた選択の先をクリック＝始点はそのまま。ダブルクリックの行選択から上へ伸ばしても、その行が欠けない）
- 選択が無い → 直前に普通にクリックした位置から、クリック位置まで
- 押したまま引けば終点がついてくる
- 始点が無い／別のペイン／alt 画面の出入り／RIS／ペインの大きさが変わった → 普通のクリックと同じ
- コピーは今までどおり（Ctrl+C・右クリックメニュー）

長い回答は「頭をクリック → ホイールで下へ → 終わりを Alt+クリック」で選べる。始点はバッファの絶対行で控えるので、履歴を動かしても同じ文字を指し続ける。

### 実装

- `src/app.rs`: `ClickAnchor { pane_id, pos, alt, grid_gen, size }` と `App::click_anchor`
- `src/event.rs`
  - `is_extend_click(mods)`: Alt か Shift で、Ctrl なし（AltGr = Ctrl+Alt は対象外）
  - `extended_selection(existing, anchor, pid, click, alt, grid_gen, size)`: 純粋関数。伸ばせないときは None
  - `mouse_forced_local(kind, mods)`: マウスモードのペインで ccnest 側が処理する入力。Shift／Ctrl は今までどおり全部、Alt は左ボタン（押す・引く・離す）だけ
  - 普通のクリックで `click_anchor` を控える所は 2 つ: 通常ペインの新規選択と、マウスモードのペインの保留クリック
  - alt 画面の子へホイールを流すときは `click_anchor` も捨てる（選択と同じ理由。子が中身を動かすので追えない）
  - `fmt_event`: マウスの修飾キーを付いているときだけ出す（`Mouse(Down(Left)@31,7,KeyModifiers(ALT))`）。伸ばしたときは `extend_selection pane=N anchor=.. cursor=..`

計画では「始点はそのまま＋ダブルクリックの行選択から上へのときだけ行末を始点に」だったが、選択の外をクリックしたときは一律に「遠い側の端を始点にする」1 本の決まりにした。ダブルクリックかどうかを覚える状態が要らず、どの向きでも選んだ文字が欠けない。ブラウザ（Windows）と違うのは「上へ引いた選択の下をクリック」「下へ引いた選択の上をクリック」の 2 つだけで、そこはブラウザだと元の選択が外れる。

### 制限

- 全画面表示（alt 画面）のアプリは、ホイールで動かすと選択も始点も消える（今までの選択と同じ）。見えている範囲の中だけ
- WT の中の Shift+クリックは WT 自身の選択のまま

### 確認

- `cargo test`: 442 件（新規 9 件: `extend_*` 6 件・`extend_click_needs_alt_or_shift_without_ctrl`・`alt_left_button_stays_local_in_a_mouse_mode_pane`・`trace_shows_mouse_modifiers_only_when_present`）。`cargo fmt --all --check`・`cargo clippy --all-targets` 警告なし
- E2E `scripts/e2e/click-extend-selection.ps1`: **PASS**（通常ペイン）。使い捨て conhost に `WriteConsoleInputW` で「クリック → Alt+クリック → Shift+クリック → 中を Alt+クリック → 手前を Alt+クリック → クリック」を入れ、反転したセルを読み戻す（20 項目）
- 同 `-MouseMode`: **PASS**（マウスモードのペイン）。押したままのセルが反転しない＝保留クリックの経路に入っていることを先に確かめる
- E2E `scripts/e2e/click-extend-selection-wt.ps1`: **PASS**（実物の Windows Terminal・本物のマウス入力）
  - `Mouse(Down(Left)@31,7,KeyModifiers(ALT))` → `extend_selection pane=1 anchor=(14, 2) cursor=(30, 5)`
  - Shift+クリックのマウス入力は 1 件も届かない
  - スクショで LN09 の途中から LN12 の途中まで反転
- 既存 E2E `url-click-wrap.ps1`（Ctrl+クリック 4/4）・`drag-reorder-tab.ps1` は PASS のまま

### 分かったこと

- ConPTY は、子アプリが VT 入力（stdin に `ENABLE_VIRTUAL_TERMINAL_INPUT` 0x200）を使っているときだけ、マウスモードの要求（`ESC[?1000h` など）を端末（ccnest）へ渡す。偽アプリが出力に書くだけでは ConPTY の中で止まり、ccnest はマウスモードを見ない。偽アプリでマウスモードのペインを作るときは、先に stdin の mode に 0x200 を足す（`click-extend-selection-shim.ps1`）
- WT の窓を起こすクリック（非アクティブな窓への 1 回目）もアプリへ届く
- 本物のマウス入力（`SetCursorPos`＋`mouse_event`＋`keybd_event`）を使うのは WT の実物確認だけ。`GetLastInputInfo` でゆうさんが 4 秒操作していないのを待ち、窓を最前面にして `WindowFromPoint` でクリック先がその窓だと確かめてから入れる。ポインタの位置と前面の窓は戻す
- E2E の出力を `Select-Object -First N` で切ると、スクリプトが finally の前に止められて使い捨ての ccnest が残る（`target\release\ccnest.exe` を掴んだままになり、次の `cargo build --release` が「アクセスが拒否されました」で失敗する）。出力は切らずに全部受ける

---

## 2026-11-18 — 放置中のまとめのあとも、裏の workflow 待ちは 🟢（v0.1.43）

ブランチ: `fix/waiting-line-after-idle-compaction`

### 背景

ゆうさんのスクショ（2026-11-18）: 裏で dynamic workflow（8/9・2h05m）が動いていて、判断待ちは「今は無し」のタブが 🟨 だった。

入力欄の上はこうなっていた（claude v2.1.327）:

```
✻ Waiting for 1 dynamic workflow to finish
  ⎿  Referenced file ..\28_Affilia\CLAUDE.md
  ⎿  Read ..\..\..\.claude\projects\…\memory\project_affilia.md (39 lines)
  ⎿  Plan file referenced (~\.claude\plans\….md)
  ⎿  Skills restored (deep-plan, workflow-authoring)

● Compacted while idle, before the prompt cache expired · turn off with "idleCompaction": false in settings.json
```

放置中のまとめ（idle compaction）は、ターンを始めずに、待ちの行の下へ子の行とお知らせを足す。v0.1.39 の判定は「最後の空でない行が待ちの行」だったので、最後がお知らせになって偽 → フッターに札も無い → NeedsAttention。

あわせて聞かれた「🔵 は廃止したはず」は、その窓が古い版だっただけ（11/15 15:51 起動＝v0.1.38。🔵 をやめた v0.1.40 は 16:53。exe を差し替えても、開いたままの窓は古い版で動き続ける）。

### 判定

`is_waiting_for_background`:

- 見るのは **0 桁目から始まる行**だけ（`starts_at_column_zero`）。Claude Code はターンの区切り（`●`／`✻`／`❯`）を必ず 0 桁目から書き、子の行（`  ⎿ …`）と折り返しの続きは字下げする
- 下から見て、放置中のまとめのお知らせ（`IDLE_COMPACTION_NOTICES`）は読み飛ばす。出だしだけを見る（後ろの `· turn off with …` は狭いペインで折り返す）
  - `Compacted while idle, before the prompt cache expired`
  - `Compacted using a summary written while you were away`
  - 文言は claude v2.1.327 の本体で確認。手で打つ `/compact` の `Conversation compacted` は入れない
- 残った行が待ちの行（完全一致・今までどおり）なら真
- お知らせの下に次のターン（`❯ …`／`● …`／`✻ Worked for …`）があれば偽（今までどおり）

フッターの `/workflows to stop` は目印にしていない（本体では「Large workflow」の警告のときだけ出る文字）。

### 変更点

| ファイル | 内容 |
|---|---|
| `src/pane/status.rs` ver1.10 | `is_waiting_for_background` を上のとおりに。`starts_at_column_zero`・`is_idle_compaction_notice`・`IDLE_COMPACTION_NOTICES`。テスト 7 件（スクショの画面・子の行だけ・折り返し・文言の別形・次のターン・待ちの行なし・判断待ちあり）。既存の字下げつきの待ちの行（実物に無い形）は例から外した |
| `scripts/e2e/idle-compaction-waiting.ps1`・`idle-compaction-shim.ps1`・`fixtures/idle-compaction-step*.txt` | 使い捨て ccnest に偽 claude で同じ画面を出させ、`CCNEST_STATUS_TRACE` の判定を見る（ConPTY → vt100 → `Screen::rows` の通り道）。ps1 は ASCII だけ、画面の文字は UTF-8 の fixtures |
| `scripts/e2e/background-marker.ps1` | claude v2.1.327 に合わせた（下の「本体 2.1.327 での E2E」）。`Replying`（経過秒つきのスピナー行・経過秒の無いスピナー行・フッターの `esc to interrupt`）で「返答が終わった」を見る。スピナー行が無いときに `.Trim()` で落ちていた表示を直した |
| `README.md`・`Cargo.toml`／`Cargo.lock` | 説明・0.1.43 |

### 本体 2.1.327 での E2E

claude v2.1.327 は、返答中のフッターに `esc to interrupt` をまた出す（`⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← 8 agents`）。返答の出だしは経過秒の無いスピナー行（`✶ Bootstrapping…`）だけで、ccnest は `BUSY_MARKERS` で Busy と読む（判定は正しい）。

`background-marker.ps1` は「スピナー行（経過秒つき）が無い＝返答が終わった」と見ていたので 2 か所で外れた:

- Busy の画面に経過秒つきの行が無く、表示用の `.Trim()` が null で落ちた
- 「スピナー行は消えたが `esc to interrupt` は残っている」瞬間を「返答が終わった」と読み、その時点の判定（Busy）を Background と比べて FAIL

ccnest 側の判定は Busy → Background → NeedsAttention と正しく移っていた（2 秒ごとの trace を全部控えて確認）。スクリプトだけ直した。

PowerShell の変数は大文字小文字を区別しない: 直す途中で足した `$spin` が既存の `$SPIN`（正規表現）を上書きし、後ろの待ちが全部外れた。

### 確認

- `cargo test`: 全 442 件（status.rs 63 件。新しい 7 件のうち 5 件が実装前に赤）
- `scripts/e2e/idle-compaction-waiting.ps1`: PASS（待ちの行＋子の行＋お知らせ → Background／お知らせの下に次のターン → NeedsAttention）
- `scripts/e2e/background-marker.ps1`（なし／`-Agent`／`-Decision -SleepSec 150`）: 3 本とも PASS（claude v2.1.327・sonnet）

### 残る制限

- テストと E2E の画面は**スクショからの書き起こし**（本物は workflow を動かしたまま 1 時間ほど放置しないと出ない）。0 桁目・字下げの形が実物と違っていたら効かない → 次に同じ場面で 🟨 だったら `CCNEST_STATUS_TRACE` で実物を取る
- 放置中のまとめ以外のお知らせが待ちの行の下に 0 桁目から出たら、また 🟨（`IDLE_COMPACTION_NOTICES` に足す）
- フッターの下の一覧（`◯ <名前> … 8/9 · 2h05m`）は今回も判定に使っていない

---

## 2026-11-15 — 判断待ちがあるときは、裏で動いていても 🟨（v0.1.42）

ブランチ: `feature/decision-wait-attention`

### 背景

ゆうさんのスクショ（2026-11-15・2 枚目）: 裏でシェル 2 つとエージェントが動いているタブで、返答の最後が

```
▼ ゆうさんの判断待ち
・マーク: A〜F のどれにするか
・文字: 1／2／3 のどれにするか
```

なのに「裏で実行中」の色だった。「ゆうさんの判断待ちがあるときは、オレンジアイコンが良い」。1 枚目のスクショ（`・今はなし`）は裏で実行中のままでよい。

質問文言（"Would you like …"）を探す方式は誤検知が多くて v0.1.31 でやめている。今回は文言ではなく、レイナの報告の型（最後に「ゆうさんの判断待ち」の節・無ければ「今はなし」）を目印にする。

### 判定

`asks_for_decision`（会話の部分＝入力欄の上枠より前、にある**最後の**「判断待ち」の見出しを見る）:

- 見出し: 30 文字以内の行が `判断待ち`（＋コロン）で終わる。箇条書きの行（`・ロゴの反映はゆうさんの判断待ち`）は見出しではない
- 見出しのすぐ下の行（最初の項目）が「なし」の類（`なし`／`無し`／`ありません`／`None`、前に `今は`・`今のところ`・`特に` など）→ 偽。長い「今はなし（…）」が折り返して 2 行になっても、見るのは 1 行目だけ
- 見出しのすぐ下が空行（中身が無い）→ 偽
- 見出しより後ろにユーザーの発話（`❯ …`）がある → 偽（もう答えている）
- それ以外 → 真

`detect_status`: スピナー行 → Busy／（札 または 待ちの行）かつ 判断待ちでない → Background／それ以外 → NeedsAttention。返答中（Busy）は判断待ちの節が画面に残っていても Busy。

### 変更点

| ファイル | 内容 |
|---|---|
| `src/pane/status.rs` ver1.9 | `asks_for_decision`・`is_decision_heading`・`is_none_item`・`DECISION_*`・`BULLETS`。会話の部分の切り出しを `transcript` にして `is_waiting_for_background` と共用。`screen_text`（画面の各行を改行でつなぎ、行末の空白と末尾の空行を落とす）|
| `src/app.rs` ver0.16 | 判定に渡す画面の文字を `Screen::contents()` から `screen_text(Screen::rows(…))` に（下の「行のつながり」）。`CCNEST_STATUS_TRACE` に書く文字も同じもの |
| `scripts/e2e/background-marker.ps1` | `-Decision`（返答の文面は `reply1.txt`／`reply2.txt` から読ませる＝日本語をキー入力で送らない）|
| `README.md`・`Cargo.toml`／`Cargo.lock` | 説明・0.1.42 |

### 行のつながり（実機 E2E で見つけた食い違い）

最初の実装は単体テストが通ったのに、実機（`-Decision`）では Background のままだった。原因は vt100 の `Screen::contents()`: 最終列まで埋まった行（wrapped）のあとに改行を入れない。Claude Code が見出しの行を空白で最終列まで埋めて書いたとき、

```
● ▼ ゆうさんの判断待ち（空白 78 個）・マーク: A か B のどれにするか
```

と 1 行になり、見出しとして読めなかった（同じ実行の 2 つ目の返答は普通に 2 行だった＝つながるかどうかは書かれ方しだい）。v0.1.36 で見つけた「罫線とプロンプトがつながる」（`────…────❯ `）も同じ仕組み。

→ 判定に渡す文字を、行ごとに取って改行でつなぐ形（`Screen::rows` → `screen_text`）に変えた。行の区切りが必ず残る。罫線がつながった形のテストは、頑丈さの確認として残してある。

### 残る制限

- 「判断待ち」の節を使わない返答（文章だけの質問）は今までどおり 🟢 のまま
- 見出しが画面の外（スクロールで上に流れた）だと見えない。節は返答の最後に置くので、ふつうは入力欄のすぐ上にある
- 見出しの言葉は `判断待ち` だけ（`DECISION_HEADINGS`）。型の言い回しを変えるときはここに足す

---

## 2026-11-15 — タブ名を短く・何の話か分かる名前に（v0.1.41）

ブランチ: `feature/short-tab-title`

### 背景

ゆうさんの要望（2026-11-15）「タブの文字短くできる？」「『続きから再開』とかのタイトルになってるから何のことか不明」。

- タブは `N:<名前>` を 20 セルで切っていて、5 枚で画面の 7 割を使っていた
- `resume-p1-affilia-features`（Affilia の P1 の続き）のタブ名が「続きから再開」になっていた。Haiku への入力に「最後の依頼: 続きから再開して」を添えていて、内容の無い依頼文に引っぱられて対象（Affilia P1）が落ちていた

### 変更点

| ファイル | 内容 |
|---|---|
| `src/claude/jp_title.rs` ver0.2 | `SYSTEM_PROMPT` を作り直し: 目安は全角 6 文字（以前は 8）・対象（製品名・固有名詞）を必ず入れる・「再開」「続き」「修正」「確認」など動作だけの名前を禁止・「続きから」「進めて」「まだだめ」のような内容の無い依頼は無視・長いカタカナ語は言い換え。`PROMPT_VERSION`（`p2`）を保存キーの末尾に足し、古い文面で作った名前を作り直させる |
| `src/ui/mod.rs` ver0.20 | `TAB_LABEL_MAX` 20 → 16 セル（`NN:` ＋ 全角 6 文字が切れない幅）|
| `scripts/dev/try_title_prompt.py` | ソースの `SYSTEM_PROMPT` を読み、本体と同じ呼び方（`claude -p --model haiku`・設定やツール無し）でサンプル 12 件を言い換えて幅を出す |
| `scripts/e2e/background-marker.ps1` | `-Agent` でエージェントにさせる仕事を `ping -n <秒> 127.0.0.1` に（`sleep 60` も `Start-Sleep` も、フォアグラウンドの「待つだけのコマンド」としてツールに止められ、エージェントが 4 秒で終わっていた）。待ち中のスクショを判定の直後に撮る。`-Model`（既定 `sonnet`）を `ANTHROPIC_MODEL` で渡す＝使い捨ての claude が本人の既定モデル（Fable）の週の枠を 1 回で約 1% 使っていたため |
| `README.md`・`Cargo.toml`／`Cargo.lock` | 説明・0.1.41 |

### プロンプトの試行（Haiku・同じ 12 件）

| 話題 / 最後の依頼 | 旧プロンプト相当 | 新プロンプト |
|---|---|---|
| `resume-p1-affilia-features` / 続きから再開して | 続きから再開（実物）| Affilia P1 |
| `namegear-dns-change` | ネームギア DNS変更（実物）| ネームギアDNS |
| `move-business-payments-to-gmo` | 決済の移行を GMO …（実物・20 セルで切れ）| 決済GMO移行 |
| `fix-login-redirect-loop` / まだだめです | — | ログイン転送ループ |
| `ccnest-background-agent-blue-marker` / 続き | — | ccnest青マーカー |
| `continue-work` / 続きから再開 | — | 作業の続き（対象が無い話題は訳すだけ）|

字数の指定だけ（「全角 6 文字以内」）では Haiku は守らない（「ログインリダイレクトループ修正」= 30 セルが返った）。収まる例と言い換えの例を書くと 10〜18 セルに収まる。

### 残る制限

- Haiku の出力なので毎回同じとは限らず、18 セル（全角 9 文字）程度は返ることがある。はみ出した分はタブ側が 16 セルで「…」にする
- 古い版の ccnest の窓が開いたままだと、その窓が `jp-titles.json` を自分の内容で書き戻し、新しい名前が消えることがある（次に開いたとき作り直すだけ）
- `/rename` で付けた名前と、日本語の ai-title はそのまま（言い換えない）

---

## 2026-11-15 — 裏で実行中のマーカーを 🔵 → 🟢（v0.1.40）

ブランチ: `feature/background-marker-green`

### 背景

v0.1.39 を入れた直後のゆうさんの指示「やっぱり、青の判定時も緑アイコンにして欲しい」。v0.1.36 で「🟢 に混ぜず別マーカー 🔵」と決めていたのを改め、マーカーは **🟢 実行中（裏で実行中も含む）／🟨 対応が必要** の 2 つに戻す。

### 変更点

| ファイル | 内容 |
|---|---|
| `src/pane/status.rs` ver1.8 | `status_marker(Background)` を 🔵 → 🟢。判定（`ClaudeStatus::Background`・`has_background_work`・`is_waiting_for_background`）と畳み込みの順位はそのまま＝また分けるときは絵文字 1 か所 |
| `src/sidebar/panelist.rs` ver0.6 | サイドバーの行頭も 🟢（`status_marker` に追従）|
| `README.md`・`scripts/e2e/background-marker.ps1` | 説明を 🟢 に |
| `Cargo.toml`／`Cargo.lock` | 0.1.40 |

### 確認

- `cargo test`
- `scripts/e2e/background-marker.ps1 -Agent`: 判定は Background のまま、スクショのタブは 🟢

---

## 2026-11-15 — 裏のエージェント待ちのタブも 🔵（v0.1.39）

ブランチ: `feature/background-agent-wait-marker`

### 背景

ゆうさんのスクショ（2026-11-15）: 返答が `✻ Waiting for 1 background agent to finish` で終わり、裏のエージェント 1 つを待っているタブが 🟨 のままだった。「この画面でも青アイコンになってほしい」。

v0.1.36 の 🔵 はフッターの札（`1 shell`・`1 local agent` など）だけを見ていた。裏のエージェントは札を出さず、フッターの下の一覧（`● main`／`◯ general-purpose … 2s · ↓ 48.6k tokens`）に出る。

### 判定

- 入力欄の上枠より前の、最後の空でない行が `[記号 ]Waiting for <n> background agent(s) [and <n> dynamic workflow(s)] to finish` と完全に一致したら Background（🔵）
- 文言は Claude Code v2.1.325 の本体で確認。ターン終了の行（`✻ Worked for 9s · done 10:09` と同じ場所）の差し替えで、会話に残る文字
- 古い行に当たらない理由: エージェントが終わると通知で Claude が次のターンを始め、行の下に新しい内容が足される（実物でも、完了後の画面の上に古い待ちの行が残っていた）
- 入力欄（上枠と下枠の罫線 2 本）が無い画面は見ない（許可ダイアログは 🟨）

### 変更点

| ファイル | 内容 |
|---|---|
| `src/pane/status.rs` ver1.7 | `is_waiting_for_background`・`is_waiting_line`・`WAITING_NOUNS`。罫線探しを `last_rule`（3 個以上並んだ最後の所の開始・終了位置）に切り出して `has_background_work` と共用。`strip_leading_glyph` を切り出し、`is_count_and_noun` は名詞の表を引数に取る。`detect_status` はスピナー行 → Busy／札 **または** 待ちの行 → Background／それ以外 → NeedsAttention |
| `scripts/e2e/background-marker.ps1` | `-Agent` スイッチ（裏のエージェントで同じ 4 場面）。完了待ちは、経過秒の無いスピナー行（`* Tinkering…`）も待ち、同じ条件が 6 秒あけて 2 回続いたら落ち着いたとみなす。シェル版の `'\b1 shell\b'` に紛れていた制御文字（0x08）を正規表現の `\b` に直した |
| `README.md` | 🔵 の説明に待ちの行を追記・ver0.1.39 |
| `Cargo.toml`／`Cargo.lock` | 0.1.39 |

### 確認

- `cargo test`: status.rs の新旧テスト（実物 2 画面を含む）
- `scripts/e2e/background-marker.ps1 -Agent`: PASS。入力待ち NeedsAttention（🟨）→ 返答中 Busy（🟢）→ 返答が終わって待ちの行 Background（🔵）→ エージェントが終わって NeedsAttention（🟨・古い待ちの行は画面に残っている）。スクショで 🔵 を目視

### 残る制限

- フッターの下の一覧は判定に使っていない（終わったエージェントの行がどう残るかを確かめていない）
- Claude Code の完了行の表示（showTurnDuration）を切っていると待ちの行が出ず、🟨 のまま
- 裏のエージェントを手で止めた直後は、次の発話まで待ちの行が最後に残り 🔵 のまま
- 返答の出だしのスピナー行は経過秒が付く前（`* Tinkering…`）があり、その 1〜2 秒は Busy にならない（以前から）

---

## 2026-11-15 — 引いているタブを浮かせる（v0.1.38）

ブランチ: `feature/tab-drag-float`

### 背景

v0.1.37 でタブをドラッグで並べ替えられるようにした直後の、ゆうさんの要望「もうちょっとアニメーション追加できる？ドラッグしてるときの」「フローティングのアニメーションかな」。v0.1.37 は、ポインタが隣のタブに入った瞬間に位置がぱっと入れ替わるだけだった。

### 動き

- **浮く**: 押した桁から 1 桁でも動くと、引いているタブが定位置を離れてポインタの下についてくる（ほかのタブの上に重ねて描く。定位置は空けたまま）。クリックだけでは浮かない
- **すべる**: 隣を越えると、越えられたタブが空いた定位置へすべる（140 ms・ease-out・1 桁ずつ）
- **収まる**: 離すと、浮いていた位置から定位置へすべって収まる
- 浮いたタブは、見えているタブの帯（先頭タブの左端〜末尾タブの右端）から出ない
- `CCNEST_TAB_ANIM=off`（または `0`）で無効。v0.1.37 と同じ、その場での入れ替えになる

### 変更点

| ファイル | 内容 |
|---|---|
| `src/app.rs` ver0.15 | `tab_drag` を `TabDrag`（位置・押した桁・タブ内のつかんだ位置・今のポインタの桁・浮いているか）に。`Tab::slide: Option<TabSlide>`（定位置からのずれが 140 ms で 0 へ）。`slide_displaced`／`settle_tab`／`has_tab_slides`／`expire_tab_slides`。純粋関数 `slide_offset`（3 乗の ease-out）・`displaced_after_move`・`float_in_strip`。`tab_anim_enabled` |
| `src/event.rs` ver0.21 | Drag で `pointer_x` を更新し、押した桁から動いたら `floating`。`move_tab` のあと `slide_displaced`。Up で `float_in_strip` の位置から `settle_tab`。イベントループは、すべりの途中のタブがあるあいだだけ約 16 ms ごとに描き直す |
| `src/ui/mod.rs` ver0.19 | タブバーを 1 行の Paragraph ではなく、タブ 1 枚ずつ描く（`render_tab_items`／`put_tab_label`）。定位置 + すべりのずれに描き、浮いたタブは最後に重ねる |
| `src/ui/theme.rs` ver0.7 | `tab_floating` |
| `scripts/e2e/drag-reorder-tab.ps1` | 押したまま読む `hold`／`more`／`release`、`-Wt`（Windows Terminal でスクショ）、`-NoAnim` |

### 決めたこと

- **当たり判定は定位置のまま**: `tab_rects` は並び順どおりの定位置を記録し、アニメーション中の表示位置は使わない。入れ替えの判定 `tab_drop_target` も v0.1.37 のまま。見た目だけを足したので、往復しない条件は崩れない
- **仕組みは 1 つ**: 「定位置からのずれが時間で 0 へ近づく」だけ。押しのけられたタブも、離して収まるタブも同じ `TabSlide`。ずれは相対値なので、タブの定位置が変わっても足し引きで続けられる（すべりの途中でまた押しのけられたら、今いる所から続ける）
- **状態は `Tab` に持たせる**: タブに id が無いので、並べ替えでタブと一緒に動く `Tab::slide` にした
- **描画と離したときで同じ計算**: 浮いたタブの桁は純粋関数 `float_in_strip` 1 つで決め、描画と「離したときの収まり始め」が同じ値を使う
- **浮いたタブの色は前面のタブと同じ白帯**: 最初はオレンジの帯にしたが、実機のスクショで状態マーカー 🟨 が背景に溶けて見えなかったので白帯に戻した。色のパターンを増やさない（2026-11-08 のゆうさんの決定）にも合う

### はまった所

- **重ねると絵文字が割れる**: 浮いたタブの左端が、下のタブの絵文字（2 桁）の 2 桁目に当たると、ratatui の差分は 2 桁目のセルを飛ばすので、重ねたラベルの 1 桁目が出ない。重ねる前に、左隣のセルが 2 桁の文字なら空白に戻す（`put_tab_label`・テスト `tab_items_overlay_blanks_a_split_wide_char`）
- **E2E の桁の数え間違い**: `bravo` の終わりを名前 5 文字ぶんで数えていて、越えたつもりが越えていなかった（ccnest 側の不具合ではない）。cc のタブの位置から数え直した
- conhost は `…` を全角として描くので、conhost 越しに読んだタブバーは `…` のあとが 1 桁ずれる（v0.1.37 の E2E でも同じ）。Windows Terminal のスクショではずれない

### 検証

- `cargo test`: 単体 403 件（今回の追加 8 件）
- 実機 E2E `scripts/e2e/drag-reorder-tab.ps1`（3 通りとも PASS）
  - conhost: E（押したまま 6 桁右 → `:alpha` の桁が 5 → 11・並びは動かない → 離すと 5 に戻る・`move_tab` なし）＋ v0.1.37 の A〜D
  - `-NoAnim`: 押したまま引いても桁は 5 のまま（浮かない）。A〜D は同じ結果
  - `-Wt`: Windows Terminal の窓で、浮いた直後／隣を越えた後／離した後の 3 枚を PrintWindow で撮って目で確認

---

## 2026-11-15 — タブをドラッグで並べ替える（v0.1.37）

ブランチ: `feature/tab-drag-reorder`

### 背景

ゆうさんの要望（2026-11-15）「ccnest のタブってドラッグで順番変えるようにできる？」。タブの並びは開いた順のままで、変えるには閉じて開き直すしかなかった。

左ボタンの Down／Drag／Up はテキスト選択用にすでに届いていて、タブの当たり判定（`tab_rects`）もある。タブの実体は `App.tabs: Vec<Tab>` と `active_tab: usize` だけで、並びを変えても壊れる参照が無い（ペインは `PaneId` で引く・タブ番号は描くときに振り直す）。足りなかったのは、タブバーでの Drag を拾って入れ替える処理だけ。

### 仕様

- タブを左ボタンで押す → 今までどおりそのタブに切り替わる。そのまま左右に引くと、引いている最中にタブがついてくる（ブラウザと同じ。ドロップ位置の印は出さない）。離すと確定
- 引いているタブは常に前面のタブ（色で分かる）
- 見るのはポインタの横位置だけ。ペインの上まで下がっても並べ替えは続く
- 見えているタブの左端より左／右端より右まで引くと、見えている範囲の先頭／末尾へ
- リネーム中は始めない
- 対象外: 画面の右に収まらず描かれていないタブへは引けない（タブバーにスクロールが無い）

### 変更点

| ファイル | 内容 |
|---|---|
| `src/app.rs` ver0.14 | `tab_drag: Option<usize>`（引いているタブの今の位置）・`move_tab(from, to)`・純粋関数 `move_in_vec`／`active_after_move`（前面のタブは同じタブを指し続ける）。`remove_tab`／`reopen_closed_tab` は位置がずれるので `tab_drag` を消す |
| `src/event.rs` ver0.20 | タブ上の Down(Left) で `tab_drag` を立てる。立っている間の Drag(Left) は `try_forward_mouse` より前で取り、純粋関数 `tab_drop_target` が動かす先を決める。動かしたら `tab_rects.clear()`。Up(Left) で終わり。`CCNEST_INPUT_TRACE` に `move_tab from=A to=B` |
| `scripts/e2e/drag-reorder-tab.ps1` | 新規。使い捨て conhost に `WriteConsoleInputW` でマウスを入れる |
| `README.md`・`Cargo.toml` | キー表と Tabs 節・0.1.37 |

### 動かす条件（`tab_drop_target`）

2 つとも満たしたときだけ動かす。

1. ポインタが動かす先のタブに入っている（自分の上・区切りの上では動かさない）
2. 動かしたあと、引いているタブがポインタの下に来る

式にすると、右へは `mx >= x_j + max(0, w_j - w)`、左へは `mx < x_j + min(w_j, w)`（`w` は引いているタブの幅）。

- 1 だけだと: 狭いタブを広いタブの向こうへ引いたとき、入れ替えた直後もポインタが広いタブの上に残り、次の Drag で元へ戻る（往復してちらつく）
- 2 だけだと: 広いタブの真ん中を押しただけで、両隣の狭いタブに対して条件が成り立ち、動かしていないのに入れ替わる（設計中に気づいてテスト `tab_drop_target_wide_over_narrow_does_not_oscillate` にした）

### 検証

- `cargo test`: 単体 395 件（うち今回の追加 9 件）
- 実機 E2E `scripts/e2e/drag-reorder-tab.ps1`: 幅の違う 3 タブ（alpha／bravo-long-tab-name／cc）で 4 場面
  - A. alpha を 1 桁ずつ右端まで引く → bravo, cc, alpha。`move_tab` は `from=0 to=1`／`from=1 to=2` の 2 行だけ（往復なし）
  - B. alpha を 1 回の Drag で左端まで → alpha, bravo, cc。`from=2 to=0`
  - C. bravo を押してペインの行（6 行目）まで下げ、その行のまま右端まで → alpha, cc, bravo
  - D. クリックだけ → 並びは変わらない・`move_tab` なし・押したタブが前面
- 前面のタブは、タブバーのセル属性が 1 枚だけ違うことで確かめた

---

## 2026-11-14 — 裏で実行中のタブに 🔵（v0.1.36）

ブランチ: `feature/background-marker`

### 背景

返答が終わるとスピナー行（`… (2m 24s`）が消えるので、裏でシェル（レンダーなど）やエージェントが動いていてもタブは 🟨（対応が必要）になっていた。実際には何もしなくてよい（終われば Claude に通知が届いて自分で続きを始める）ので、🟨 の意味が薄まっていた。

ゆうさんの決定（2026-11-14）: 🟢 に混ぜず **🔵 の別マーカー**にする。裏に dev サーバーを置いたままのタブが「実行中」と見分けられなくなるため。マーカーは 🟢 実行中／🔵 裏で実行中／🟨 対応が必要 の 3 つ。

### 調べたこと（Claude Code v2.1.325 の本体）

- 裏で動いているものは、入力欄の下のフッターに札で出る: `  ⏵⏵ bypass permissions on · 1 shell · ← 8 agents          /rc`
- 札の文言は本体の 1 つの関数が作る: `1 shell`／`N shells`、`1 monitor`、`1 shell, 1 monitor`、`1 local agent`、`1 team`、`1 background dynamic workflow`、`1 MCP task`、`<記号> 1 cloud session`、`<記号> 1 remote dynamic workflow`、種類が混ざると `N background tasks`
- 完了行 `✻ Sautéed for 3s · done 18:08 · 1 shell still running` にも同じ文言が出るが、こちらは会話に残る文字。判定には使わない
- `← 8 agents` は札ではない（エージェント一覧への案内）

### 変更点

| ファイル | 内容 |
|---|---|
| `src/pane/status.rs` ver1.6 | `ClaudeStatus::Background`・`has_background_work`（最後の罫線 `─`×3 以上より後ろ＝フッターを ` · ` と空白 2 個以上で区切り、1 つが札の文言と完全一致）・`detect_status` はスピナー行 → Busy／札 → Background／どちらも無い → NeedsAttention・`aggregate_status` は NeedsAttention > Busy > Background > Idle・`status_marker` に 🔵 |
| `src/app.rs` ver0.13 | 開発用 `CCNEST_STATUS_TRACE=<path>`: 2 秒 tick ごとに、Claude ペインの判定結果と判定に使った画面の文字を書く |
| `src/sidebar/panelist.rs` ver0.5・`src/ui/mod.rs` ver0.18 | 表示は `status_marker` に追従（コードはコメントとテストだけ） |
| `scripts/e2e/background-marker.ps1` | 新規。WT の新しい窓に使い捨て ccnest → 本物の claude に裏で sleep を走らせる → 4 場面の判定とスクショ |

🔵 にしないもの: `1 Artifact comment monitor`（アーティファクト公開後ずっと出る見張り）、`dreaming`／`saving memories`／`memory import`／`auto-mode scan`（本体の後片付け）。

### はまった所

- **罫線の行で切れない**: vt100 の `contents()` は幅いっぱいの行を次の行と改行なしでつなぐことがある（実測 `────…────❯ `）。「罫線だけの行」を探すと外れるので、最後の `─` の位置で切る
- **テスト文字列は実物から**: v0.1.31〜33 は conhost 越しに読んだ（空白が潰れた）文字列でテストを書いて 3 回外した。今回は `CCNEST_STATUS_TRACE` で vt100 の文字をそのまま取り、`REAL_BACKGROUND_SCREEN`／`REAL_DONE_SCREEN` としてテストに入れた
- **WT は環境変数を引き継がないことがある**: E2E は窓の中で `run.cmd` を走らせて環境変数を決める。既定は plan モードで裏のコマンドを走らせないので `CCNEST_CLAUDE_PERMISSION_MODE=off`
- **PrintWindow が真っ黒（約 2 KB）を返すことがある**: 窓が最小化されたとき。`ShowWindow(SW_SHOWNOACTIVATE)` で戻して撮り直す

### 確認

- `cargo fmt --all`・`cargo clippy --all-targets -- -D warnings`・`cargo test --all`（lib 387 件）
- `scripts/e2e/background-marker.ps1`: PASS。入力待ち NeedsAttention（🟨）→ 返答中 Busy（🟢）→ 返答が終わって `1 shell` の札 Background（🔵）→ 裏の sleep が終わって NeedsAttention（🟨）。スクショで 🔵 と 🟨 を目視

### 残る制限

- 裏が動いている間に Claude が文章で質問して返答を終えた場合も 🔵（画面からは区別できない）。許可ダイアログは入力欄ごとフッターが消えるので 🟨
- ペインが狭くてフッターの札が切れると 🟨（今までどおりの側に倒れる）
- 札の文言が Claude Code の版で変わると 🟨 に戻る。そのときは `BACKGROUND_NOUNS` に足す

---

## 2026-11-14 — npm（ccnest-cli）の公開を Trusted Publishing（OIDC）に切り替え

ブランチ: `claude/npm-trusted-publishing`（11/11 に別セッションが作成）→ `fix/npm-trusted-publishing` として main にマージ

### 背景

npm の `ccnest-cli` が 0.1.3（2026-04-27）のまま止まっていた。GitHub の secret `NPM_TOKEN`（2026-04-26 登録）が期限切れで、v0.1.22／24／30／35 のタグ push は 4 回とも `publish-npm` だけ `E404 Not Found - PUT https://registry.npmjs.org/ccnest-cli` で落ちていた（npm は認証失敗を 404 で返す）。公式サイトの導入手順は `npm install -g ccnest-cli` なので、そこから入れた人には 7 か月前の版が入っていた。

トークンを作り直してもまた期限で切れる。npm 側も「2FA を迂回するトークンは制限していく」と告知しているので、トークンをやめて OIDC にした。

### 変更点（`.github/workflows/release.yml` だけ。Rust のコードと version は変えていない）

| 箇所 | 内容 |
|---|---|
| `publish-npm` の権限 | `permissions: id-token: write`・`contents: read` を追加。`NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}` を削除 |
| Node | 20 → 24。続けて `npm install -g npm@latest`（Trusted Publishing は npm 11.5.1 以上・Node 22.14 以上が要る） |
| 入力 `npm_publish_tag` | `workflow_dispatch` に追加。Release がすでにあるタグを指定すると、ビルドせず npm だけ出し直す。失敗した run の再実行はタグ時点の古い workflow で動くので、main の workflow から出し直す道が要った |
| `publish-npm` の条件 | タグ push で `checksums` が成功、または `npm_publish_tag` 指定の手動実行（`!cancelled() && (…)`） |
| checkout | `RELEASE_TAG`（入力のタグ、無ければ push されたタグ）を checkout するので、`npm/` の中身はそのタグのもの |
| Release の確認 | publish の前に `releases/download/<tag>/` の `ccnest-windows-x64.exe` と `checksums.txt` があるか確かめる（postinstall が同じ版の Release から exe を落とすため。Release の無い版を npm に出さない） |
| `build` | `npm_publish_tag` 指定のときは回さない |

### npm 側の設定（npmjs.com → ccnest-cli → Settings → Trusted Publisher）

- Publisher: GitHub Actions ／ `mitama987` / `ccnest` ／ Workflow filename: `release.yml` ／ Environment: なし
- Allowed actions: `npm publish` を許可（`npm stage publish` は常に許可）。`npm dist-tag` は不許可
- この接続の必須項目はあとから変えられない（変えるときは消して作り直す）
- 登録にはアカウントの 2 段階認証が要る（このとき有効にした）

### 使い方

- ふだん: これまでどおり `vX.Y.Z` のタグを push すれば、Release と npm の両方が出る
- 既存タグを npm にだけ出し直す: `gh workflow run release.yml -R mitama987/ccnest --ref main -f npm_publish_tag=vX.Y.Z`

### 確認

- 登録前の試し実行（run 43570426250）: Release の確認・`npm version 0.1.35` までは通り、publish だけ E404（未登録なので想定どおり）
- 登録後（run 43571389832）: success。`+ ccnest-cli@0.1.35`、provenance 付き（sigstore logIndex 3872792419）。node v24.22.2・npm 12.2.2
- レジストリへの反映は publish の約 1 分半後（直後の `npm view` はまだ 0.1.3 を返す。「Your package is being processed」と出る）
- `npm view ccnest-cli version` → `0.1.35`
- 実際に入れて確認（PATH の `ccnest` には触らず、一時フォルダへ `npm install ccnest-cli@latest --prefix <一時フォルダ>`）: `Downloading ccnest v0.1.35 for win32-x64...` → `Checksum verified.` → `ccnest v0.1.35 installed.`。落ちてきた `bin/ccnest.exe` は 5,979,648 バイトで、SHA-256 `c013bd11…23ed8` が Release の `ccnest-windows-x64.exe.sha256` と一致

### 残り

- タグ push からの自動公開（`build` → `checksums` → `publish-npm` の順）は、次のタグ（v0.1.36 以降）で初めて通る。今回確かめたのは手動の出し直しの道
- secret `NPM_TOKEN` は使わなくなったので、同日に削除した（`gh secret delete NPM_TOKEN`。リポジトリの secret は 0 件）

---

## 2026-11-11 — 動いている窓にタブを足す `ccnest --add`（v0.1.35）

ブランチ: `feature/add-tab-remote`

### 背景

ゆうさん「対応必要な 4 本のセッションをこの ccnest 内で新タブで追加できたりする？」。v0.1.34 までの `--resume` は起動時にしか読まず、動いている窓へ外から「このセッションを開いて」を届ける口が無かった（Ctrl+T → `/resume` を人が打つしかない）。レイナ（タブの中の Claude）が自分でタブを足せるようにする。

### 仕組み

パイプやソケットは使わず、依頼ファイルの受け渡し。既存の「`pending_resumes` を 2 秒 tick で 1 タブずつ開く」流れに積むだけなので、スレッドも依存も増えない。

1. 窓側は起動時に `%APPDATA%\ccnest\requests\<自分の pid>\` を作る（ついでに、動いていない pid のフォルダを消す）。App と一緒に drop されて消える
2. 送る側 `ccnest --add [--to <pid>] --resume <uuid>[=<タブ名>]...` は raw mode に入らず、依頼を `.json.tmp` → rename で置き、`.done` を最大 10 秒待つ
3. 窓側は 2 秒 tick で `*.json` を拾い、検査して `pending_resumes` に積み、結果を `.done` に書く
4. 宛先は `--to <pid>` → 環境変数 `CCNEST_PID`。ccnest がペインの子へ `CCNEST_PID=<自分の pid>` を渡すので、タブの中から打てば「この窓」に届く
5. 開かずに理由を返すもの: その窓ですでに開いている／開き待ち（`already_open`）、その窓のフォルダに会話ログが無い（`no_session_log`＝開いてもすぐ終わる）
6. 終了コード: 頼んだセッションがすべてその窓のタブにある（開いた／開いていた）なら 0、それ以外は 1

### 変更点

| ファイル | 内容 |
|---|---|
| src/remote.rs | 新規。`Inbox`（pid ごとの依頼フォルダ・起動時に死んだ pid を片付け・Drop で削除）、`take_requests`／`plan_requests`／`write_reply`（窓側）、`resolve_target`／`send`／`format_reply`（送る側）、`pid_alive`（Windows は OpenProcess＋WaitForSingleObject）。テスト 19 件 |
| src/cli.rs | `Cli.add`・`Cli.to`、`--add`／`--to <pid>` の解析（`--add` で `--resume` 0 件・フォルダ指定・`--to` だけ、はエラー）。`ResumeSpec` に Serialize／Deserialize。テスト 3 件 |
| src/main.rs | `cli.add` なら raw mode の前に `run_add` → 結果表示 → 終了コード |
| src/app.rs | `App.inbox`、`accept_remote_requests()`（シェルに切り戻したペインは「開いている」に数えない） |
| src/event.rs | 2 秒 tick で `spawn_next_resume()` の前に `accept_remote_requests()` |
| src/claude/launcher.rs | `apply_env` が `CCNEST_PID` を自分の pid で上書き。テスト 1 件 |
| scripts/e2e/add-tab.ps1 | 新規。使い捨て ccnest（conhost・偽 claude）へ別プロセスの `ccnest --add` を送る |
| README.md / Cargo.toml | 「Add tabs to a running window」の節・0.1.35 |

### 確認

`cargo fmt --all -- --check`・`cargo clippy --all-targets -- -D warnings`・`cargo test`（lib 375 件 PASS・353 → 375）。`scripts/e2e/add-tab.ps1` PASS（18 項目）: 2 本送る → 終了コード 0・偽 claude が `--resume <id>` で 2 つ起動・タブバーに `first-tab` と `追加タブ`・表示中のタブは 1 のまま・3 ペインとも `CCNEST_PID` が窓の pid ／ 同じセッションをもう一度 → 「すでに開いています」で増えない（終了コード 0）／ 会話ログの無いセッション → 「会話ログがありません」で開かない（終了コード 1）／ 動いていない pid 宛て → 1 秒でエラー（終了コード 1）／ 依頼フォルダは空に戻る。

### 知っておくこと

- **動いている窓は旧 exe のまま**。この機能は v0.1.35 で起動した窓にしか届かない（旧版の窓には依頼フォルダが無く、送る側はすぐエラーになる）
- タブ名（`=<タブ名>`）は、Claude がタスク名（`/rename`・日本語タブ名・端末タイトル・最後の発話）を出すまでの表示。起動時の `--resume` と同じ
- セッションは窓の起動フォルダで再開する。別フォルダ（別リポジトリ）のセッションは `no_session_log` で断る
- 別の ccnest やターミナルで開いているセッションは検出しない（開くとそのタブがすぐ終わるだけで、会話は壊れない）
- E2E の偽 claude は `title claude` を出す（cmd 既定のタイトル `C:\WINDOWS\system32\cmd.exe` はタスク名として扱われ、タブ名を上書きするため）

---

## 2026-11-09 — 実行中判定がスピナー行を拾えていなかった修正（v0.1.34）

ブランチ: `fix/spinner-timer-minutes`

### 背景

ゆうさん「ccnest で CC が実行中も結局オレンジ四角なんだが」「そもそも、最初の 60 秒も緑になってないが」。スクショのスピナー行は `✢ Metamorphosing… (2m 24s · ↓ 4.4k tokens · thinking with high effort)`。v0.1.31 の `has_spinner_timer` は `…(` が直結し、かつ経過時間が秒だけ（`<n>s`）の形しか見ていなかった。実物は `…` と `(` の間に空白があり、60 秒を超えると `2m 24s`、1 時間超は `1h 2m 3s` になるので、一度も Busy にならず全タブ 🟨 のままだった（v0.1.31 の単体テストは conhost 読み取りで空白が落ちた文字列を正としていた）。Claude Code は昨日と同じ 2.1.319。

### 変更点

| ファイル | 内容 |
|---|---|
| src/pane/status.rs | `has_spinner_timer`: `…` の後ろの空白を読み飛ばして `(`、続く `elapsed_time_prefix`（`<n>h `? `<n>m `? `<n>s` → ` ` か `)`）。テスト `spinner_timer_with_minutes_and_space_is_busy` 追加・不一致側に `Worked for 2m 24s` / `…(2m)` / `…(2m24s)` 等を追加 |
| scripts/e2e/status-marker.ps1 | `$SPIN` を `… *\((\d+h )?(\d+m )?\d+s( \|\))` に・コメント |
| scripts/e2e/status-marker-wt.ps1 | 新規。使い捨て ccnest を WT の新しい窓で起こし、入力待ち／実行中／終了後の 3 枚を PrintWindow で撮る（conhost では絵文字が読めないため・隠れていても撮れる・起動フォルダは信頼済みリポ配下 target/） |
| README.md / Cargo.toml | 🟢 の説明に実物の例・0.1.34 |

### 確認

`cargo test` 353 件 PASS・`status-marker.ps1` PASS・`status-marker-wt.ps1` のスクショで実行中 🟢（`✶ Spelunking… (7s · ↓ 363 tokens · thought for 3s)`）→ 終了後 🟨 を目視。ConPTY（WT）経由の画面では `…` と `(` の間の空白がそのまま見え、conhost 直では `…(3s` と潰れる（conhost が `…` を全角扱い）＝ v0.1.31 のテスト文字列はこの潰れた形を写していた。

---

## 2026-11-09 — 「対応が必要」のマーカーを 🟨 に戻す（v0.1.33）

ブランチ: `fix/attention-marker-yellow`

### 背景

ゆうさん「ccnest の四角のオレンジは前のオレンジの色にして」。v0.1.31 で「対応が必要」を 🟧（U+1F7E7）にしたが、ゆうさんが「オレンジ」と呼んでいたのは v0.1.30 までの 🟨（U+1F7E8・Segoe UI Emoji では橙寄りの黄に見える）。絵文字の色はフォントが決めるので、ccnest 側は絵文字を差し替えるだけ。意味（質問も作業完了も＝実行中でなければ全部）と判定は変えない。幅はどちらも 2 でレイアウト不変。

### 変更点

| ファイル | 内容 |
|---|---|
| src/pane/status.rs | `status_marker`: `NeedsAttention => "🟨"`。テスト `marker_attention_is_yellow_square`・幅テスト |
| src/sidebar/panelist.rs | Panes 行のマーカー期待値を 🟨 に |
| src/ui/mod.rs | `tab_label_marked_width_bound` のマーカーを 🟨 に |
| scripts/e2e/status-marker.ps1 | コメントのみ（判定はセル属性で幅 2 を見るので不変） |
| README.md / Cargo.toml | 説明を 🟨 に・0.1.33 |

---

## 2026-11-08 — Shift+Enter で Claude の入力欄に改行（v0.1.32）

ブランチ: `feature/shift-enter-newline`

### 背景

ゆうさん「Claude Code のフォーム入力欄で Shift+Enter で改行できるようにできますか？」。ccnest は `Shift+Enter`／`Ctrl+Enter` の KEY_EVENT を `ESC CR`（Claude Code が改行として受ける列）に変換する実装を持っていた（README にも記載）。一方 Windows Terminal の settings.json には `shift+enter → sendInput "\u001b\r"`（Claude Code の `/terminal-setup` 相当・素の WT タブで効かせるための設定）が入っており、WT が先に Shift+Enter を横取りして文字列 `ESC CR` を ConPTY に流していた。

**実測**（`scripts/e2e/conpty-input-probe.py`・pywinpty の ConPTY に `CCNEST_INPUT_TRACE=1` の使い捨て ccnest を起こし、入力パイプに文字列を書く）:

| 入力パイプに書いた文字列 | ccnest（crossterm）が受けたイベント |
|---|---|
| `"\x1b\r"` を 1 回の write（WT の sendInput と同じ） | `Key(Enter, Press, ALT)` |
| `"\x1b"` と `"\r"` を別 write | `Key(Esc)` → `Key(Enter)` |
| `"\r"` | `Key(Enter)` |

つまり実運用の経路では ccnest に **Alt+Enter** として届き、`key_to_bytes` が Alt を見ていなかったため素の `\r`（送信）になっていた。

### 変更点

| ファイル | 内容 |
|---|---|
| src/event.rs | `key_to_bytes`: `KeyCode::Enter` で `shift || ctrl` → `shift || ctrl || alt` のとき `ESC CR`。テスト `key_to_bytes_enter_modifiers_insert_newline` |
| scripts/e2e/conpty-input-probe.py | 新規。ConPTY 入力の届き方を実測（`uv run --with pywinpty`） |
| scripts/e2e/shift-enter-newline.ps1 | 新規。使い捨て conhost ＋ 本物の claude に Shift／Alt／Ctrl 付き Enter を注入し、入力欄が 2 行になり送信されないことを確認 |
| README.md / Cargo.toml | キー表に Alt+Enter と WT の注記・ver0.1.32 |

Esc→Enter の 2 キーを束ねる案（計画の手順 2）は、実測で Alt+Enter として届くと分かったので実装していない。WT の `User.SendNewline` は残す（ccnest を使わない WT タブでも Claude Code の Shift+Enter が効く）。

### 検証

- `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test`
- `cargo test`: 352 passed（新規 `key_to_bytes_enter_modifiers_insert_newline` を含む）・clippy 警告 0
- E2E `scripts/e2e/shift-enter-newline.ps1`: **PASS**。Shift+Enter／Alt+Enter（WT の経路）／Ctrl+Enter のそれぞれで、`abc636` と `def405` のように前後に打った文字が入力欄の別の行に入り、2 秒待ってもスピナー行が出ない（送信されない）。ccnest プロセス before=1 after=1
- 残る未確認: WT の keybinding → ConPTY → ccnest の実運用経路は WT への実キー入力でしか発火しないため、ゆうさんに新しい ccnest 窓で Shift+Enter を 1 回押してもらう

## 2026-11-08 — タブの状態表示を 2 色に（v0.1.31）

ブランチ: `feature/two-color-status`

### 背景

ゆうさんの指摘「色が多すぎて把握できない。実行中＝緑、私の対応が必要＝オレンジの 2 種類でいい。実行してるのにオレンジが多い」。それまでは 4 状態（Busy／Idle／NeedsAttention／DoneUnseen）× 2 表示（タブ先頭の四角 🟩🟨🟪 ＋ タブ名の文字色 緑／黄／マゼンタ）に、フォーカス枠のオレンジが重なっていた。

**「実行中なのにオレンジ」の根本原因（E2E で判明）**: Claude Code 2.1.319 のスピナー行は `✶ Boondoggling…(6s · ↓302 tokens · thought for 1s)` で、v2.1.227 当時の `(esc to interrupt)` が消えている（本体に残る 6 箇所は再試行待ち・remote 送信の通知だけ）。ccnest はこの文言だけで Busy を判定していたので、実行中が一度も 🟩 にならず常に 🟨（黄の四角＝オレンジに見える）だった。質問判定（`Would you like` が本文に残るだけで黄の文字色）も重なっていた。

決定: 「対応が必要」＝ (1) 質問に答える (2) 作業が終了して次の指示か終了を判断する、の両方。実行中でなければ全部オレンジ。枠線とステータスバーのオレンジは据え置き。状態は四角（丸）だけで出し、文字色は変えない。緑は丸（🟢）、オレンジは四角（🟧）。

### 変更点

| ファイル | 内容 |
|---|---|
| src/pane/status.rs | `ClaudeStatus` を Idle／Busy／NeedsAttention の 3 値に（`DoneUnseen` 削除）。**`has_spinner_timer`**: `…(` の直後に `<数字>s` が続くスピナー行を実行中とみなす（完了行 `Worked for 9s · done` やタイマー無しの `Brewing…` は不一致）。`BUSY_MARKERS = ["esc to interrupt", "enter to interrupt"]` は旧 CLI 向けに残す。`ATTENTION_MARKERS` 削除。`status_marker`: 🟢／🟧。テスト書き換え（実機から取ったスピナー行 5 本・否定例 5 本・幅 2 の確認） |
| src/app.rs | `refresh_pane_state` の `active_leaves`／`tab_active` を削除。`mark_active_tab_seen` と呼び出し 4 箇所を削除 |
| src/event.rs | タブクリック時の `mark_active_tab_seen` 呼び出しを削除 |
| src/ui/mod.rs | `tab_label_style` とテスト 2 件を削除。タブ名は `base` のまま。Panes 行は `Span::raw`。幅テストのマーカーを 🟧 に |
| src/sidebar/panelist.rs | `PaneRow::display()` が Claude ペインの行頭に 🟢／🟧 を付ける。テスト 2 件追加 |
| src/ui/theme.rs | `status_busy`／`status_attention`／`status_done` を削除 |
| README.md / Cargo.toml | マーカー表を 2 行に・Panes の説明・ver0.1.31 |
| scripts/e2e/status-marker.ps1 | 新規。使い捨て conhost に本物の claude を起こし、WriteConsoleInputW で指示を入れて、入力待ち→実行中（スピナー行 `…(Ns`）→完了 の 3 段階でタブ行のマーカーを確認 |

### 検証

- `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings`（警告 0）→ `cargo test`（351 passed）
- 実機 `scripts/e2e/status-marker.ps1`: PASS（入力待ち・実行中 `✶ Zesting…(2s · thinking)`・完了 の 3 段階でタブ行先頭に幅 2 のマーカーが 1 つ＝セル属性 LEADING 0x100／TRAILING 0x200、はみ出し無し。ccnest プロセス before=1 after=1）
- **制約**: conhost の `ReadConsoleOutputW`／`ReadConsoleOutputCharacterW` は非 BMP 文字（🟢 U+1F7E2・🟧 U+1F7E7）を U+FFFD に潰して返すので、どちらの絵文字かはこの経路では読めない。🟢／🟧 と状態の対応は `status.rs` の単体テストで固定（旧 🟩🟨🟪 も同じ制約だった）
- このPCの `ccnest` コマンドを差し替え（ハッシュ一致・次回起動から）。タグ push はしていない

## 2026-11-03 — work フォルダ番号振り直しに伴うパス参照の更新（テスト期待値含む）

- `src/ui/mod.rs` のテスト: cwd の例を `90_other\ccnest`→`80_ccnest`・`30_XTP3`→`20_XTP3` に。期待値のフォルダ名 3 箇所（`[1] 30_XTP3`）も `20_XTP3` に合わせた（機械置換はパスだけだったので落ちていた）
- `examples/model_probe.rs`・`scripts/capture-statusbar.ps1`・`scripts/e2e/mmb-close-tab.ps1`・`scripts/latency/measure-typing-latency.ps1`・この文書: 絶対パスを新名に。対応表は `work\README.md`
- 検証: cargo fmt / clippy / test（356 passed）

## 2026-10-31 — Ctrl+Shift+T で直前に閉じたタブを開き直す（v0.1.29）

ブランチ: `feature/reopen-closed-tab`

### 背景

ゆうさんの要望「Ctrl+Shift+T で直前に消したタブを開けるようにしたい」（ブラウザと同じ）。`--resume` で Claude のセッションを開き直す仕組みは v0.1.23 からある。Windows Terminal は既定で Ctrl+Shift+T を自分の「新しいタブ」に使っているので、WT 側の割り当てを外す（ゆうさん決定）。

### 変更点

| ファイル | 内容 |
|---|---|
| src/keymap.rs | `Action::ReopenClosedTab`。Ctrl+Shift+`t`/`T` を Ctrl+T（新しいタブ）より先に判定 |
| src/pane/grid.rs | `Layout::remap`: 分割の向き・比率を保ったまま葉の ID を付け替える |
| src/app.rs | `ClosedTab` / `ClosedPane` と `closed_tabs`（上限 20）。`close_tab` と、`close_focused_pane` でタブが空になるときに控えを取る（`snapshot_tab`。会話ログがあれば `resumable`）。`reopen_closed_tab`: 元の位置に同じ分割で開き直して前面へ。ログがあるペインは `--resume`、無ければ新しいセッション。途中で起動に失敗したら作ったペインを閉じて控えを戻す |
| src/event.rs | `ReopenClosedTab` の処理。閉じたタブが無ければステータス欄に「開き直せる閉じたタブはありません」 |
| scripts/e2e/reopen-tab.ps1 | 使い捨て conhost に WriteConsoleInputW で Ctrl+T → Ctrl+W → Ctrl+Shift+T を入れて確認 |
| README.md / Cargo.toml / Cargo.lock | 0.1.29 |

### 検証

- 追加テスト 5 件（キー割り当て・remap・控えの上限・開き直す位置・Resume/New の選択）。`cargo test --all` 通過
- 実機 E2E `scripts/e2e/reopen-tab.ps1`: タブ 2 を閉じると claude が終わり、Ctrl+Shift+T で `--resume <タブ 2 のセッション>` の claude が起動、もう 1 回押すと「ありません」→ PASS

### 既知の限界

- 分割中のペインを 1 つだけ Ctrl+W で閉じたものは戻せない（タブ単位）
- 最後のタブを閉じると ccnest 自体が終わるので戻せない
- 覚えておくのは ccnest を起動している間だけ（終了したら消える）

---

## 2026-10-31 — 折り返した URL を Ctrl+クリックすると途中までのリンクが開くのを直す（v0.1.28）

ブランチ: `fix/url-click-app-wrap`

### 背景

Claude Code の応答の中の Google ドキュメントの URL が右端で割られ、次行に字下げ 2 桁で続いていた。Ctrl+クリックすると 1 行目の `…kZVKeb8l` までしか開かなかった。コピーは v0.1.19 / v0.1.21 でアプリ折り返しをつなぐようにしていたが、Ctrl+クリックの `url_at_cell_in_screen` は端末のソフトラップ（`row_wrapped`）しか見ていなかった。最終列まで埋まった自動折り返しの形でも、続き行の字下げの空白で URL が切れていた。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/event.rs | 0.14 | `row_join`（`RowJoin::{Soft, SoftDropIndent, App}`）: 行のつながりをコピーと同じ `app_wrapped` / `app_wrapped_after_autowrap` で判定する。`url_at_cell_in_screen` はこれで上下に広げ、アプリ折り返しの境目では前の行の末尾の余白と次の行の字下げを落とす。全角の 2 セル目は飛ばす。URL の終わりは `is_url_char`（RFC 3986 の ASCII 文字）以外の文字（日本語・空白）。`open_url` は `CCNEST_OPEN_URL_LOG=<path>` があればブラウザを開かず URL を追記する（E2E 用） |
| scripts/e2e/url-click-wrap.ps1 | 新規 | 使い捨て conhost の ccnest に WriteConsoleInputW で Ctrl+クリックを入れ、開いた URL を確認する |
| README.md / Cargo.toml / Cargo.lock | 0.1.28 | 履歴・版 |

### 検証

- 追加テスト 5 件（アプリ折り返し・自動折り返し＋字下げ・日本語の手前で終わる・URL の一覧をつながない・短い行をつながない）
- 実機 E2E: `scripts/e2e/url-click-wrap.ps1`

### 既知の限界

- Windows Terminal 自身のホバー表示（吹き出し）は 1 行目までのまま（WT が画面を自分で読んでいるため。直すには OSC 8 を出す必要がある）
- 日本語を含む URL（IRI）は日本語の手前で切れる

---

## 2026-10-27 — 日本語の応答をコピーしたときの改行と字下げを消す（v0.1.27）

ブランチ: `feature/copy-jp-prose-dedent`

### 背景

ccnest のペインで Claude Code の日本語の返答をマウスで選んでコピーすると、(1) Claude Code が端末幅で折り返した位置に改行が入り、(2) 各行の頭に字下げ 2 桁（1 行目は `● `）が付いていた。お客さんへの返信をそのまま貼れない。従来のアプリ折り返し判定（v0.1.19 / v0.1.21）は「行末の語が行頭からの 1 語、またはパス・URL」のときだけつなぐので、`（@sx09s のポスト 20 件を確認）` のように半角の空白を含む日本語の行は拾えなかった。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/event.rs | 0.13 | `continues_wide_prose`: 右端に届いた行 a と次行 b の境目のどちらかが全角文字で、b が a の本文の列（`body_indent`、1 以上）からちょうど始まり、b が箇条書きでなければ、改行も空白も挟まずにつなぐ（CR + カーソル移動の形と、最終列まで埋まった自動折り返しの形の両方）。`AppWrapTraits` に `tail_wide` / `head_wide`。`starts_list_item_without_space`（`・` `※` `①` `1）` `１．` など、空白なしで本文が続く日本語の箇条書きの行頭）を `head_blocks_join` に追加。`dedent_copied_lines`: 空でない行に共通する字下げを落とす（行の途中から選んだときの 1 行目は対象外、行頭から選んだ 1 行目の `● ` は字下げ 2 桁扱い、空行と入れ子の相対字下げは残す） |
| src/pane/pty.rs | 0.3 | `CCNEST_PTY_DUMP=<path>` で全ペインの ConPTY 出力の生バイトを `<path>` に追記（fixture 用）。`1`・空・`true`・`on` は従来どおりエスケープ済みの `%APPDATA%\ccnest\pty-dump.log`。未設定なら OnceLock 判定のみ（`PtyDumpMode`） |
| README.md / Cargo.toml / Cargo.lock | 0.1.27 | 履歴・版 |

既存テストのうち、字下げや `● ` 付きの結果を期待していたもの（パス結合・自動折り返し・採取した実バイト列の再生など 20 件）は、共通の字下げが落ちた形に期待値を更新した（つなぐ／つながない判定は不変）。英文の単語折り返し（`keeps_newline_for_word_wrapped_prose`）は改行が残ることを引き続き確認している。

### 検証

- 追加テスト 14 件（日本語 10 件・PTY ダンプ 4 件）。日本語の応答は CAPTURED_REPLY と同じ出し方（`CSI r;3H` の絶対移動＋最終列まで埋まった直後の自動折り返し）を合成した fixture で再生（文面はダミー）
- `cargo fmt --all`・`cargo clippy --all-targets -- -D warnings` クリーン、`cargo test --all`: lib 343 件＋統合 9 件 pass
- 実機でのマウス選択によるコピーの確認は未実施（ドラッグ選択を自動で動かせないため）

### 既知の限界

- 境目が両側とも半角（例: `sx0` | `9s`）のときは英文と区別できないのでつながない（改行が残る）
- 元の文で境目にあった半角空白（例: `20 件` を `20` | `件` で割った形）は復元できず、詰めてつながる
- 字下げが同じで右端から 2 桁以内に届いた日本語の行が、実は別の行だった場合は誤ってつながる（まれ）
- 選択を `●` の右の空白から始めたなど、行の途中から始めた 1 行目は頭の空白が残る（仕様どおり手を付けない）

---

## 2026-10-25 — ウィンドウタイトルを起動フォルダ名にする（v0.1.26）

ブランチ: `feature/window-title-folder`

### 背景

タスクバーに並ぶ ccnest の窓がすべて起動元 cmd の既定タイトル「C:\Windows\System32\cmd.exe」で、どれがどのリポジトリか分からなかった。ccnest は端末タイトルを一度も設定しておらず、子の Claude Code が出す OSC タイトルは ccnest 内の vt100 がタブ名に使うだけで外の窓には届かない。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/main.rs | 0.3 | 起動時に `SetTitle(folder_title(&cwd))`（OSC 0）を出す。フォルダ名はタブ名と同じ `app::folder_title`。起動フォルダで固定（タブ切替では変えない） |
| src/app.rs | — | `folder_title` の単体テスト（末尾 `\` 付き・ルート）を追加 |
| README.md / Cargo.toml | 0.1.26 | 履歴 |

### 検証

- `cargo test`: 329 件 pass／`cargo fmt`・`cargo clippy --all-targets -D warnings` クリーン
- 実機: 使い捨て ccnest を `conhost.exe` で `26_XTP3_titletest` フォルダ指定・cwd=`C:\Windows\System32` で起動 → `Get-Process` の `MainWindowTitle=[26_XTP3_titletest]`、窓のスクショのタイトルバーも同じ。ユーザーの ccnest（3 プロセス）は前後で 3 のまま
- Windows Terminal の settings.json に `suppressApplicationTitle` は無い（アプリ側のタイトルが効く）

---

## 2026-10-24 — タブ名を日本語の短い名前にする（v0.1.25）

ブランチ: `feature/jp-tab-title`

### 背景

タブ名（`N:<task>`）は Claude Code の端末タイトル（OSC）を優先していたが、これは英語。Claude Code のセッション名は本体埋め込みの英語プロンプト（「Generate a short kebab-case name (2-4 words)…」）で作られ、会話ログの `ai-title` も直近 15 件中 13 件が `fix-user-name-issues` のような英語の kebab-case。設定では変えられないので ccnest 側で日本語にする。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/claude/jp_title.rs | 0.1（新規） | `JpTitler`: 英語の ai-title を `claude -p --model haiku --effort low --no-session-persistence --strict-mcp-config --tools "" --disable-slash-commands --setting-sources ""` で日本語の短い名詞句に言い換える。専用スレッド 1 本で 1 件ずつ・30 秒で打ち切り・コンソール無し（CREATE_NO_WINDOW）・親セッションの `CLAUDE*`／`AI_AGENT` env は渡さない。結果は `%APPDATA%\ccnest\jp-titles.json`（キー＝sessionId＋ai-title）。日本語の ai-title はそのまま。`CCNEST_JP_TITLE=off` で無効。`pick_task`（優先順位）・`clean_title`（引用符・句読点を落とす・40 字超は捨てる・16 字で切る） |
| src/claude/session.rs | 0.8 | `SessionInfo.ai_title`／`custom_title`（`ai-title`・`custom-title` 行、最後が勝つ）。Claude Code は数ターンごとに書き直すので末尾 64KB の窓で拾える（実測: 175MB のログでも最後の ai-title は末尾 19KB 以内） |
| src/app.rs | 0.8 | タブ名の優先順位 = `/rename` の名前 > 日本語の名前 > OSC タイトル（英語・つなぎ） > 最後の発話。生成の依頼は `--resume` のタブを開き終え、claude 起動から 10 秒後（`~/.claude.json` の同時書き込みを避ける） |
| src/claude/launcher.rs | — | PATH 探索を `claude_on_path()` に切り出し（シムを通さず本物の claude を呼ぶ） |
| README.md / Cargo.toml | 0.1.25 | 説明と履歴 |

### 検証

- `cargo test`: 328 件 pass（新規: jp_title 10 本・session 1 本）／`cargo fmt --check`・`cargo clippy --all-targets -D warnings` クリーン
- Haiku 単体: 入力 768 トークン・1 回 約 $0.0002・2〜3 秒。`~/.claude/projects` にセッションが増えないことを確認。例を入れたプロンプトで `update-x-account-limits` → 「X上限更新」、`landscape-app-preview-banner` → 「アプリ横長バナー化」
- 実機（使い捨て ccnest を conhost で起動 → そのセッション ID の会話ログに `ai-title: fix-login-redirect-loop` の偽ログを置く → 30 秒後にコンソール画面の 1 行目を ReadConsoleOutputCharacter で読む）: タブ行 `1:ログイン転送ルー…`、キャッシュ `ログイン転送ループの修正`。ユーザーの ccnest（3 プロセス）には触れず、偽ログのフォルダは削除
- 罠: 前面化（SetForegroundWindow）はユーザー操作中だと効かずスクショに別の窓が写る → 画面の文字はコンソールバッファから読む（`scripts/e2e/read-console-row.ps1`）

---

## 2026-10-21 — タブを閉じたら子プロセス木ごと終了させる（v0.1.24）

ブランチ: `fix/kill-process-tree-on-close`

### 背景

再起動前の保存スクリプト（`~/.claude/scripts/claude-sessions-snapshot.ps1`）を見直したところ、生きている claude.exe が 28 本あるのに本物の ccnest タブは 9 本だった。残り 19 本（約 5.5GB）は 10/19 の復元後にタブを閉じた分で、親のシム（`ccnest-claude-launcher.exe`）だけが終了し、孫の claude.exe が ConPTY を失ったまま生き残っていた。

原因は `Pane::terminate` → `PtyHandle::kill` → portable-pty `Child::kill` が直下の子を `TerminateProcess` するだけで、子孫には何もしないこと（`portable-pty-0.9.0/src/win/mod.rs`）。ConPTY のハンドルを閉じても node（claude）は終了しない。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| Cargo.toml | 0.1.24 | `windows-sys 0.52`（既に依存木にある版）を `cfg(windows)` 依存に追加（JobObjects / Threading / ToolHelp） |
| src/pane/pty.rs | 0.2 | `tree::ProcessTree`: spawn 直後に子を `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` の Job に入れる。`kill` は `TerminateJobObject` で木ごと終了、Job 化に失敗していれば Toolhelp で子孫を列挙して個別に `TerminateProcess`（`kill_descendants`）。ccnest 自体が終了・落ちてもハンドルが閉じて OS が木を片付ける。非 Windows は従来どおり |

### 検証

- `cargo test`: 新規 3 本（`descendants_in` の純粋関数、ConPTY で cmd→cmd→ping を起こして `kill` で孫まで消える、Job 無しのフォールバック `kill_descendants`）を含め全件 pass
- 旧 kill に一時的に戻して同じテストを回すと `still alive after kill: [16320, 16472]` で FAIL（= 修正前の症状を再現し、修正で直ることを確認）
- `cargo fmt` / `cargo clippy --all-targets -D warnings` クリーン

### 挙動の変化

- これまで Ctrl+Q やウィンドウを閉じたときに（事故的に）生き残っていた claude.exe も、今後は ccnest と一緒に終了する。セッションの会話ログは残るので `--resume` で戻せる。

---

## 2026-10-19 — 保存済みセッションを再開して開く `--resume`（v0.1.23）

ブランチ: `feature/resume-sessions`

### 背景

PC 再起動後の復元スクリプト（`~/.claude/scripts/claude-sessions-restore.ps1`）は 1 セッション 1 つの Windows Terminal タブで素の `claude --resume` を開いていた。ゆうさんの希望は「ccnest で、リポジトリごとに 1 つの ccnest にまとめて開く」。ccnest は常に新しい uuid の `--session-id` で起動するので、既存セッションを開く手段が無かった。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/cli.rs | 0.1 | 新規。`parse_args`（位置引数 cwd＋繰り返し `--resume <uuid>[=<タブ名>]`・未知のオプションはエラー）と `ResumeSpec` |
| src/claude/launcher.rs | 1.4 | `SessionStart { New, Resume }`。Resume は `--session-id` の代わりに `--resume <id>`（permission 系フラグは同じ） |
| src/pane/mod.rs | 0.2 | `Pane::spawn` が `SessionStart` を受け取る |
| src/app.rs | 0.7 | `App::with_resumes`。先頭のセッションで最初のタブ、残りは `pending_resumes` に積み `spawn_next_resume` で 1 タブずつ（アクティブタブは変えない） |
| src/event.rs | - | 2 秒 tick で `spawn_next_resume` を 1 回呼ぶ。失敗はステータス行へ |
| src/main.rs | 0.2 | `parse_args` → `App::with_resumes` |
| README.md | - | Launch 節に `--resume`、Version History |
| Cargo.toml | - | 0.1.22 → 0.1.23 |

設計メモ:
- 1 tick 1 タブにしたのは、並行起動した claude が `~/.claude.json` を同時に書いて壊すのを避けるため（旧スクリプトも 1.5 秒ずつ空けていた）。14 セッションなら約 28 秒で揃う
- `--resume` は fork しないので session id は不変 → サイドバーの残りコンテキスト（`session_path(cwd, id)`）はそのまま効く
- 副作用: `ccnest --help` など未知のオプションは raw mode に入る前にエラー終了する（以前は対話モードで起動していた）

---

## 2026-10-10 — ステータスバーの利用制限に解除までの残り時間を出す（v0.1.22）

ブランチ: `feature/usage-reset-countdown`

### 背景

v0.1.20 で利用制限 %（`5h 42% · wk 18% · Fable 7%`）は出るようになったが、「いつ解除されるか」が分からず結局 `/usage` を開いていた。API 応答の `resets_at` は v0.1.20 の時点で取得・保存済み（`Bucket.resets_at`）だったので、表示を足すだけでよい。

本人の決定: 形式はカウントダウン（絶対時刻ではない）・常に出す（幅が足りないときだけ自動で落とす）・週枠（wk と Fable）は解除が同じなら末尾に 1 回だけ。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/claude/usage.rs | 0.2 | `format_remaining`（1 時間未満 `59m`・1 日未満 `2h10m`・以上 `2d9h`・解除済みは None）、`format_usage_with_reset`（隣接する枠の `resets_at` が 60 秒以内なら 1 グループにして末尾に 1 回 ` ⏳…`）、`status_label` の戻りを `UsageLabel { full, bare, level }` に。`buckets()` で表示順の列挙を共通化 |
| src/app.rs | 0.6 | `usage_label()` が `UsageLabel` を返す |
| src/ui/mod.rs | 0.15 | `layout_status` に `usage_bare` を追加。削る順を cwd → ブランチ → 残り時間（full→bare 差し替え）→ 利用制限を丸ごと → モデル に |
| README.md | - | Plan usage 節に ⏳ の書式・グループ化・落とし順、Version History |
| Cargo.toml | - | 0.1.21 → 0.1.22 |

表示例: `5h 42% ⏳2h10m · wk 18% · Fable 7% ⏳2d9h`（wk と Fable の解除が違えば `wk 18% ⏳2d9h · Fable 7% ⏳4d1h`）。

設計メモ:

- 残り時間は `now` を引数に取る純粋関数なので、既存の 2 秒 tick の再描画でそのまま進む（タイマー追加なし）。タイムゾーン変換も不要
- `⏳`（U+231B）は unicode-width が 2・Windows Terminal も絵文字 2 桁で描くので行幅の計算と食い違わない（`hourglass_is_two_cells_wide` で固定）。ステータスバーの他の記号で起きた「申告 1 桁・描画 2 桁」のはみ出し（`draw_statusbar` のコメント）とは逆に、両方 2 で揃っている
- 実応答では wk が `18:00:00.254094`、Fable が `18:00:00` と秒以下だけ違うため、完全一致ではなく 60 秒の許容幅でまとめる

### 検証

- TDD: usage.rs に 8 件・ui/mod.rs に 2 件のテストを先に書いて赤を確認してから実装。`cargo fmt --all` 差分なし、`cargo clippy --all-targets -- -D warnings` 警告なし、`cargo test --all` 307 件（lib）＋ 9 件（統合）緑
- 実応答 FIXTURE（2026-10-06 10:00 UTC）で `5h 1% ⏳4h40m · wk 0% · Fable 0% ⏳6d8h`
- 幅 45 の描画バッファで `Fable 5.1 │ 5h 42% · wk 18% · Fable 7%`（残り時間だけ落ちて % が残る）

## 2026-10-10 — 最終列まで埋まった行のコピーで字下げの空白が混ざらないようにする（v0.1.21）

ブランチ: `fix/copy-soft-wrapped-indent`

### 背景

本人報告: Claude ペインで `auth_multi` の長い文字列（空白なし・約 340 字）をドラッグ選択してコピーすると、折り返し位置に半角 2 つが混ざる（`…247e  4b44…`）。改行は入らない。

v0.1.19 は Claude Code の自前改行（`CR` + `CSI n C` + `CSI 1 B`）を `app_wrapped` でつなぐようにしたが、**行が最終列まで埋まった**ときは ConPTY の出し方が変わる。実測（Claude Code v2.1.296・ペイン幅 118 桁・`CCNEST_PTY_DUMP=1`）では、同じ応答の中で 1 つ目の折り返しは `CSI 12;3H`（絶対カーソル移動・vt100 の `row_wrapped` は立たない）、2 つ目は最終列まで埋まった直後に `  210|…` と**字下げの空白ごと直接続けて**書かれていた（端末の自動折り返し・`row_wrapped` が立つ）。後者はソフトラップ経路（改行なし・trim なし・字下げも残す）に入るため、次行の字下げ 2 つがそのまま貼られていた。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/event.rs | 0.11 | `AppWrapTraits` に `body_indent`（箇条書き記号を飛ばした本文位置）を追加。`app_wrapped_after_autowrap(a, b)`: a がソフトラップ行で、a の行末の語が割られた長い文字列らしく（`tail_splittable`）、b の行頭が箇条書き・新しいパスでなく、a の本文位置が 1 桁以上で、b の行頭空白の幅が a の本文位置と同じなら、b の行頭空白を字下げとみなして落とす。結合ループのソフトラップ分岐で `strip_indent` に使う |
| README.md | - | Version History |
| Cargo.toml | - | 0.1.20 → 0.1.21 |

落とさない（空白を本文として残す）条件:

- a の本文位置が 0（字下げの無いシェル出力などの素のソフトラップ）
- b の行頭空白の幅が a の本文位置と違う（2 に対して 4 など）
- b が箇条書き記号・`C:\`・`http://` で始まる
- a の行末の語がパス風でも本文先頭からの 1 語でもない（英文の単語折り返し）

### 検証

- TDD: 新規テスト 8 件を先に書き、つなぐ 4 件が赤（出力に `  ` が混ざる＝報告と同じ症状）になることを確認してから実装した。つなぐケースは、字下げ 2 桁の段落・3 行版・`● ` 行・`  - ` の箇条書き・実測のバイト列どおりに `CSI r;3H` と自動折り返しが混ざった応答。残すケースは、字下げ無しの素のソフトラップ・次行が箇条書き・字下げ幅が違う行
- `cargo fmt --all` 差分なし、`cargo clippy --all-targets -- -D warnings` 警告なし、`cargo test --all` 297 件（lib）＋ 9 件（統合）緑
- 実バイトの再生テスト `extract_selected_text_replays_captured_claude_reply`: 上の実測で採取した応答の生バイト列（`CSI 11;1H ● … CSI 12;3H … 直接続く "  210|…"`）をそのまま 118 桁の vt100 に流し、3 行を選択してコピーすると改行も空白も入らず元のトークン列と一致する
- マウス E2E（新しい conhost 窓で本物の Claude ペインにトークン列を出させ、ドラッグ選択→Ctrl+C→クリップボード照合）は 2 回試みて **未完**。SendInput はフォーグラウンド窓が自分の窓であることを条件にしているが、本人がキーボード操作中（`GetLastInputInfo` でアイドル 0 ms）でフォーカスが外れ、2 回目は入力送信直後に出力が止まった（窓が閉じられたとみられる）。本人の作業を邪魔しないため打ち切った。スクリプトはセッションの scratchpad（`e2e-copy.ps1`）に残した
- 原因確定のための生バイト採取（`capture-wrap.ps1`・自分の conhost 窓・`CCNEST_PTY_DUMP=1`）は成功。採取・E2E とも自分で起動した窓の子孫プロセスだけを止め、本人の ccnest 2 本と Claude 6 本は無傷であることを `Get-Process` で確認した

## 2026-10-06 — ステータスバーに利用制限 %（5h・週・Fable）を出す（v0.1.20）

ブランチ: `feature/usage-in-statusbar`

### 背景

本人質問「ccnest の表示の中に、Claude Code のセッション、Weekly、Fable の制限を今何%か表示させられる？」。

- 公式のステータスライン（`rate_limits.five_hour` / `seven_day`）には Fable が無い。さらに、ステータスラインを設定すると Claude Code が `esc to interrupt` を出さなくなり、タブの 🟩🟨🟪 判定（`detect_status`）が壊れる。そのため使わない
- `/usage` は `GET /api/oauth/usage`（非公式・ドキュメント無し）を叩いている。2026-10-06 の実物では、`five_hour.utilization` / `seven_day.utilization`（0〜100）が返る。Fable は `limits[]` の `kind: "weekly_scoped"` の行（`scope.model.display_name: "Fable"`、`percent`）で返る。`/usage` の「Current week (Fable)」はこの行
- 本人が「非公式でも 3 つとも出す」「下のステータスバー」を選んだ

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/claude/usage.rs | 0.1 | 新規。応答の解析 `parse_usage`（5h・週と、`limits[]` のモデル別週枠。5h・週が null なら `kind: session` / `weekly_all` で補う）、表示文字列 `format_usage`（切り捨て・リセット時刻を過ぎた枠は 0%）、色の段階 `usage_level`（70% / 90%）、30 分より古い値を隠す `status_label`。問い合わせスレッド `spawn_usage_poller` は、トークンを `.credentials.json` から読むだけで更新しない。結果は `%APPDATA%\ccnest\usage-cache.json` で窓どうし共有し、問い合わせは全体で 5 分に 1 回。429 のあとは 15 分、トークン無し・期限切れのときは 60 秒あけて再確認する |
| src/app.rs | 0.5 | `usage_cell`（スレッドが書く）を 2 秒 tick で `try_lock` して `usage` にコピー。`usage_label()` |
| src/event.rs | 0.10 | 入力ポンプの隣で問い合わせスレッドを起動 |
| src/ui/mod.rs | 0.14 | ステータスバー 1 行目の 4 つ目のセグメント。削る順は cwd → ブランチ → 利用制限 %（丸ごと落とす）→ モデル |
| src/ui/theme.rs | 0.5 | `status_usage` / `status_usage_warn` / `status_usage_crit` |
| Cargo.toml | - | 0.1.19 → 0.1.20。`ureq` 2（rustls。OpenSSL 不要）を追加 |
| README.md | - | 「Plan usage in the status bar」節、Version History |

環境変数: `CCNEST_USAGE=off`（機能ごと無効）、`CCNEST_USAGE_POLL_SECS`（既定 300・最小 60）。

### 検証

- 応答の形は、実装前に本物の `/api/oauth/usage` を 1 回叩いて確かめた（200・2,235 文字）。テストのフィクスチャはこの実物から作った
- CI（windows-latest）: `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` は警告なし、`cargo test --all` 288 件緑。新規テストは usage.rs 18 件（解析・切り捨て・リセット済み 0%・色の段階・30 分で隠す・トークン期限・`CLAUDE_CONFIG_DIR`・間隔の env・失敗ごとの待ち・キャッシュの読み書き）と ui/mod.rs 5 件（並び順・削る順・丸ごと落とす・0〜160 桁ではみ出さない・色）
- release.yml をブランチで試し実行（`dry_run_tag=v0.1.20-rc1`）し、4 プラットフォームともビルド成功。Windows の exe は 3.7 MB → 5.7 MB（rustls と ring のぶん）
- 実機 E2E（Windows Terminal の新しいウィンドウ・shell ペイン・キー入力なし）
  - 起動 9 秒後のステータスバーが `cwd: C:\work\80_ccnest │ shell │ ⎇ feature/usage-in-statusbar │ 5h 2% · wk 0% · Fable 0%`。`usage-cache.json` の値（5h 2.0 / 週 0.0 / Fable 0.0）と一致
  - `CCNEST_USAGE=off` では利用制限 % が出ない
  - 2 つ目の窓は共有キャッシュの値をそのまま出し、`fetched_at` は変わらなかった（問い合わせは増えない）
  - 3 回とも、自分で起動した ccnest だけを止め、WT はそのウィンドウだけ閉じた（残骸 0）

## 2026-10-04 — Claude Code が割った長いパスを改行なしでコピーする（v0.1.19）

ブランチ: `fix/copy-app-wrapped-path`

### 背景

ccnest で長いパスをドラッグ選択してコピーすると、`...\follow` と `  _list_fetch_ui_e2e\...` の 2 行に割れて貼られる、という本人報告。
2 行目の行頭に字下げがあるのが手がかりで、これは端末の自動折り返し（ソフトラップ）ではない。Claude Code が自分で幅を計算して改行と字下げを出している。
ConPTY 越しのバイト列を実測すると、右端まで埋めた行のあとに `CR` + `CSI 2 C` + `CSI 1 B` で次の行へ移っていた。LF も自動折り返しも使っていないので、vt100 の `row_wrapped` は立たない。v0.1.18 までのコピーは `row_wrapped`（と全角の遅延ラップ）しかつながないため、必ず `\n` が入っていた。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/event.rs | 0.9 | `extract_selected_text_from_parser` を行ごとの `RowShape`（抽出文字列・ソフトラップ・行末まで選択済みか・`AppWrapTraits`）で組み立てるように変更。隣り合う 2 行が `app_wrapped` なら、前の行の右余白と次の行の字下げを落として改行なしでつなぐ |
| README.md | - | Version History |
| Cargo.toml | - | 0.1.18 → 0.1.19 |

`app_wrapped(a, b)` の条件はすべて満たすこと:

- a がソフトラップ行ではなく、選択が a の右端まで届いている
- a の最後の文字が右端から `APP_WRAP_EDGE_SLACK = 2` 列以内にある。実測（Claude Code v2.1.289・89 桁）では、応答本文は右端の列まで埋めて割り、入力欄のエコーは右に 1 桁空けて割る。全角 1 文字ぶん（+1）を足して 2 にした
- a の行末の語が「割られた長い文字列」らしい。英数字か CJK を含み、かつ「行の本文の先頭（`●` `-` `1.` などの箇条書き記号を飛ばした位置）から始まる 1 語」か「`\` か `/` を含む」こと
- b の行頭が空行・箇条書き記号（1 文字の記号か `1.` `12)`）・新しいパス／URL の始まり（`C:\` `C:/` `\\` `http(s)://`）ではない
- b の字下げが a 以上

各行の特徴は画面セルだけから決まり、隣の行は見ない。そのため、scrollback を窓ごとに読む抽出ループの境目でも判定が変わらない。

### 検証

- TDD: 新規テスト 13 件を先に書いて 8 件が赤になることを確認してから実装した。内訳は、つなぐケース 7 件（字下げ 2 桁の段落、`● ` 行で ConPTY の実バイト形、`保存先:` の後ろから始まるパス、箇条書きの中、3 行に割れたもの、右端から 1 桁内側で割れたもの、scrollback の窓の境目）と、改行を残す回帰ケース 6 件（次の箇条書き、パスの一覧、右端に届かない行、罫線、字下げが浅くなる行、英文の単語折り返し）
- `cargo test --all` 265 件ほか全件緑。`cargo clippy --all-targets -- -D warnings` は警告なし
- 本物の Claude Code の出力でも確認した。89 桁の ConPTY で Claude Code（Haiku）にパスを 3 通りの形で表示させ、生バイトを ccnest の vt100 に流してコピーした結果、3 つとも 1 行につながった（行頭のパス、`保存先:` の後ろのパス、`- item` の後ろのパス）。3 行をまとめて選択したときは各行の間の改行が残った
- 実機 E2E: conhost（120×30）で ccnest を起動し、本物の Claude Code ペインに長いパスを表示させた。2 行にわたってマウスでドラッグ選択し、Ctrl+C を注入した。クリップボードの中身は改行なしの 1 行で、元のパスと完全に一致した（`has_newline: False` / `match: True`）
  - 注意: 日本語ロケールの conhost は `●` を全角で描くので、ccnest 内部の桁と画面上の見た目が 1 桁ずれる。座標は ccnest 側の桁で指定した

## 2026-09-27 — 空き行と細線枠を撤回して v0.1.15 の見た目に戻す（v0.1.18）

ブランチ: `fix/restore-pane-frame-look`

### 背景

v0.1.17（細線枠・空き行 1 行）を実機で見た本人から「微妙だったから空き行なし、設定も元に戻して」
「線も元に戻して」。タブ行の真下に通常の箱線の枠と「[1] フォルダ名」が来る v0.1.15 の見た目に戻す。
枠タイトルのフォルダ名表示（v0.1.15）はそのまま。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/ui/mod.rs | 0.13 | `main_layout` から空き行（`TABBAR_GAP_ROWS`）を削除、`pane_block` から `border_set(ONE_EIGHTH_TALL)` を削除。描画は v0.1.15 と同じ（縦割り 1 / Min(3) / 2、`Borders::ALL` の箱線）。純関数 2 つとテストは残す |
| README.md | - | Version History |
| Cargo.toml | - | 0.1.17 → 0.1.18 |

教訓: 行単位の端末で「半行」を 1/8 ブロックで擬似的に作ると、枠全体の線種が変わって見た目の印象が大きく変わる。
見た目の微調整はプレビューだけでなく実機で並べて見てもらってから確定する。

### 検証

- `cargo test --all` 252 件緑（`main_layout_puts_panes_right_below_tabbar` / `main_layout_rows_are_contiguous_at_various_heights`
  を空き行なしに、`pane_block_draws_plain_box_lines` で上辺 `┌ [1] 30_XTP3 ─────────┐`・側面・下辺と色を検証）、
  `cargo clippy --all-targets -- -D warnings` 警告なし。v0.1.15（`7d75099`）との差分はコメント・関数の切り出し・テストのみ
- 実機: Windows Terminal の新規ウィンドウで `ccnest C:\Users\mitam\Desktop\work\20_XTP3` を起動しスクリーンショットで確認

## 2026-09-27 — ペイン枠を細線（McGugan 式）にして上辺を半行上げる（v0.1.17）

ブランチ: `feature/pane-border-one-eighth`

### 背景

v0.1.16 の空き行 1 行に対して「空き行を 1 行の半分にできる？もうちょい上が良い」という本人依頼。
文字は行単位でしか置けないので、半行ぴったりの空き行は作れない。箱線 `─` は行の真ん中に引かれるが、
1/8 ブロック文字（`▔` は行の上端、`▁` は行の下端）で枠を描けば線だけを半行動かせる。
4 案（線だけ半行上／線を半行上・名前は線の上／空き行なしに戻す／今のまま）をプレビュー付きで確認し、
**「線だけ半行上げる」**（空き行は残し、上辺をフォルダ名の行の上端へ）に決定。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/ui/mod.rs | 0.12 | 純関数 `pane_block(title, style)` に Claude ペインの枠を切り出し、ratatui 標準の `border::ONE_EIGHTH_TALL` で描く（上辺 `▔`・左右 `▕` `▏`・下辺 `▁`）。`render_layout` はこれを使う |
| README.md | - | Version History |
| Cargo.toml | - | 0.1.16 → 0.1.17 |

- 線の高さ: タブ行の下端から上辺まで 1.5 行 → 1 行（空き行 1 行はそのまま）。フォルダ名は上辺のすぐ下の行（v0.1.16 と同じ行）
- 左右の線は各ペインの内側の縁、下辺は最終行の下端。内側（中身）の座標とサイズは `Block::inner` のまま変わらない
- 左右に並べた分割では左右の線の間に 2 列ぶんの余白、上下に並べた分割では上のペインの下辺と下のペインの上辺が接して少し太い線になる
- 非フォーカスのグレーとフォーカス中のオレンジはそのまま。シェルペイン（枠なし）は影響なし
- 1/8 ブロック文字は Windows Terminal では内蔵グリフで描かれる（フォント非依存）

### 検証

- `cargo test --all` 252 件緑（新規 `pane_block_draws_one_eighth_tall_lines`: 24×4 の TestBackend で
  上辺 `▕ [1] 30_XTP3 ▔▔▔▔▔▔▔▔▔▏`・側面・下辺 `▕▁…▁▏` と色を 1 文字単位で検証。既存の
  `pane_frame_renders_folder_title_in_top_border` も `pane_block` 経由に変更）、`cargo clippy --all-targets -- -D warnings` 警告なし
- 実機: Windows Terminal の新規ウィンドウで `ccnest C:\Users\mitam\Desktop\work\20_XTP3` を起動しスクリーンショットで確認

## 2026-09-27 — タブバーとペイン枠の間に空き行を 1 行（v0.1.16）

ブランチ: `feature/pane-top-gap`

### 背景

v0.1.15 でペイン枠のタイトルをフォルダ名にした直後、「わずかにオレンジのフォルダ名の表示含めて、
オレンジ線を下に下げられません？」という本人依頼。タブ行（例「30_XTP3」）の真下にオレンジの上枠と
「[1] 30_XTP3」が貼り付いていて窮屈に見える。端末の最小単位は 1 行なので、1 行下げる。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/ui/mod.rs | 0.11 | 純関数 `main_layout(area)` にメイン列の縦割りを切り出し（タブバー 1 行 → 空き `TABBAR_GAP_ROWS`=1 行 → ペイン領域 → ステータスバー 2 行）。`draw` はこれを使う。空き行には何も描かない |
| README.md | - | Version History |
| Cargo.toml | - | 0.1.15 → 0.1.16 |

- 空き行はペイン領域全体の上に 1 行だけ。分割したペイン同士の間には入らない
- シェルペイン（枠なし）もタブ行の下に 1 行空く
- サイドバーは左列で全高のまま（位置は変わらない）
- マウスはすべて描画した矩形（`pane_rects` / `tab_rects`）で判定しているため、空き行のクリックやホイールはどこにも当たらず無反応。ペイン内の座標変換と PTY サイズは描画後の矩形から自動で追従（ペイン高さは 1 行減る）

### 検証

- `cargo test --all` 251 件緑（新規 `main_layout_leaves_one_blank_row_below_tabbar` /
  `main_layout_rows_are_contiguous_at_various_heights`）、`cargo clippy --all-targets -- -D warnings` 警告なし
- 実機: conhost 独立窓で `ccnest C:\Users\mitam\Desktop\work\20_XTP3` を起動しスクリーンショットで確認
  （1 行目タブ、2 行目空き、3 行目にオレンジの上枠と「[1] 30_XTP3」）

## 2026-09-27 — ペイン枠のタイトルを exe 名からフォルダ名へ（v0.1.15）

ブランチ: `feature/pane-title-folder-name`

### 背景

Claude ペインの枠タイトルが「[1] ccnest-claude-launcher.exe」になっていた。`Pane.command`（spawn した
実行ファイル名）をそのまま出しており、実運用では `CCNEST_CLAUDE_BIN` のシム名になるため、どのプロジェクトの
ペインか枠だけでは分からない。「フォルダパスの名前にしてほしい」という本人依頼。表示形式は
「~ 短縮パス／フォルダ名だけ／フルパス」の 3 択で確認し、**フォルダ名だけ**（`[1] 30_XTP3`）に決定。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/ui/mod.rs | 0.10 | 純関数 `pane_title(pid, cwd)` = `" [id] {folder_title(cwd)} "`（タブ初期名と同じ `app::folder_title` を再利用）。`render_layout` の Claude ペイン枠タイトルを `p.command` からこれに差し替え。`(gone)` フォールバックは据え置き |
| docs/index.html | 0.6 | モックアップのペインタイトル `[1] claude.exe` / `[2] claude.exe` → `[1] ccnest` / `[2] ccnest` |
| README.md | - | Version History |
| Cargo.toml | - | 0.1.14 → 0.1.15 |

`Pane.cwd` は spawn 時に固定（split／新タブは親ペインの cwd を継承）で、ステータスバーの `cwd:` と同じ値。
シェルペインは v0.1.14 から枠もタイトルも描かないので影響なし。サイドバーの Panes 行は `command`
表示のまま（cwd を括弧で併記済み）。

申し送り: README の「`F2` でタブ名変更」は実装が `Alt+F`（keymap.rs）でズレている（今回は対象外）。

### 検証

- `cargo test --all` 249 件緑（新規 `pane_title_shows_folder_name_not_exe` /
  `pane_title_falls_back_to_full_path_at_drive_root` / `pane_frame_renders_folder_title_in_top_border`）、
  `cargo clippy --all-targets -- -D warnings` 警告なし
- 実機: conhost 独立窓で `ccnest C:\Users\mitam\Desktop\work\20_XTP3` を起動しスクリーンショットで確認
  （上枠が「[1] 30_XTP3」）

## 2026-09-26 — シェルペインの枠線とタイトル行を描かない（v0.1.14）

ブランチ: `feature/shell-pane-no-border`

### 背景

Ctrl+C×2 で Claude ペインをシェル (cmd.exe) に戻すと、フォーカス中シェルペインの水色の枠線と
「[4] C:\WINDOWS\system32\cmd.exe」のタイトル行が付く。「通常の cmd に戻ってほしい（青枠やめて）」という
本人フィードバック。確認の結果「見た目の話。ccnest の中に留まるのは OK」だったので、シェルペインだけ
枠もタイトル行も描かない。

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/ui/mod.rs | 0.9 | 純関数 `pane_frame_style`: シェルペイン (claude_running=false) は None → `render_layout` が枠もタイトルも描かず `area` 全体をペインに使う（pane_rects / PTY サイズも全域）。Claude ペインはフォーカスでオレンジ、非フォーカスで暗いグレーのまま |
| README.md | - | Version History |
| Cargo.toml | - | 0.1.13 → 0.1.14 |

`theme.border_focused`（水色）はサイドバー枠とコンテキストメニューで引き続き使用。
分割中にシェルペイン同士が隣り合うと境界線が無くなる（Claude ペインが隣なら Claude 側の枠が境界になる）。
必要になったら分割時だけ細い区切りを足す。

### 検証

- `cargo test --all` 246 件緑（新規 `pane_frame_style_table`）、`cargo clippy --all-targets -- -D warnings` 警告なし
- 実機: conhost 独立窓でシェルペインの ccnest を起動しスクリーンショットで確認（枠線・タイトル行なし、
  cmd のバナーが 1 行目から表示）

## 2026-09-26 — タブバーの中クリック（ホイールクリック）でタブを閉じる（v0.1.13）

ブランチ: `feature/middle-click-close-tab`

### 背景と設計

タブ見出しは左クリックで切替できたが、閉じる手段は `Ctrl+W`（フォーカス中ペインのみ）だけだった。
ブラウザと同じく中ボタンでタブを丸ごと閉じる。

- **Down(Middle) で確定**。対の Up(Middle) は追跡しない（`classify_menu_mouse` と同じ方針。
  中ボタンから `mouse_local_drag` を立てない）
- **タブ丸ごと**（分割中の全ペインを terminate）。確認ダイアログ無し（Ctrl+W / Ctrl+Q と統一）
- リネーム中・コンテキストメニュー表示中は無視（リネームのコミット先がアクティブタブのため）
- **描画矩形は閉じた瞬間に無効化**: `tab_rects` を `&mut Vec` にして `clear()`。同一バッチ内の次クリックが
  旧 index を踏まない（`menu_rect = None` と同じ考え方）。左クリック切替にも `i < tabs.len()` ガードを追加
- **quit 後の残イベント不処理**: 最後のタブを閉じた／Ctrl+Q の直後に同一バッチの残りや保留矢印 flush が
  `current_tab()` を踏む潜在 panic（Ctrl+W でも起き得た）を、`process_batch` と `run_event_loop` の
  `if app.quit { break; }` で塞いだ

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/event.rs | 0.8 | 純関数 `tab_at` / `classify_tab_mouse`（`TabMouseAction`）。`Down(Left)` のインライン切替を置換、`Down(Middle)` → `App::close_tab`。`tab_rects` を `&mut Vec` にして閉じた直後に `clear()`。quit ガード。`CCNEST_INPUT_TRACE` に `close_tab idx=N tabs_left=M` |
| src/app.rs | 0.4 | `close_tab(idx)`（配下全ペイン terminate）、`remove_tab`、純関数 `active_after_close`。`close_focused_pane` の空タブ分岐と共用（挙動差: Ctrl+W でタブが消えた時も新アクティブタブに `mark_active_tab_seen` が掛かる。次 tick で収束する処理の前倒し） |
| README.md | - | キー表に左クリック／中クリック行、Tabs 節に 1 文、Version History |
| scripts/e2e/mmb-close-tab.ps1 | - | 実機 E2E（新規）。conhost 独立窓 + WT 新規窓で SendInput の中クリックを注入し、input-trace の `close_tab` 行と子プロセス数で判定 |
| Cargo.toml | - | 0.1.12 → 0.1.13 |

守った不変条件: 保留矢印 flush は drain + `process_batch` 直後（quit 経路だけ手前で抜ける）。
ホイール簿記（`last_wheel_at` / `pending_arrow`）と `mouse_local_drag` は中ボタンから触らない。
閉じる処理は `TerminateProcess` のみでループを止めない。

### 検証

- `cargo fmt --all -- --check` 差分なし、`cargo clippy --all-targets -- -D warnings` 警告なし、
  `cargo test --all` 245 件緑（新規 4: `active_after_close_table` / `tab_at_table` /
  `tab_at_returns_recorded_index` / `classify_tab_mouse_table`）
- 実機（`scripts/e2e/mmb-close-tab.ps1`、`CCNEST_INPUT_TRACE=1`、シェルペイン）:
  - conhost 独立窓: `Ctrl+T`×2 + `Ctrl+D` で 3 タブ 4 ペイン（cmd.exe 4）→ 左端タブを中クリック ×3 →
    `close_tab idx=0 tabs_left=2` / `tabs_left=1` / `tabs_left=0`、cmd.exe 4→3→2→0、最後で ccnest が
    panic 無く終了（crash log なし）、`pending_arrow_flush` 0 件
  - Windows Terminal 新規窓（`wt -w <unique>`）: `Mouse(Down(Middle)@2,0)` が WT→ConPTY 経由で届き
    `close_tab idx=0 tabs_left=0` → ccnest 終了 → WT 窓が閉じる（WT が中ボタンを横取りしない実証）
  - 本物の Claude ペイン（launcher 経由）でも同じ 3 クリックで同じ `close_tab` 行と正常終了を確認（初回実行）

### 教訓（E2E ハーネス）

- ユーザー環境は `CCNEST_CLAUDE_BIN` が launcher シムを指しているので、PATH を隠しても本物の Claude が起動する。
  シェルペインにしたいときは `CCNEST_CLAUDE_BIN` を存在しないパスで上書きし、かつ PATH から claude を外す
- WT は `-w <一意な名前>` で新規窓になるが**既存の WindowsTerminal.exe プロセス内**に作られる。
  プロセス kill は不可。前面ウィンドウのタイトル（`--title` + `--suppressApplicationTitle`）で注入先を必ず照合する
- マウス注入は `SetProcessDPIAware()` が必須（PowerShell 5.1 は DPI 非対応で座標が仮想化される）。
  セル幅の較正は 2 点プローブだと量子化誤差が大きい（4 行差か 5 行差かで 25%）ので、trace が返す `@col,row` で
  狙いを補正する反復にした
- PowerShell の `@($a + 240, $b + 120)` は `$a + @(240, $b) + 120` と解釈される（カンマが `+` より強い）

## 2026-09-07 — 「Claude の処理中／処理後も打鍵がかくかくする」の調査と対策（v0.1.12）

ブランチ: `fix/typing-while-processing`

### 報告と調査

v0.1.11（アイドル時の打鍵）を本人が「直った気がする」と確認した直後の報告:
**「Claude Code が処理中だと入力表示が遅い。処理が終わってもかくかくしたままになる」**。

読み取り専用監査（3 レンズ → 12 候補 → 反証チェック 22 エージェント）の結論は
「ccnest 側に >20ms を単独で説明できる機構は無い。既知の寄与は合計 +13ms 程度で、
未計測の大物は Claude Code 側」。そこで**処理中に信用できる計測**を作った。

### 計測ハーネス（`scripts/latency/`）

打鍵ごとに `child_render`（子がエコーを返すまで）と `present`（ccnest が描くまで）を
分離する。エコーの同定は PTY 生バイトから行い、注入文字は非 ASCII（`♠♥♦…`）にして
VT エスケープ列との誤検出を無くした。処理前・処理中・処理後の 3 相を 1 プロセスで測る。

**ハーネス作りで踏んだ罠（すべて実害あり）**:

- `wt -w new` は Windows Terminal がプロセスを再利用すると**ユーザーの既存ウィンドウの
  タブ**として開く。13:08 の実行では注入した 19 打が全て別ウィンドウへ行った
  （ccnest の入力トレース 0 件）。以後 `conhost.exe <bat>` で独立ウィンドウにし、
  **前面ウィンドウが自分のプロセスツリーに属することを検証**してから打つようにした
- 1 打ごとにもフォーカスを検証する。13:32 の実行では途中で前面を奪われ 19 打中 17 打が
  他所へ流れていた
- 最初の 1 打が ccnest に届かなければ即中止（プロンプト文字列を未知の窓へ打たない）
- `~ [ ? > %` 等の ASCII 記号は VT シーケンス内にも現れるためエコー同定に使えない
- `tool` / `flood` はプランモードだと承認待ちで止まる → 権限モードを外す

### 計測結果（2026-09-07、before = v0.1.11、after = 本版。各相 19 打、p50/p90 ms）

**マウスを動かしながら打つ（アイドル・API 不要）**

| | 同一入力での描画回数 | 打鍵→画面 p50 / p90 |
|---|---|---|
| before | 376 | 11 / 22 |
| after | **151** | 11 / **17** |

**Claude が忙しいとき（40 秒間じわじわ出力するツール実行）**

| 相 | before total | after total | before ccnest 分 | after ccnest 分 |
|---|---|---|---|---|
| 処理前 idle | 12 / 23 | 10 / 19 | 2 / 6 | 2 / 4 |
| 処理中 busy | 9 / 30 | 8 / 28 | 2 / 3 | 2 / 6 |
| 処理後 idle | 8 / 12 | 10 / 13 | 2 / 5 | 2 / 6 |

- **ccnest 側（`present`）はどの状態でも 2ms（p50）/ 3〜6ms（p90）**。処理中に伸びるのは
  `child_render`（21 → 29ms p90）＝ **Claude Code 自身の描画**。監査の R2（Ink の
  レンダ throttle 16ms）と一致する
- **「処理後も遅いまま」は新規セッションでは再現しなかった**（処理後 idle は処理前と同等）。
  本人環境との差は、長い会話履歴・同時に走る 4 つの ccnest と 5 つの claude・
  Windows Terminal ホスト・**そして 4 窓のうち 2 窓が v0.1.10 のまま**（2026-09-06 22:06 の
  exe 差し替え以前に起動）であること

### 変更点

| ファイル | ver | 内容 |
|---|---|---|
| src/event.rs | 0.7 | `echo_pending`: キー書き込みから既定 100ms 以内に届く最初の出力は出力フレーム cap を免除して即描く（ストリーミング中だけエコーが遅れる非対称を解消）。Moved だけのバッチでは再描画しない（メニュー・ドラッグ中を除く）。`parse_ms_override` で 3 定数を env 上書き可能に |
| src/app.rs | 0.3 | `echo_pending` / `last_forwarded_move` を保持 |
| src/claude/launcher.rs | 1.3 | `should_pass_env`: `WT_SESSION` / `TERM_PROGRAM` / `ConEmu*` / `TMUX` 等を子へ渡さない |
| scripts/latency/ | - | 計測ハーネス一式（新規） |

守った不変条件: 保留矢印 flush は drain + `process_batch` 直後、`last_output_draw` の更新は
出力起因のときのみ（ストリーミングの束ね方を変えない）、`is_paste_candidate` の Press/Release 許容。

### 検証

- `cargo test --all` 241 件緑、`cargo clippy --all-targets -- -D warnings` 警告なし
- 上表の実測（`scripts/latency`）。処理中の打鍵は 19/19 エコー同定、取りこぼし 0

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
cwd     : C:\Users\mitam\Desktop\work\10_ClaudeCompany
実在dir : C--Users-mitam-Desktop-work-10-ClaudeCompany
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
