# Shift+Enter / Alt+Enter / Ctrl+Enter で Claude Code の入力欄に改行が入る (送信されない) ことの実機確認 (v0.1.32)。
# 使い捨て ccnest を conhost で起動し、本物の claude をペインに起こして、WriteConsoleInputW で
#  1. Enter + SHIFT_PRESSED   (ccnest が Shift+Enter を直接受けるとき)
#  2. Enter + LEFT_ALT_PRESSED (Windows Terminal の shift+enter → sendInput "\u001b\r" が ConPTY で
#                              Alt+Enter に合成されて届くとき = 実運用の経路。scripts/e2e/conpty-input-probe.py で実測)
#  3. Enter + LEFT_CTRL_PRESSED
# を入れ、その前後に文字を打って「入力欄が 2 行になり、スピナー行 (…(Ns) が出ない」ことを画面から確かめる。
# 各ケースの後は Ctrl+U (行クリア) ではなく Esc を 2 回… は rewind を開くので使わず、Ctrl+C 1 回で入力欄を空にする。
# 素の Enter は最後まで打たない (API を使わない)。ユーザーの ccnest には触らない。
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
  static void Push(System.Collections.Generic.List<INPUT_RECORD> l, short vk, char ch, uint ctrl) {
    l.Add(new INPUT_RECORD { EventType = 1, KeyDown = 1, Repeat = 1, Vk = vk, Ch = ch, Ctrl = ctrl });
    l.Add(new INPUT_RECORD { EventType = 1, KeyDown = 0, Repeat = 1, Vk = vk, Ch = ch, Ctrl = ctrl });
  }
  public static void Type(string s) {
    var l = new System.Collections.Generic.List<INPUT_RECORD>();
    foreach (char c in s) Push(l, 0, c, 0);
    var recs = l.ToArray(); uint n; WriteConsoleInputW(inH, recs, (uint)recs.Length, out n);
  }
  // Enter に修飾キーの状態を付けて 1 回だけ。SHIFT_PRESSED=0x10, LEFT_ALT_PRESSED=0x2, LEFT_CTRL_PRESSED=0x8。
  public static void EnterWith(uint ctrl) {
    var l = new System.Collections.Generic.List<INPUT_RECORD>();
    Push(l, 0x0D, '\r', ctrl);
    var recs = l.ToArray(); uint n; WriteConsoleInputW(inH, recs, (uint)recs.Length, out n);
  }
  public static void CtrlKey(char letter) {
    var l = new System.Collections.Generic.List<INPUT_RECORD>();
    Push(l, (short)char.ToUpper(letter), (char)(char.ToUpper(letter) - 'A' + 1), 0x8);
    var recs = l.ToArray(); uint n; WriteConsoleInputW(inH, recs, (uint)recs.Length, out n);
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  switch ($Inner) {
    'screen'      { }
    'type'        { [K]::Type($Text) }
    'shift-enter' { [K]::EnterWith(0x10) }
    'alt-enter'   { [K]::EnterWith(0x2) }
    'ctrl-enter'  { [K]::EnterWith(0x8) }
    'ctrl-c'      { [K]::CtrlKey('c') }
  }
  Start-Sleep -Milliseconds 300
  $text = [K]::Screen()
  [void][K]::FreeConsole()
  [IO.File]::WriteAllText($Out, $text, (New-Object Text.UTF8Encoding $false))
  exit 0
}

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$env:CCNEST_JP_TITLE = 'off'
$exe = (Resolve-Path $Exe).Path
$work = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$tmp = Join-Path $env:TEMP "ccnest-e2e-shift-enter"
New-Item -ItemType Directory -Force $tmp | Out-Null
$screenFile = Join-Path $tmp 'screen.txt'
$SPIN = [char]0x2026 + '\(\d+s( |\))'

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
# 入力欄 (❯ の行から次の ── 区切りまで) の行を返す。
function PromptRows($screen) {
  $rows = $screen -split "`n"
  $start = -1
  for ($i = 0; $i -lt $rows.Count; $i++) { if ($rows[$i] -match '^\s*\S?\s*[>\u276F]\s') { $start = $i } }
  if ($start -lt 0) { return @() }
  $out = @()
  for ($i = $start; $i -lt $rows.Count; $i++) { if ($i -gt $start -and $rows[$i] -match '\u2500{6}') { break }; $out += $rows[$i].TrimEnd() }
  return $out
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
  $screen = Wait-Screen $cc '[>\u276F]\s' 40
  Check ($screen -match '[>\u276F]\s') "claude prompt is up"

  foreach ($case in @(@('shift-enter', 'Shift+Enter'), @('alt-enter', 'Alt+Enter (= WT sendInput ESC CR route)'), @('ctrl-enter', 'Ctrl+Enter'))) {
    $what = $case[0]; $label = $case[1]
    $a = "abc" + (Get-Random -Maximum 999); $b = "def" + (Get-Random -Maximum 999)
    [void](Send $cc 'type' $a)
    [void](Send $cc $what)
    $screen = Send $cc 'type' $b
    $screen = Wait-Screen $cc ([regex]::Escape($b)) 10
    $rows = PromptRows $screen
    $rowA = ($rows | Where-Object { $_ -match [regex]::Escape($a) } | Select-Object -First 1)
    $rowB = ($rows | Where-Object { $_ -match [regex]::Escape($b) } | Select-Object -First 1)
    "  [$label] prompt rows:"; $rows | ForEach-Object { "      " + $_ }
    Check (($null -ne $rowA) -and ($null -ne $rowB) -and ($rowA -ne $rowB)) "$label puts '$a' and '$b' on different lines of the prompt"
    Check (-not ($screen -match $SPIN)) "$label did not submit (no spinner line)"
    Start-Sleep -Seconds 2
    $screen = Send $cc 'screen'
    Check (-not ($screen -match $SPIN) -and ($screen -match [regex]::Escape($b))) "$label still not submitted after 2 s (text remains in the prompt)"
    # 入力欄を空に (Ctrl+C 1 回 = 入力クリア。2 回だと ccnest が shell 化するので 1 回だけ)
    [void](Send $cc 'ctrl-c')
    Start-Sleep -Milliseconds 800
    $screen = Send $cc 'screen'
    Check (-not ($screen -match [regex]::Escape($b))) "prompt cleared before the next case"
  }

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
