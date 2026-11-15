# 「裏で実行中」の判定とタブマーカーを Windows Terminal の実画面で確かめる (v0.1.36)。
# マーカーは v0.1.40 から実行中と同じ 🟢 (それまでは 🔵)。判定 (Background) は trace で見分ける。
# 使い捨て ccnest を WT の新しい窓で起こし、本物の claude に「裏で sleep を走らせて返答を終える」
# をさせて、次の 4 場面を見る:
#   1. 入力待ち (🟨)  2. 返答中 (🟢)  3. 返答は終わったが裏のシェルが動いている (🟢・判定は Background)
#   4. 裏のシェルも終わったあと (🟨 に戻る＝🟢 が貼り付かない)
# 判定の同定は CCNEST_STATUS_TRACE (ccnest が 2 秒 tick ごとに「判定結果＋判定に使った画面の
# 文字」を書く開発用ファイル) で行う。conhost 越しの読み取りは空白が潰れるので使わない。
# 絵文字の見た目は PrintWindow のスクショ (他の窓に隠れていても撮れる) で目視する。
# 各場面の画面の文字は $OutDir の trace-*.txt に残す (単体テストの文字列の出どころ)。
# -Agent (v0.1.39): 裏のシェルの代わりに「裏のエージェント」で同じ 4 場面を見る。この場面は
#   フッターに札が出ず、入力欄のすぐ上に "Waiting for 1 background agent to finish" が出る。
# -Decision (v0.1.42): 裏のシェルを動かしたまま、返答を「▼ ゆうさんの判断待ち」の節で終わらせる。
#   中身あり → NeedsAttention (🟨)、次の返答が「今はなし」→ Background (🟢) を見る。
#   シェルが 2 回の返答のあいだ動いているよう、-SleepSec は 150 くらいにする。
# ゆうさんの ccnest には触らない。Claude に 1 回だけ短い指示を送る (プラン利用量をごく僅かに使う)。
# 起動フォルダは信頼済みのリポ配下 target/ に作る (新しい一時フォルダだと信頼ダイアログが出る)。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-background-marker'),
  [int]$SleepSec = 60,
  [switch]$Agent,
  [switch]$Decision,
  # 使い捨ての claude に使わせるモデル。既定のまま (本人の設定) だと上位モデルの週の枠を
  # 1 回で約 1% 使うので、判定の確認には軽いモデルで足りる。
  [string]$Model = 'sonnet'
)
$ErrorActionPreference = 'Stop'
$typer = Join-Path $PSScriptRoot 'status-marker.ps1'
$exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $OutDir | Out-Null
Get-ChildItem $OutDir -File | Remove-Item -Force

Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static IntPtr Find(string title) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { if (!IsWindowVisible(h)) return true; var sb = new StringBuilder(512); GetWindowText(h, sb, 512); if (sb.ToString() == title) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
}
"@
Add-Type -AssemblyName System.Drawing
[void][W]::SetProcessDPIAware()

$tag = 'ccnest-bg-' + (Get-Date -Format 'HHmmss')
$work = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..\target')).Path $tag
New-Item -ItemType Directory -Force $work | Out-Null
$traceFile = Join-Path $work 'status-trace.txt'
$typeOut = Join-Path $work 'type-out.txt'
# WT は新しいタブの環境変数を作り直すことがあるので、環境変数は窓の中の cmd で決める。
# 入れ子の Claude Code 判定 (CLAUDECODE 等) と本人のシム設定を外し、bypass で始める
# (既定の plan モードだと裏のコマンドを走らせない)。
$runCmd = Join-Path $work 'run.cmd'
@"
@echo off
for /f "delims==" %%v in ('set CLAUDE 2^>nul') do if /i not "%%v"=="CLAUDE_CONFIG_DIR" set "%%v="
for /f "delims==" %%v in ('set AI_AGENT 2^>nul') do set "%%v="
for /f "delims==" %%v in ('set CCNEST 2^>nul') do set "%%v="
set "CCNEST_JP_TITLE=off"
set "CCNEST_CLAUDE_PERMISSION_MODE=off"
set "ANTHROPIC_MODEL=$Model"
set "CCNEST_STATUS_TRACE=$traceFile"
"$exe"
"@ | Set-Content $runCmd -Encoding ASCII

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
# 最小化されていると PrintWindow は真っ黒 (約 2 KB) を返す。前面には出さずに戻して撮り直す。
function Shot($h, $name) {
  $p = Join-Path $OutDir $name
  foreach ($try in 1..4) {
    if ([W]::IsIconic($h)) { [void][W]::ShowWindow($h, 4); Start-Sleep -Milliseconds 700 }   # SW_SHOWNOACTIVATE
    $r = New-Object W+RECT
    [void][W]::GetWindowRect($h, [ref]$r)
    $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    [void][W]::PrintWindow($h, $hdc, 2)   # PW_RENDERFULLCONTENT
    $g.ReleaseHdc($hdc)
    $bmp.Save($p, [Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    if ((Get-Item $p).Length -gt 10000) { break }
    Start-Sleep -Milliseconds 700
  }
  $len = (Get-Item $p).Length
  "  shot: $p ($len bytes)"
  Check ($len -gt 10000) "$name is not a blank capture"
}
function Trace { if (Test-Path $traceFile) { try { [IO.File]::ReadAllText($traceFile, [Text.Encoding]::UTF8) } catch { '' } } else { '' } }
function Status($t) { if ($t -match 'status=(\w+)') { $Matches[1] } else { '' } }
# 入力欄の下枠 (罫線だけの行) より下＝フッター。ccnest の判定と同じ範囲。
$RULE = '^' + [char]0x2500 + '{3,}$'
function Footer($t) {
  $lines = @($t -split "`n")
  $last = -1
  for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i].Trim() -match $RULE) { $last = $i } }
  if ($last -lt 0) { return '' }
  ($lines[($last + 1)..($lines.Count)] | Where-Object { $_ -and $_.Trim() }) -join "`n"
}
function Wait-Trace([scriptblock]$cond, [int]$sec) {
  $deadline = (Get-Date).AddSeconds($sec)
  do { $t = Trace; if ($t -and (& $cond $t)) { return $t }; Start-Sleep -Milliseconds 300 } while ((Get-Date) -lt $deadline)
  return $null
}
function Save($t, $name) { if ($t) { [IO.File]::WriteAllText((Join-Path $OutDir $name), $t, (New-Object Text.UTF8Encoding $false)) } }
function Type-Line($ccId, $text) {
  Start-Process powershell.exe -WindowStyle Hidden -Wait -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$typer`"", '-Inner', 'type', '-ProcId', $ccId, '-Out', "`"$typeOut`"", '-Text', "`"$text`"")
}
function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }

$PROMPT = [string][char]0x276F
$SPIN = [char]0x2026 + ' *\((\d+h )?(\d+m )?\d+s( |\))'
# 裏のエージェント待ちの行 (ターン終了の行の差し替え)。
$WAIT = 'Waiting for \d+ background agents? to finish'
# 経過秒がまだ付いていないスピナー行 ("* Tinkering…")。
$BARE_SPIN = '(?m)^\s*\S \S+' + [char]0x2026 + '\s*$'
# その行が「入力欄のすぐ上の最後の行」か (ccnest の判定と同じ見方)。古い行の下に別の内容が
# 足されていれば偽。
function Waiting-Is-Last($t) {
  $lines = @($t -split "`n")
  $idx = -1
  for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i] -match $WAIT) { $idx = $i } }
  if ($idx -lt 0) { return $false }
  for ($i = $idx + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i].Contains(([string][char]0x2500) * 3)) { return $true }
    if ($lines[$i].Trim()) { return $false }
  }
  return $false
}
$cc = $null; $hwnd = [IntPtr]::Zero; $sessionId = $null
try {
  Start-Process wt.exe -ArgumentList @('-w', 'new', '--size', '110,26', '-d', "`"$work`"", 'cmd.exe', '/c', "`"$runCmd`"")
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $cc = Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1
  } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw 'no new ccnest' }
  "ccnest pid: $($cc.Id)"
  $deadline = (Get-Date).AddSeconds(15)
  do { Start-Sleep -Milliseconds 500; $hwnd = [W]::Find($tag) } while ($hwnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
  if ($hwnd -eq [IntPtr]::Zero) { throw "window '$tag' not found" }
  [void][W]::MoveWindow($hwnd, 0, 0, 1500, 900, $true)

  # 1. 入力待ち
  $t = Wait-Trace { param($t) $t.Contains($PROMPT) -and (Footer $t) } 60
  Check ([bool]$t) 'claude prompt and footer are up'
  Start-Sleep -Seconds 3
  $t = Trace; Save $t 'trace-1-idle.txt'
  "  status (idle): $(Status $t)"
  Check ((Status $t) -eq 'NeedsAttention') 'idle is NeedsAttention'
  Shot $hwnd 'background-marker-1-idle.png'
  $cl = Descendants $cc.Id | Where-Object { $_.CommandLine -match '--session-id\s+([0-9a-f-]{36})' } | Select-Object -First 1
  if ($cl -and ($cl.CommandLine -match '--session-id\s+([0-9a-f-]{36})')) { $sessionId = $Matches[1] }
  "claude session: $sessionId"

  if ($Decision) {
  # 返答の文面はファイルから読ませる (日本語をキー入力で送らないため)。
  $utf8 = New-Object Text.UTF8Encoding $false
  $HEAD = 'ゆうさんの判断待ち'
  $ITEM = 'マーク: A か B のどれにするか'
  $NONE = '今はなし'
  [IO.File]::WriteAllText((Join-Path $work 'reply1.txt'), "▼ $HEAD`n・$ITEM`n", $utf8)
  [IO.File]::WriteAllText((Join-Path $work 'reply2.txt'), "▼ $HEAD`n・$NONE`n", $utf8)
  $quiet = { param($t) ($t -notmatch $SPIN) -and ($t -notmatch $BARE_SPIN) -and $t.Contains($PROMPT) }

  # 2. 裏のシェルを動かしたまま、返答が「判断待ち (中身あり)」で終わる → こちらの番
  Type-Line $cc.Id "Use a shell tool with run_in_background set to true to run the command ping -n $SleepSec 127.0.0.1. Do not wait for it and do not check its output. Then read the file reply1.txt in the current folder and reply with exactly its contents and nothing else."
  $t = Wait-Trace { param($t) (& $quiet $t) -and ((Footer $t) -match '\b1 shell\b') -and $t.Contains($ITEM) } 120
  Shot $hwnd 'background-marker-2-decision.png'
  Save $t 'trace-2-decision.txt'
  Check ([bool]$t) "reply ended with the decision section while '1 shell' is in the footer"
  if ($t) {
    "  footer: " + ((Footer $t) -split "`n" | Where-Object { $_ -match 'shell' } | Select-Object -First 1).Trim()
    "  status (decision pending): $(Status $t)"
    Check ((Status $t) -eq 'NeedsAttention') 'status is NeedsAttention: a decision is pending although a background shell runs'
  }

  # 3. 次の返答の判断待ちが「今はなし」→ 裏で実行中に戻る (シェルはまだ動いている)
  Type-Line $cc.Id "Read the file reply2.txt in the current folder and reply with exactly its contents and nothing else."
  $t = Wait-Trace { param($t) (& $quiet $t) -and $t.Contains($NONE) } 120
  Shot $hwnd 'background-marker-3-none.png'
  Save $t 'trace-3-none.txt'
  Check ([bool]$t) "reply ended with the decision section saying none"
  if ($t) {
    Check ((Footer $t) -match '\b1 shell\b') "the background shell is still running ('1 shell' in the footer)"
    "  status (no decision): $(Status $t)"
    Check ((Status $t) -eq 'Background') 'status is Background: nothing to decide, the background shell still runs'
  }
  } elseif ($Agent) {
  # 2. 返答中
  Type-Line $cc.Id "Use the Agent tool with run_in_background set to true to start one general-purpose agent. Its only job is to run the shell command ping -n $SleepSec 127.0.0.1 once in the foreground (not in the background), wait for it to end and then answer done. Do not wait for the agent and do not check on it. Then reply with only the word started."
  $t = Wait-Trace { param($t) ((Status $t) -eq 'Busy') -or (Waiting-Is-Last $t) } 90
  if ($t -and (Status $t) -eq 'Busy') {
    Save $t 'trace-2-busy.txt'
    "  status (replying): Busy"
    Start-Sleep -Milliseconds 500
    Shot $hwnd 'background-marker-2-busy.png'
  } else { "  (reply was too short to catch Busy on a 2 s tick; skipped)" }

  # 3. 返答は終わり、裏のエージェントが動いている: スピナー行が無く、入力欄のすぐ上が待ちの行
  $t = Wait-Trace { param($t) ($t -notmatch $SPIN) -and (Waiting-Is-Last $t) } 120
  #    待っている間の見た目を先に撮る (エージェントが早く終わると次のターンが写る)。
  Shot $hwnd 'background-marker-3-background.png'
  Save $t 'trace-3-background.txt'
  Check ([bool]$t) "reply ended with the 'Waiting for 1 background agent to finish' line above the input box"
  if ($t) {
    "  line: " + ((($t -split "`n") | Where-Object { $_ -match $WAIT } | Select-Object -Last 1)).Trim()
    "  footer: " + (((Footer $t) -split "`n") -join ' | ')
    "  status (background): $(Status $t)"
    Check ((Status $t) -eq 'Background') 'status is Background while only the background agent runs'
  }

  # 4. 裏のエージェントが終わり、通知を受けた返答も終わる: スピナー行が無く、待ちの行が最後ではない
  #    通知を受けたターンの出だしは経過秒の無いスピナー行 ("* Tinkering…") なので、それも待つ。
  #    落ち着いたとみなすのは、同じ条件が 6 秒あけて 2 回続いたとき。
  $settled = { param($t) ($t -notmatch $SPIN) -and ($t -notmatch $BARE_SPIN) -and -not (Waiting-Is-Last $t) -and $t.Contains($PROMPT) -and ((Footer $t) -notmatch '\b(shells?|monitors?)\b') }
  $deadline = (Get-Date).AddSeconds($SleepSec + 300)
  $t = $null
  do {
    $first = Wait-Trace $settled 20
    if ($first) { Start-Sleep -Seconds 6; $again = Trace; if (& $settled $again) { $t = $again } }
  } while (-not $t -and (Get-Date) -lt $deadline)
  Check ([bool]$t) 'background agent finished (no spinner, the waiting line is no longer the last line)'
  if (-not $t) { $t = Trace }
  Save $t 'trace-4-done.txt'
  "  status (done): $(Status $t)"
  "  footer: " + (((Footer $t) -split "`n") -join ' | ')
  "  stale waiting line left on screen: $([bool]($t -match $WAIT))"
  Check ((Status $t) -eq 'NeedsAttention') 'status is back to NeedsAttention after the background agent finished'
  Shot $hwnd 'background-marker-4-done.png'
  } else {
  # 2. 返答中
  Type-Line $cc.Id "Use a shell tool with run_in_background set to true to run the command sleep $SleepSec. Do not wait for it and do not check its output. Then reply with only the word started."
  #    返答が 2 秒 tick より短いと Busy を 1 度も見ないまま終わることがあるので、ここは記録だけ
  #    (🟢 の確認は status-marker-wt.ps1 の役目)。裏のシェルの札が先に出たら次へ進む。
  $t = Wait-Trace { param($t) ((Status $t) -eq 'Busy') -or ((Footer $t) -match '\b1 shell\b') } 60
  if ($t -and (Status $t) -eq 'Busy') {
    Save $t 'trace-2-busy.txt'
    "  status (replying): Busy"
    "  spinner: " + ((($t -split "`n") | Where-Object { $_ -match $SPIN } | Select-Object -First 1)).Trim()
    Start-Sleep -Milliseconds 500
    Shot $hwnd 'background-marker-2-busy.png'
  } else { "  (reply was too short to catch Busy on a 2 s tick; skipped)" }

  # 3. 返答は終わり、裏のシェルが動いている: スピナー行が無く、フッターに "1 shell" の札
  $t = Wait-Trace { param($t) ($t -notmatch $SPIN) -and ((Footer $t) -match '\b1 shell\b') } 90
  Save $t 'trace-3-background.txt'
  Check ([bool]$t) "reply ended with '1 shell' in the footer (background shell is running)"
  if ($t) {
    "  footer: " + ((Footer $t) -split "`n" | Where-Object { $_ -match 'shell' } | Select-Object -First 1).Trim()
    "  status (background): $(Status $t)"
    Check ((Status $t) -eq 'Background') 'status is Background while only the background shell runs'
  }
  Start-Sleep -Milliseconds 500
  Shot $hwnd 'background-marker-3-background.png'

  # 4. 裏のシェルが終わり、通知を受けた返答も終わる: スピナー行もフッターの札も無い
  $t = Wait-Trace { param($t) ($t -notmatch $SPIN) -and ((Footer $t) -notmatch '\bshells?\b') } ($SleepSec + 150)
  Check ([bool]$t) 'background shell finished (no spinner, no shell pill in the footer)'
  Start-Sleep -Seconds 3
  $t = Trace; Save $t 'trace-4-done.txt'
  "  status (done): $(Status $t)"
  "  stale 'still running' line left on screen: $([bool]($t -match 'still running'))"
  Check ((Status $t) -eq 'NeedsAttention') 'status is back to NeedsAttention after the background shell finished'
  Shot $hwnd 'background-marker-4-done.png'
  }

  if ($fails -eq 0) { 'PASS' } else { "FAIL ($fails)" }
} finally {
  if ($cc) { Kill-Tree $cc.Id }
  if ($hwnd -ne [IntPtr]::Zero) { [void][W]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
  Start-Sleep -Seconds 1
  if ($sessionId) {
    $enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
    $proj = Join-Path $env:USERPROFILE ".claude\projects\$enc"
    Remove-Item (Join-Path $proj "$sessionId.jsonl") -Force -ErrorAction SilentlyContinue
    if ((Test-Path $proj) -and -not (Get-ChildItem $proj -Force)) { Remove-Item $proj -Force }
  }
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
