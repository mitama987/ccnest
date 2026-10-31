# 折り返した URL の Ctrl+クリック (v0.1.28) の実機確認。
# 使い捨て ccnest を conhost で起動し (偽 claude = url-click-wrap-shim.ps1 が割れた URL を出す)、
# その conhost の入力に WriteConsoleInputW で Ctrl+左クリックを直接入れる。SendInput は使わない
# ので、ユーザーのキーボード・マウス操作と衝突しない。ユーザーの ccnest には触らない。
# ccnest は CCNEST_OPEN_URL_LOG があればブラウザを開かず URL をファイルに追記する。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [switch]$Inner, [int]$ProcId, [string]$Out
)
$ErrorActionPreference = 'Stop'
$expected = 'https://docs.google.com/document/d/1QZlfxaJs8FVooN-kZVKeb8lnJBc86sHAPZk9dFEMe_A/edit'

if ($Inner) {
  trap { ("inner error: " + $_ + "`n" + $_.InvocationInfo.PositionMessage) | Set-Content $Out -Encoding UTF8; exit 1 }
  Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
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
  [StructLayout(LayoutKind.Explicit)] public struct INPUT_RECORD {
    [FieldOffset(0)] public short EventType;
    [FieldOffset(4)] public COORD Pos;
    [FieldOffset(8)] public uint Buttons;
    [FieldOffset(12)] public uint Ctrl;
    [FieldOffset(16)] public uint Flags;
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
  public static int Top { get { return info.Win.T; } }
  public static int Rows { get { return info.Win.B - info.Win.T + 1; } }
  // 画面の行 r を 1 セル 1 文字で返す (全角の 2 セル目は '\0')。
  public static string Row(int r) {
    short w = info.Size.X;
    var buf = new CHAR_INFO[w];
    var rect = new SMALL_RECT { L = 0, T = (short)(info.Win.T + r), R = (short)(w - 1), B = (short)(info.Win.T + r) };
    ReadConsoleOutputW(outH, buf, new COORD { X = w, Y = 1 }, new COORD { X = 0, Y = 0 }, ref rect);
    var sb = new StringBuilder();
    foreach (var c in buf) sb.Append((c.Attr & 0x200) != 0 ? '\0' : c.Ch); // 0x200 = TRAILING_BYTE
    return sb.ToString();
  }
  public static void CtrlClick(int col, int row) {
    var p = new COORD { X = (short)col, Y = (short)(info.Win.T + row) };
    var recs = new INPUT_RECORD[] {
      new INPUT_RECORD { EventType = 2, Pos = p, Buttons = 1, Ctrl = 0x8, Flags = 0 },
      new INPUT_RECORD { EventType = 2, Pos = p, Buttons = 0, Ctrl = 0x8, Flags = 0 },
    };
    uint n; WriteConsoleInputW(inH, recs, (uint)recs.Length, out n);
  }
}
"@
  $log = New-Object Collections.Generic.List[string]
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  $rows = 0..([K]::Rows - 1) | ForEach-Object { [K]::Row($_) }
  # "https://docs" で始まる URL の行 (A と B の 2 か所) と、その次の行の "/edit" をクリックする。
  $targets = @()
  for ($r = 0; $r -lt $rows.Count - 1; $r++) {
    $c = $rows[$r].IndexOf('https://docs')
    if ($c -lt 0) { continue }
    $e = $rows[$r + 1].IndexOf('/edit')
    $targets += , @(($c + 12), $r, 'row1')
    if ($e -ge 0) { $targets += , @(($e + 2), ($r + 1), 'row2') }
  }
  $log.Add("screen rows with URL:")
  for ($r = 0; $r -lt $rows.Count; $r++) { if ($rows[$r] -match 'docs|edit') { $log.Add(("  [{0,2}] " -f $r) + ($rows[$r] -replace "`0", '').TrimEnd()) } }
  foreach ($t in $targets) {
    [K]::CtrlClick($t[0], $t[1]); $log.Add("ctrl+click $($t[2]) at col=$($t[0]) row=$($t[1])")
    Start-Sleep -Milliseconds 600
  }
  [void][K]::FreeConsole()
  [IO.File]::WriteAllLines($Out, $log, (New-Object Text.UTF8Encoding $false))
  exit 0
}

# Claude のセッション内から起動したときの env 漏れを止める
Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "ccnest-e2e-url-click"
New-Item -ItemType Directory -Force $work | Out-Null
$opened = Join-Path $work 'opened.txt'
$report = Join-Path $work 'inner.txt'
Remove-Item $opened, $report -ErrorAction SilentlyContinue
$shim = Join-Path $work 'claude-shim.cmd'
$shimPs = Join-Path $PSScriptRoot 'url-click-wrap-shim.ps1'
Set-Content $shim "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$shimPs`"" -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim
$env:CCNEST_OPEN_URL_LOG = $opened

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory $work -PassThru
try {
  Start-Sleep -Seconds 6
  $c = Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1
  if (-not $c) { throw "no ccnest.exe under conhost $($p.Id)" }
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', '-ProcId', $c.ProcessId, '-Out', "`"$report`"") -WindowStyle Hidden -Wait
  Get-Content $report -Encoding UTF8
  Start-Sleep -Seconds 1
  "opened.txt:"
  $got = @(if (Test-Path $opened) { Get-Content $opened -Encoding UTF8 })
  $got | ForEach-Object { "  $_" }
  $ok = $got.Count -eq 4 -and @($got | Where-Object { $_ -ne $expected }).Count -eq 0
  if ($ok) { "PASS: 4/4 clicks opened the full URL" } else { "FAIL: expected 4 x $expected" }
} finally {
  Kill-Tree $p.Id
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
