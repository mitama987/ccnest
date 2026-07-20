# VERSION HISTORY ARCHIVE

各ソースファイル末尾の `// Version History` を横断して追える形でまとめたもの。
新しいものが上。

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
- コンテキストウィンドウは `200_000` 固定のまま（`[1m]` サフィックスが JSONL に
  現れないので判定材料が無い）。`CCNEST_CONTEXT_WINDOW` で上書き可。
