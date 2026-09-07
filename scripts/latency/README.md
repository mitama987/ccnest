# 打鍵レイテンシ計測ハーネス

ccnest の「文字入力が遅い／かくかくする」を**数字で**確かめるための計測一式。
Claude ペインで 19 個の非 ASCII 記号を注入し、1 打鍵ごとに次の 3 つを出す。

| 値 | 意味 |
|---|---|
| `child_render` | ccnest がキーを子へ書いてから、子 (Claude Code) がエコーを返すまで。**子側のコスト** |
| `present` | エコーが届いてから ccnest が描き終わるまで。**ccnest 側のコスト** |
| `total` | 合計（打鍵 → 画面） |

## 使い方

```powershell
# アイドル時のみ（API 呼び出しなし）
.\measure-typing-latency.ps1 -Exe <ccnest.exe のパス> -Label idle-test -Mode idle

# Claude を実際に忙しくして、その前後も測る（プロンプトを 1 回送るので API 課金あり）
.\measure-typing-latency.ps1 -Exe <ccnest.exe> -Label flood-test -Mode flood -ClaudeDebug

# マウスを動かしながら打つ（トラックパッド操作の再現。API 不要）
.\measure-typing-latency.ps1 -Exe <ccnest.exe> -Label mouse-test -Mode idle -Mouse

python .\analyze-typing-latency.py idle-test flood-test mouse-test
```

`-Mode`: `idle`（アイドルのみ）/ `stream`（テキスト出力中）/ `tool`（ping 実行中）/
`flood`（40 秒間じわじわ出力するツール実行中。処理前・処理中・処理後の 3 相を測る）。

## 設計上の落とし穴（すべて実際に踏んだもの）

- **`wt` で起動しない**。Windows 11 の既定ターミナルは Windows Terminal なので、
  新しいコンソールが**ユーザーの既存ウィンドウのタブ**として開き、注入したキーが
  別タブへ飛ぶ。必ず `conhost.exe <bat>` で独立ウィンドウにする。
- **打鍵前と 1 打ごとにフォーカスを検証する**。途中で前面が奪われると残りのキーが
  他のアプリへ入る。最初の 1 打が ccnest の入力トレースに出なければ即中止する
  （プロンプト文字列を未知のウィンドウへ打ち込まないため）。
- **注入文字は非 ASCII にする**。`~ [ ? > % $` などは VT エスケープ列の中にも現れるので、
  PTY ダンプからエコーを同定できない。非 ASCII なら `<e2><99><a0>` の形で一意に拾える。
- **`SendKeys` は使わない**。ジャーナルフック経由で打鍵が数百 ms 単位に束ねて届く。
  `SendInput`（`KEYEVENTF_UNICODE`、down/up 別送、60ms ホールド）を使う。
- **計測用 cwd は信頼済みフォルダにする**。初見のフォルダでは Claude Code が
  「trust this folder?」を出し、打鍵がそこへ吸われる。
- **`tool` / `flood` は権限モードを外す**。プランモードのままだと承認待ちで止まり、
  「処理中」の状態が作れない。
- ストリーミング中は `CCNEST_LATENCY_TRACE` の `key_write->output` は信用できない
  （最初に届いた出力＝ストリーム本文を拾う）。エコー同定は PTY ダンプで行う。
