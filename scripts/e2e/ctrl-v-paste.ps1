# Ctrl+V の貼り付け (v0.1.30) の実機確認。本物の claude をペインに起こす。
# 使い捨て ccnest を conhost で起動し、その conhost の入力に WriteConsoleInputW で
# Ctrl+V (uChar 0x16) を直接入れる (SendInput は使わない＝ユーザー操作と衝突しない)。
#  1. クリップボードに画像 → Ctrl+V → 画面に [Image #1] チップ (ccnest が ESC v を送り Claude が画像を読んだ)
#  2. クリップボードに文字列 → Ctrl+V → その文字列が入力欄に出る (bracketed-paste)
# クリップボードは実行前の文字列を控えて最後に戻す (画像など文字列以外は戻せない)。
# ユーザーの ccnest には触らない。作業フォルダはこのリポジトリ (claude の trust 済み)。
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
  // Ctrl+<letter>。uChar は制御文字 (Ctrl+V = 0x16)。
  public static void CtrlKey(char letter) {
    short vk = (short)char.ToUpper(letter);
    char ch = (char)(char.ToUpper(letter) - 'A' + 1);
    var recs = new INPUT_RECORD[] {
      new INPUT_RECORD { EventType = 1, KeyDown = 1, Repeat = 1, Vk = vk, Ch = ch, Ctrl = 0x8 },
      new INPUT_RECORD { EventType = 1, KeyDown = 0, Repeat = 1, Vk = vk, Ch = ch, Ctrl = 0x8 },
    };
    uint n; WriteConsoleInputW(inH, recs, (uint)recs.Length, out n);
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  switch ($Inner) {
    'screen' { }
    'ctrl-v' { [K]::CtrlKey('v') }
  }
  Start-Sleep -Milliseconds 200
  $text = [K]::Screen()
  [void][K]::FreeConsole()
  [IO.File]::WriteAllText($Out, $text, (New-Object Text.UTF8Encoding $false))
  exit 0
}

# 入れ子の Claude Code 判定 (CLAUDECODE 等) と本人のシム設定を外し、PATH の claude を使う。
Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$env:CCNEST_JP_TITLE = 'off'
$exe = (Resolve-Path $Exe).Path
$work = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$tmp = Join-Path $env:TEMP "ccnest-e2e-ctrl-v"
New-Item -ItemType Directory -Force $tmp | Out-Null
$screenFile = Join-Path $tmp 'screen.txt'

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
function Send($ccId, $what) {
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', $what, '-ProcId', $ccId, '-Out', "`"$screenFile`"") -WindowStyle Hidden -Wait
  Get-Content $screenFile -Raw -Encoding UTF8
}
function Wait-Screen($ccId, $pattern, $sec) {
  $deadline = (Get-Date).AddSeconds($sec)
  do { $s = Send $ccId 'screen'; if ($s -match $pattern) { return $s }; Start-Sleep -Milliseconds 500 } while ((Get-Date) -lt $deadline)
  return $s
}

# クリップボードの控え (文字列のときだけ戻せる)。
$savedText = $null
if ([System.Windows.Forms.Clipboard]::ContainsText()) { $savedText = [System.Windows.Forms.Clipboard]::GetText() }
$hadImage = [System.Windows.Forms.Clipboard]::ContainsImage()
"clipboard before: text=$($null -ne $savedText) image=$hadImage"

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

  # Claude Code の入力欄 (❯ プロンプト) が出るまで待つ。
  $screen = Wait-Screen $cc '[>\u276F]\s' 40
  Check ($screen -match '[>\u276F]\s') "claude prompt is up"

  # 1. 画像 → [Image #1]
  $bmp = New-Object System.Drawing.Bitmap 64, 32
  $g = [System.Drawing.Graphics]::FromImage($bmp); $g.Clear([System.Drawing.Color]::Orange); $g.Dispose()
  [System.Windows.Forms.Clipboard]::SetImage($bmp)
  Check ([System.Windows.Forms.Clipboard]::ContainsImage()) "clipboard holds an image"
  [void](Send $cc 'ctrl-v')
  $screen = Wait-Screen $cc '\[Image #1\]' 15
  Check ($screen -match '\[Image #1\]') "Ctrl+V with an image shows [Image #1]"
  if ($screen -notmatch '\[Image #1\]') { "---- screen ----"; $screen; "----------------" }

  # 2. 文字列 → そのまま入力欄へ
  $marker = "ccnest-ctrlv-" + (Get-Random -Maximum 99999)
  [System.Windows.Forms.Clipboard]::SetText($marker)
  [void](Send $cc 'ctrl-v')
  $screen = Wait-Screen $cc ([regex]::Escape($marker)) 10
  Check ($screen -match [regex]::Escape($marker)) "Ctrl+V with text pastes '$marker'"
  if ($screen -notmatch [regex]::Escape($marker)) { "---- screen ----"; $screen; "----------------" }
  "---- prompt rows ----"
  ($screen -split "`n") | Where-Object { $_ -match '\[Image #1\]|ccnest-ctrlv-' } | ForEach-Object { "  " + $_.TrimEnd() }

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  if ($null -ne $savedText) { [System.Windows.Forms.Clipboard]::SetText($savedText) } else { [System.Windows.Forms.Clipboard]::Clear() }
  if ($sessionId) {
    $enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
    Remove-Item (Join-Path $env:USERPROFILE ".claude\projects\$enc\$sessionId.jsonl") -Force -ErrorAction SilentlyContinue
  }
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
