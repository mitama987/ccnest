# VERSION HISTORY ARCHIVE

各ソースファイル末尾の `// Version History` を横断して追える形でまとめたもの。
新しいものが上。

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
  - 起動 9 秒後のステータスバーが `cwd: C:\work\90_other\ccnest │ shell │ ⎇ feature/usage-in-statusbar │ 5h 2% · wk 0% · Fable 0%`。`usage-cache.json` の値（5h 2.0 / 週 0.0 / Fable 0.0）と一致
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
- 実機: Windows Terminal の新規ウィンドウで `ccnest C:\Users\mitam\Desktop\work\30_XTP3` を起動しスクリーンショットで確認

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
- 実機: Windows Terminal の新規ウィンドウで `ccnest C:\Users\mitam\Desktop\work\30_XTP3` を起動しスクリーンショットで確認

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
- 実機: conhost 独立窓で `ccnest C:\Users\mitam\Desktop\work\30_XTP3` を起動しスクリーンショットで確認
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
- 実機: conhost 独立窓で `ccnest C:\Users\mitam\Desktop\work\30_XTP3` を起動しスクリーンショットで確認
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
