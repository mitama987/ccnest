# タブの状態マーカー 2 色化 (v0.1.31) の実機確認。本物の claude をペインに起こす。
# 使い捨て ccnest を conhost で起動し、その conhost の入力に WriteConsoleInputW で文字と Enter を
# 直接入れる (SendInput は使わない＝ユーザー操作と衝突しない)。画面はコンソールバッファから読む。
#
# 制約: conhost の ReadConsoleOutputW / ReadConsoleOutputCharacterW は非 BMP 文字 (🟢 U+1F7E2 /
# 🟨 U+1F7E8) を U+FFFD に潰して返すので、どちらの絵文字かはこの経路では読めない (2026-11-08 実測)。
# 読めるのは「幅 2 の 1 文字がタブ行の先頭にある」こと (先頭セル attr 0x100 LEADING + 次セル 0x200
# TRAILING)。🟢/🟨 と状態の対応は src/pane/status.rs の単体テストが固定している。ここで見るのは:
#  1. 起動直後 (入力待ち) のタブ行に幅 2 のマーカーが 1 つ、はみ出し無し
#  2. 指示を入れて実行中 ("esc to interrupt" が見える) でもマーカーが 1 つ (消えない・増えない)
#  3. 返答が終わって入力待ちに戻ってもマーカーが 1 つ
# Claude に 1 回だけ短い指示を送る (プラン利用量をごく僅かに使う)。ユーザーの ccnest には触らない。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$Inner, [int]$ProcId, [string]$Out, [string]$Text
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
  [StructLayout(LayoutKind.Explicit, CharSet=CharSet.Unicode)] public struct INPUT_RECORD {
    [FieldOffset(0)] public short EventType;
    [FieldOffset(4)] public int KeyDown;
    [FieldOffset(8)] public short Repeat;
    [FieldOffset(10)] public short Vk;
    [FieldOffset(12)] public short Scan;
    [FieldOffset(14)] public char Ch;
    [FieldOffset(16)] public uint Ctrl;
  }
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern bool WriteConsoleInputW(IntPtr h, INPUT_RECORD[] r, uint n, out uint w);
  static IntPtr outH, inH; static INFO info;
  public static bool Attach(uint pid) {
    FreeConsole();
    if (!AttachConsole(pid)) return false;
    outH = CreateFileW("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    inH = CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
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
  // タブ行 (画面 1 行目) の先頭セルを "<hex>/<attr hex>" で返す (幅 2 マーカーの検証用)。
  public static string Cells(int n) {
    short w = info.Size.X;
    var buf = new CHAR_INFO[w];
    var rect = new SMALL_RECT { L = 0, T = info.Win.T, R = (short)(w - 1), B = info.Win.T };
    ReadConsoleOutputW(outH, buf, new COORD { X = w, Y = 1 }, new COORD { X = 0, Y = 0 }, ref rect);
    var sb = new StringBuilder();
    for (int k = 0; k < n && k < w; k++) sb.AppendFormat("{0:X4}/{1:X3} ", (int)buf[k].Ch, buf[k].Attr);
    return sb.ToString().TrimEnd();
  }
  // 文字列をそのまま打つ (Vk 0・uChar に文字)。最後に Enter (Vk 0x0D・'\r')。
  public static void Type(string s, bool enter) {
    var list = new System.Collections.Generic.List<INPUT_RECORD>();
    foreach (char c in s) {
      list.Add(new INPUT_RECORD { EventType = 1, KeyDown = 1, Repeat = 1, Vk = 0, Ch = c, Ctrl = 0 });
      list.Add(new INPUT_RECORD { EventType = 1, KeyDown = 0, Repeat = 1, Vk = 0, Ch = c, Ctrl = 0 });
    }
    if (enter) {
      list.Add(new INPUT_RECORD { EventType = 1, KeyDown = 1, Repeat = 1, Vk = 0x0D, Ch = '\r', Ctrl = 0 });
      list.Add(new INPUT_RECORD { EventType = 1, KeyDown = 0, Repeat = 1, Vk = 0x0D, Ch = '\r', Ctrl = 0 });
    }
    var recs = list.ToArray();
    uint n; WriteConsoleInputW(inH, recs, (uint)recs.Length, out n);
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  switch ($Inner) {
    'screen' { }
    'type' { [K]::Type($Text, $false); Start-Sleep -Milliseconds 300; [K]::Type('', $true) }
  }
  Start-Sleep -Milliseconds 200
  $text = [K]::Screen()
  if ($Inner -eq 'cells') { $text = [K]::Cells(8) }
  [void][K]::FreeConsole()
  [IO.File]::WriteAllText($Out, $text, (New-Object Text.UTF8Encoding $false))
  exit 0
}

# 入れ子の Claude Code 判定 (CLAUDECODE 等) と本人のシム設定を外し、PATH の claude を使う。
Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$env:CCNEST_JP_TITLE = 'off'
$exe = (Resolve-Path $Exe).Path
$work = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$tmp = Join-Path $env:TEMP "ccnest-e2e-status-marker"
New-Item -ItemType Directory -Force $tmp | Out-Null
$screenFile = Join-Path $tmp 'screen.txt'
# conhost が非 BMP 文字を潰した跡。タブ行の先頭 " <marker> N:title" はこの経路では " \uFFFD N:title" に見える。
$MARK = [string][char]0xFFFD

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
function Send($ccId, $what, $text) {
  $args = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', $what, '-ProcId', $ccId, '-Out', "`"$screenFile`"")
  if ($text) { $args += @('-Text', "`"$text`"") }
  Start-Process powershell.exe -ArgumentList $args -WindowStyle Hidden -Wait
  Get-Content $screenFile -Raw -Encoding UTF8
}
function Wait-Screen($ccId, $pattern, $sec) {
  $deadline = (Get-Date).AddSeconds($sec)
  do { $s = Send $ccId 'screen'; if ($s -match $pattern) { return $s }; Start-Sleep -Milliseconds 500 } while ((Get-Date) -lt $deadline)
  return $s
}
function TabRow($screen) { ($screen -split "`n")[0].TrimEnd() }
# タブ行の先頭が「空白・幅 2 マーカー (LEADING 0x100 / TRAILING 0x200 の 2 セル)・空白」になっているか。
function Check-MarkerCells($ccId, $label) {
  $cells = Send $ccId 'cells'
  "  cells ($label): $cells"
  $c = $cells -split ' '
  $lead = $c[1] -split '/'; $trail = $c[2] -split '/'
  $ok = ($c[0] -like '0020/*') -and ($lead[0] -eq 'FFFD') -and (([Convert]::ToInt32($lead[1], 16) -band 0x100) -ne 0) `
        -and ($trail[0] -eq 'FFFD') -and (([Convert]::ToInt32($trail[1], 16) -band 0x200) -ne 0) -and ($c[3] -like '0020/*')
  Check $ok "$label tab row starts with one width-2 marker (lead 0x100 / trail 0x200) followed by a space"
}

$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory $work -PassThru
$sessionId = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  Start-Sleep -Seconds 4
  $cc = (Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1).ProcessId
  if (-not $cc) { throw "no ccnest.exe under conhost $($p.Id)" }
  $cl = (Descendants $cc | Where-Object { $_.CommandLine -match '--session-id\s+([0-9a-f-]{36})' } | Select-Object -First 1)
  if ($cl -and ($cl.CommandLine -match '--session-id\s+([0-9a-f-]{36})')) { $sessionId = $Matches[1] }
  "claude session: $sessionId"

  # 1. Claude Code の入力欄 (❯ プロンプト) が出るまで待ち、次の 2 秒 tick でタブ行にマーカー (🟨)。
  $screen = Wait-Screen $cc '[>\u276F]\s' 40
  Check ($screen -match '[>\u276F]\s') "claude prompt is up"
  $screen = Wait-Screen $cc ('^ ' + [regex]::Escape($MARK) + ' ') 8
  $row = TabRow $screen
  "  tab row (idle):    $row"
  Check ($row -match ('^ ' + [regex]::Escape($MARK) + ' \S')) "idle tab has exactly one marker then the label"
  Check-MarkerCells $cc 'idle'

  # 2. 数秒かかる指示を入れて実行中に (2 秒 tick で拾えるよう、返答が 1 秒で終わるものは避ける)。
  #    スピナー行 "✶ Boondoggling…(6s · ↓302 tokens)" (claude 2.1.319・経過秒つき) が見えている画面で
  #    タブ行のマーカーを見る (🟢 のはず・同定は単体テスト)。
  [void](Send $cc 'type' 'Print the numbers 1 to 400, one per line, no other text.')
  $SPIN = [char]0x2026 + '\(\d+s( |\))'
  $screen = Wait-Screen $cc $SPIN 40
  Check ($screen -match $SPIN) "spinner line with elapsed seconds '...(Ns' is on screen (claude is running)"
  "  spinner: " + ((($screen -split "`n") | Where-Object { $_ -match $SPIN } | Select-Object -First 1) -replace '^\s*\S\s*', '').Trim()
  Start-Sleep -Seconds 2   # ccnest の 2 秒 tick を 1 回またぐ
  $screen = Send $cc 'screen'
  $row = TabRow $screen
  "  tab row (running): $row"
  Check ($row -match ('^ ' + [regex]::Escape($MARK) + ' \S')) "running tab still has exactly one marker"
  Check-MarkerCells $cc 'running'

  # 3. 返答が終わって入力待ちに戻ってもマーカーは 1 つ (🟨 のはず)。
  $deadline = (Get-Date).AddSeconds(120)
  do { $screen = Send $cc 'screen'; if ($screen -notmatch $SPIN) { break }; Start-Sleep -Seconds 1 } while ((Get-Date) -lt $deadline)
  Check (-not ($screen -match $SPIN)) "spinner line gone after the reply"
  Start-Sleep -Seconds 2
  $screen = Send $cc 'screen'
  $row = TabRow $screen
  "  tab row (done):    $row"
  Check ($row -match ('^ ' + [regex]::Escape($MARK) + ' \S')) "finished tab has exactly one marker"
  Check-MarkerCells $cc 'done'

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  if ($sessionId) {
    $enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
    Remove-Item (Join-Path $env:USERPROFILE ".claude\projects\$enc\$sessionId.jsonl") -Force -ErrorAction SilentlyContinue
  }
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
