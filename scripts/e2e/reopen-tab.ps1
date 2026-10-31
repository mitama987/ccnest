# Ctrl+Shift+T で閉じたタブを開き直す (v0.1.29) の実機確認。
# 使い捨て ccnest を conhost で起動し (偽 claude = 待つだけの cmd)、その conhost の入力に
# WriteConsoleInputW でキーを直接入れる (SendInput は使わない＝ユーザー操作と衝突しない)。
#  Ctrl+T → タブ 2 の偽の会話ログを置く → Ctrl+W → Ctrl+Shift+T → `--resume <id2>` で起動したか
#  → もう 1 回 Ctrl+Shift+T → 「開き直せる閉じたタブはありません」
# ユーザーの ccnest には触らない。
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
  // Ctrl(+Shift)+<letter>。uChar は制御文字 (Ctrl+T = 0x14)。
  public static void CtrlKey(char letter, bool shift) {
    uint st = 0x8 | (shift ? 0x10u : 0u);
    short vk = (short)char.ToUpper(letter);
    char ch = (char)(char.ToUpper(letter) - 'A' + 1);
    var recs = new INPUT_RECORD[] {
      new INPUT_RECORD { EventType = 1, KeyDown = 1, Repeat = 1, Vk = vk, Ch = ch, Ctrl = st },
      new INPUT_RECORD { EventType = 1, KeyDown = 0, Repeat = 1, Vk = vk, Ch = ch, Ctrl = st },
    };
    uint n; WriteConsoleInputW(inH, recs, (uint)recs.Length, out n);
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  switch ($Inner) {
    'screen' { }
    'ctrl-t' { [K]::CtrlKey('t', $false) }
    'ctrl-w' { [K]::CtrlKey('w', $false) }
    'ctrl-shift-t' { [K]::CtrlKey('t', $true) }
  }
  Start-Sleep -Milliseconds 200
  $text = [K]::Screen()
  [void][K]::FreeConsole()
  [IO.File]::WriteAllText($Out, $text, (New-Object Text.UTF8Encoding $false))
  exit 0
}

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "ccnest-e2e-reopen-tab"
New-Item -ItemType Directory -Force $work | Out-Null
$shim = Join-Path $work 'claude-shim.cmd'
Set-Content $shim "@powershell.exe -NoProfile -Command Start-Sleep -Seconds 600" -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim
$screenFile = Join-Path $work 'screen.txt'

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
function Shims($rootId, $flag) {
  @(Descendants $rootId | Where-Object { $_.Name -eq 'cmd.exe' -and $_.CommandLine -match "$flag\s+([0-9a-f-]{36})" } | ForEach-Object { [void]($_.CommandLine -match "$flag\s+([0-9a-f-]{36})"); $Matches[1] })
}
function Dump($rootId) { Descendants $rootId | Where-Object { $_.Name -eq 'cmd.exe' } | ForEach-Object { "    shim: " + ($_.CommandLine -replace '^.*claude-shim\.cmd"?\s*', '') } }
function Send($ccId, $what) {
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', $what, '-ProcId', $ccId, '-Out', "`"$screenFile`"") -WindowStyle Hidden -Wait
  Get-Content $screenFile -Raw -Encoding UTF8
}

$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory $work -PassThru
$fake = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  Start-Sleep -Seconds 6
  $cc = (Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1).ProcessId
  if (-not $cc) { throw "no ccnest.exe under conhost $($p.Id)" }
  $id1 = @(Shims $cc '--session-id')[0]
  "tab1 session: $id1"

  [void](Send $cc 'ctrl-t'); Start-Sleep -Seconds 3
  Dump $cc
  $id2 = @(Shims $cc '--session-id' | Where-Object { $_ -ne $id1 })[0]
  "tab2 session: $id2"
  Check ($null -ne $id2) "Ctrl+T opened tab 2"

  $enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
  $dir = Join-Path $env:USERPROFILE ".claude\projects\$enc"
  New-Item -ItemType Directory -Force $dir | Out-Null
  $fake = Join-Path $dir "$id2.jsonl"
  [IO.File]::WriteAllText($fake, ('{"type":"user","message":{"role":"user","content":"reopen test"},"sessionId":"' + $id2 + '"}' + "`n"), (New-Object Text.UTF8Encoding $false))

  [void](Send $cc 'ctrl-w'); Start-Sleep -Seconds 2
  Dump $cc
  Check (@(Shims $cc '--session-id') -notcontains $id2) "Ctrl+W closed tab 2 (its claude is gone)"
  Check (@(Shims $cc '--session-id') -contains $id1) "tab 1 is still running"

  [void](Send $cc 'ctrl-shift-t'); Start-Sleep -Seconds 3
  Dump $cc
  $resumed = @(Shims $cc '--resume')
  "resumed sessions: $($resumed -join ', ')"
  Check ($resumed -contains $id2) "Ctrl+Shift+T started claude with --resume <tab2 session>"
  $screen = Send $cc 'screen'
  "tab row: " + (($screen -split "`n")[0]).TrimEnd()

  $screen = Send $cc 'ctrl-shift-t'; Start-Sleep -Milliseconds 800
  $screen = Send $cc 'screen'
  Check ($screen -match '開き直せる閉じたタブはありません') "second Ctrl+Shift+T says there is nothing to reopen"
  Check (@(Shims $cc '--resume').Count -eq 1) "no extra tab was opened"
  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  if ($fake) { Remove-Item (Split-Path $fake) -Recurse -Force -ErrorAction SilentlyContinue }
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
