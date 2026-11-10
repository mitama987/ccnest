# `ccnest --add` (v0.1.35): 動いている窓にタブを足す、の実機確認。
# 使い捨て ccnest を conhost で起動し (偽 claude = 引数を表示して待つだけの cmd)、
# 別プロセスの `ccnest --add --to <pid> --resume ...` で依頼を送る。
#  1. 2 本送る → 終了コード 0・偽 claude が `--resume <id>` で 2 つ起動・タブ名が出る・
#     表示中のタブは元のまま・偽 claude が受け取った CCNEST_PID が窓の pid
#  2. 同じセッションをもう一度 → 「すでに開いています」で増えない (終了コード 0)
#  3. 会話ログの無いセッション → 「会話ログがありません」で開かない (終了コード 1)
#  4. 動いていない pid 宛て → すぐエラー (終了コード 1)
# ユーザーの ccnest には触らない (キー入力の注入も無い。画面は読むだけ)。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$Inner, [int]$ProcId, [string]$Out
)
$ErrorActionPreference = 'Stop'

if ($Inner) {
  trap { ("inner error: " + $_) | Set-Content $Out -Encoding UTF8; exit 1 }
  Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class K {
  [DllImport("kernel32.dll")] public static extern bool FreeConsole();
  [DllImport("kernel32.dll")] public static extern bool AttachConsole(uint pid);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr CreateFileW(string n, uint a, uint s, IntPtr sa, uint c, uint f, IntPtr t);
  [StructLayout(LayoutKind.Sequential)] public struct COORD { public short X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct SMALL_RECT { public short L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct INFO { public COORD Size; public COORD Cur; public short Attr; public SMALL_RECT Win; public COORD Max; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct CHAR_INFO { public char Ch; public short Attr; }
  [DllImport("kernel32.dll")] public static extern bool GetConsoleScreenBufferInfo(IntPtr h, out INFO i);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern bool ReadConsoleOutputW(IntPtr h, [Out] CHAR_INFO[] buf, COORD size, COORD at, ref SMALL_RECT r);
  static IntPtr outH; static INFO info;
  public static bool Attach(uint pid) {
    FreeConsole();
    if (!AttachConsole(pid)) return false;
    outH = CreateFileW("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    return GetConsoleScreenBufferInfo(outH, out info);
  }
  public static string Screen() {
    var sb = new StringBuilder();
    short w = info.Size.X;
    for (int r = info.Win.T; r <= info.Win.B; r++) {
      var buf = new CHAR_INFO[w];
      var rect = new SMALL_RECT { L = 0, T = (short)r, R = (short)(w - 1), B = (short)r };
      ReadConsoleOutputW(outH, buf, new COORD { X = w, Y = 1 }, new COORD { X = 0, Y = 0 }, ref rect);
      foreach (var c in buf) if ((c.Attr & 0x200) == 0) sb.Append(c.Ch);
      sb.Append('\n');
    }
    return sb.ToString();
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  $text = [K]::Screen()
  [void][K]::FreeConsole()
  [IO.File]::WriteAllText($Out, $text, (New-Object Text.UTF8Encoding $false))
  exit 0
}

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
# 入れ子 (ccnest のタブの中から実行) でも、宛先は必ず --to で渡す使い捨ての窓にする。
Remove-Item env:CCNEST_PID -ErrorAction SilentlyContinue
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "ccnest-e2e-add-tab"
New-Item -ItemType Directory -Force $work | Out-Null
$shimLog = Join-Path $work 'shim.log'
Remove-Item $shimLog -ErrorAction SilentlyContinue
$shim = Join-Path $work 'claude-shim.cmd'
# 偽 claude: 受け取った CCNEST_PID と引数を記録し、画面にも引数を出して待つ。
# `title claude` = 本物の起動直後と同じ汎用タイトル。ccnest はこれをタスク名として
# 扱わないので、タブには渡したタブ名がそのまま出る (cmd 既定のタイトルだと上書きされる)。
Set-Content $shim @(
  '@echo off',
  'title claude',
  "echo %CCNEST_PID% %*>>`"$shimLog`"",
  'echo SHIM %*',
  'powershell.exe -NoProfile -Command Start-Sleep -Seconds 600'
) -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim
$screenFile = Join-Path $work 'screen.txt'

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
function Shims($rootId, $flag) {
  @(Descendants $rootId | Where-Object { $_.Name -eq 'cmd.exe' -and $_.CommandLine -match "$flag\s+([0-9a-f-]{36})" } | ForEach-Object { [void]($_.CommandLine -match "$flag\s+([0-9a-f-]{36})"); $Matches[1] })
}
function Screen($ccId) {
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', 'screen', '-ProcId', $ccId, '-Out', "`"$screenFile`"") -WindowStyle Hidden -Wait
  Get-Content $screenFile -Raw -Encoding UTF8
}
# `ccnest --add ...` を実行して (終了コード, 標準出力, 標準エラー, 秒) を返す。
function Add($argList) {
  $o = Join-Path $work 'add.out'; $e = Join-Path $work 'add.err'
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $proc = Start-Process -FilePath $exe -ArgumentList $argList -NoNewWindow -Wait -PassThru -RedirectStandardOutput $o -RedirectStandardError $e
  $sw.Stop()
  $r = [pscustomobject]@{ Code = $proc.ExitCode; Out = (Get-Content $o -Raw -Encoding UTF8); Err = (Get-Content $e -Raw -Encoding UTF8); Sec = [math]::Round($sw.Elapsed.TotalSeconds, 1) }
  "  $ ccnest $($argList -join ' ')"
  "    exit=$($r.Code) ($($r.Sec)s)"
  foreach ($line in (("$($r.Out)`n$($r.Err)") -split "`r?`n" | Where-Object { $_.Trim() })) { "    | $line" }
  $script:last = $r
}
function Wait-Until($sec, $cond) { $until = (Get-Date).AddSeconds($sec); while ((Get-Date) -lt $until) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 500 }; return $false }

$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory $work -PassThru
$logDir = $null
$inbox = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  Start-Sleep -Seconds 6
  $cc = (Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1).ProcessId
  if (-not $cc) { throw "no ccnest.exe under conhost $($p.Id)" }
  $id1 = @(Shims $cc '--session-id')[0]
  "disposable ccnest pid: $cc / tab1 session: $id1"
  $inbox = Join-Path $env:APPDATA "ccnest\requests\$cc"
  Check (Test-Path $inbox) "the window made its request folder ($inbox)"

  # A・B は会話ログあり、C は無し。ログにユーザー発話は入れない
  # (最後の発話がタスク名になってタブ名を上書きするため)。
  $a = [guid]::NewGuid().ToString(); $b = [guid]::NewGuid().ToString(); $c = [guid]::NewGuid().ToString()
  $enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
  $logDir = Join-Path $env:USERPROFILE ".claude\projects\$enc"
  New-Item -ItemType Directory -Force $logDir | Out-Null
  foreach ($s in @($a, $b)) {
    [IO.File]::WriteAllText((Join-Path $logDir "$s.jsonl"), ('{"type":"file-history-snapshot","sessionId":"' + $s + '"}' + "`n"), (New-Object Text.UTF8Encoding $false))
  }
  $jp = "追加タブ"

  "1. add two sessions"
  Add @('--add', '--to', $cc, '--resume', "$a=first-tab", '--resume', "$b=$jp")
  Check ($last.Code -eq 0) "exit code 0"
  Check ($last.Out -match '2 件') "the reply says 2 tabs were accepted"
  $opened = Wait-Until 12 { $r = @(Shims $cc '--resume'); ($r -contains $a) -and ($r -contains $b) }
  Check $opened "both sessions started with claude --resume <id> (within 12 s)"
  Start-Sleep -Seconds 2
  $screen = Screen $cc
  $tabRow = (($screen -split "`n")[0]).TrimEnd()
  "  tab row: $tabRow"
  Check ($tabRow -match 'first-tab') "tab bar shows the given tab name (first-tab)"
  Check ($tabRow -match $jp) "tab bar shows the Japanese tab name ($jp)"
  Check (($screen -match [regex]::Escape($id1)) -and ($screen -notmatch [regex]::Escape($a)) -and ($screen -notmatch [regex]::Escape($b))) "the visible tab is still tab 1 (active tab not stolen)"
  $log = @(Get-Content $shimLog)
  "  shim log:"; $log | ForEach-Object { "    $_" }
  Check ($log.Count -eq 3 -and @($log | Where-Object { ($_ -split ' ')[0] -ne "$cc" }).Count -eq 0) "every pane got CCNEST_PID=$cc"

  "2. the same session again"
  Add @('--add', '--to', $cc, '--resume', $a)
  Check ($last.Code -eq 0) "exit code 0 (already a tab there)"
  Check ($last.Out -match 'すでに開いています') "the reply says it is already open"
  Start-Sleep -Seconds 5
  Check (@(Shims $cc '--resume' | Where-Object { $_ -eq $a }).Count -eq 1) "no second tab for the same session"

  "3. a session without a conversation log"
  Add @('--add', '--to', $cc, '--resume', $c)
  Check ($last.Code -eq 1) "exit code 1"
  Check ($last.Out -match '会話ログがありません') "the reply says there is no conversation log"
  Start-Sleep -Seconds 5
  Check (@(Shims $cc '--resume') -notcontains $c) "no tab was opened for it"
  Check (@(Shims $cc '--resume').Count -eq 2) "still exactly 2 added tabs"

  "4. a pid that is not running"
  $dead = (Start-Process cmd.exe -ArgumentList '/c', 'exit' -WindowStyle Hidden -PassThru -Wait).Id
  Add @('--add', '--to', $dead, '--resume', $a)
  Check ($last.Code -eq 1) "exit code 1"
  Check ($last.Sec -lt 3) "fails at once (no 10 s wait)"
  Check ($last.Err -match '動いていません') "the error says that ccnest is not running"

  Check (@(Get-ChildItem $inbox -ErrorAction SilentlyContinue).Count -eq 0) "the request folder is empty again (no leftovers)"
  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  if ($logDir) { Remove-Item $logDir -Recurse -Force -ErrorAction SilentlyContinue }
  # 強制終了では Drop が走らないので、使い捨ての窓の依頼フォルダはここで消す。
  if ($inbox) { Remove-Item $inbox -Recurse -Force -ErrorAction SilentlyContinue }
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
