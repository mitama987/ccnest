# タブをドラッグで並べ替える (v0.1.37) の実機確認。
# 使い捨て ccnest を conhost で起動し (偽 claude = タブごとに違う名前を出して待つだけの cmd)、
# その conhost の入力に WriteConsoleInputW でキーとマウスを直接入れる (SendInput は使わない
# ＝ユーザーのキーボード・マウス操作と衝突しない)。ユーザーの ccnest には触らない。
#
# タブ 3 枚 (alpha / bravo-long-tab-name / cc = 幅がばらばら) を作って:
#  A. alpha を 1 桁ずつ右端まで引く → bravo, cc, alpha。move_tab は 2 行だけ (往復しない)
#  B. alpha を 1 回の Drag で左端まで引く → alpha, bravo, cc。move_tab from=2 to=0
#  C. bravo を押してペインの行まで下げ、その行のまま右端まで引く → alpha, cc, bravo
#  D. alpha をクリックだけ → 並びは変わらない・move_tab は出ない・alpha が前面
# 判定はタブバー (画面 1 行目) の名前の並びと、CCNEST_INPUT_TRACE の `move_tab from=A to=B`。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$Inner, [int]$ProcId, [string]$Out, [string]$Path
)
$ErrorActionPreference = 'Stop'

if ($Inner) {
  trap { ("inner error: " + $_ + "`n" + $_.InvocationInfo.PositionMessage) | Set-Content $Out -Encoding UTF8; exit 1 }
  Add-Type @"
using System;
using System.Text;
using System.Threading;
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
  [StructLayout(LayoutKind.Explicit, CharSet=CharSet.Unicode)] public struct KEY_REC {
    [FieldOffset(0)] public short EventType;
    [FieldOffset(4)] public int KeyDown;
    [FieldOffset(8)] public short Repeat;
    [FieldOffset(10)] public short Vk;
    [FieldOffset(12)] public short Scan;
    [FieldOffset(14)] public char Ch;
    [FieldOffset(16)] public uint Ctrl;
  }
  [StructLayout(LayoutKind.Explicit)] public struct MOUSE_REC {
    [FieldOffset(0)] public short EventType;
    [FieldOffset(4)] public COORD Pos;
    [FieldOffset(8)] public uint Buttons;
    [FieldOffset(12)] public uint Ctrl;
    [FieldOffset(16)] public uint Flags;
  }
  [DllImport("kernel32.dll", EntryPoint="WriteConsoleInputW")] public static extern bool WriteKeys(IntPtr h, KEY_REC[] r, uint n, out uint w);
  [DllImport("kernel32.dll", EntryPoint="WriteConsoleInputW")] public static extern bool WriteMouse(IntPtr h, MOUSE_REC[] r, uint n, out uint w);
  static IntPtr outH, inH; static INFO info;
  public static bool Attach(uint pid) {
    FreeConsole();
    if (!AttachConsole(pid)) return false;
    outH = CreateFileW("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    inH = CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    return GetConsoleScreenBufferInfo(outH, out info);
  }
  // 画面の行 r を 1 セル 1 文字で返す (全角の 2 セル目は '\0')。attrs にセル属性。
  public static string Row(int r, out short[] attrs) {
    short w = info.Size.X;
    var buf = new CHAR_INFO[w];
    var rect = new SMALL_RECT { L = 0, T = (short)(info.Win.T + r), R = (short)(w - 1), B = (short)(info.Win.T + r) };
    ReadConsoleOutputW(outH, buf, new COORD { X = w, Y = 1 }, new COORD { X = 0, Y = 0 }, ref rect);
    var sb = new StringBuilder();
    attrs = new short[w];
    for (int i = 0; i < w; i++) { sb.Append((buf[i].Attr & 0x200) != 0 ? '\0' : buf[i].Ch); attrs[i] = buf[i].Attr; }
    return sb.ToString();
  }
  // Ctrl+<letter>。uChar は制御文字 (Ctrl+T = 0x14)。
  public static void CtrlKey(char letter) {
    short vk = (short)char.ToUpper(letter);
    char ch = (char)(char.ToUpper(letter) - 'A' + 1);
    var recs = new KEY_REC[] {
      new KEY_REC { EventType = 1, KeyDown = 1, Repeat = 1, Vk = vk, Ch = ch, Ctrl = 0x8 },
      new KEY_REC { EventType = 1, KeyDown = 0, Repeat = 1, Vk = vk, Ch = ch, Ctrl = 0x8 },
    };
    uint n; WriteKeys(inH, recs, (uint)recs.Length, out n);
  }
  // buttons: 1 = 左ボタンを押している。flags: 0 = 押す／離す、1 = 動いた (MOUSE_MOVED)。
  public static void Mouse(int col, int row, uint buttons, uint flags) {
    var p = new COORD { X = (short)col, Y = (short)(info.Win.T + row) };
    var recs = new MOUSE_REC[] { new MOUSE_REC { EventType = 2, Pos = p, Buttons = buttons, Ctrl = 0, Flags = flags } };
    uint n; WriteMouse(inH, recs, 1, out n);
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  switch ($Inner) {
    'row' { }
    'ctrl-t' { [K]::CtrlKey('t') }
    'drag' {
      # Path = "col,row;col,row;..."。最初の点で左ボタンを押し、残りの点へ 1 つずつ
      # 引いて (40 ms おき＝1 点ごとに描画が入る)、最後の点で離す。点が 1 つならクリック。
      $pts = @($Path -split ';' | ForEach-Object { , @($_ -split ',' | ForEach-Object { [int]$_ }) })
      [K]::Mouse($pts[0][0], $pts[0][1], 1, 0)
      Start-Sleep -Milliseconds 120
      for ($i = 1; $i -lt $pts.Count; $i++) {
        [K]::Mouse($pts[$i][0], $pts[$i][1], 1, 1)
        Start-Sleep -Milliseconds 40
      }
      $last = $pts[$pts.Count - 1]
      [K]::Mouse($last[0], $last[1], 0, 0)
    }
  }
  Start-Sleep -Milliseconds 400
  $attrs = $null
  $row = [K]::Row(0, [ref]$attrs)
  [void][K]::FreeConsole()
  # 1 行目 = タブバーの文字、2 行目 = セル属性 (16 進・空白区切り)。
  $lines = @($row, (($attrs | ForEach-Object { '{0:x}' -f ($_ -band 0xff) }) -join ' '))
  [IO.File]::WriteAllLines($Out, $lines, (New-Object Text.UTF8Encoding $false))
  exit 0
}

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
Remove-Item env:CCNEST_PID -ErrorAction SilentlyContinue
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$env:CCNEST_INPUT_TRACE = '1'
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "ccnest-e2e-drag-tab"
New-Item -ItemType Directory -Force $work | Out-Null
$counter = Join-Path $work 'n.txt'
Remove-Item $counter -ErrorAction SilentlyContinue
$shim = Join-Path $work 'claude-shim.cmd'
# 偽 claude: 起動した順に 1, 2, 3 と数え、タブごとに違う (幅も違う) タイトルを出して待つ。
# ccnest はペインのタイトルをタスク名としてタブに出す。
Set-Content $shim @(
  '@echo off',
  'setlocal',
  'set N=0',
  "if exist `"$counter`" set /p N=<`"$counter`"",
  'set /a N=N+1',
  ">`"$counter`" echo %N%",
  'if %N%==1 title alpha',
  'if %N%==2 title bravo-long-tab-name',
  'if %N%==3 title cc',
  'echo SHIM %N%',
  'powershell.exe -NoProfile -Command Start-Sleep -Seconds 600'
) -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim
$screenFile = Join-Path $work 'screen.txt'
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$names = @('alpha', 'bravo', 'cc')

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Send($ccId, $what, $path) {
  $a = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', $what, '-ProcId', $ccId, '-Out', "`"$screenFile`"")
  if ($path) { $a += @('-Path', "`"$path`"") }
  Start-Process powershell.exe -ArgumentList $a -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 2) { throw "inner failed: $($l -join ' ')" }
  @{ Row = $l[0]; Attrs = @($l[1] -split ' ') }
}
# タブバーの文字から、名前ごとの (開始桁, 終了桁) を返す。ラベルは "N:名前"。
function Tabs($s) {
  $t = @{}
  foreach ($n in $names) {
    $i = $s.Row.IndexOf(":$n")
    if ($i -lt 0) { return $null }
    $t[$n] = @{ Start = $i - 1; End = $i + $n.Length; Attr = $s.Attrs[$i + 1] }
  }
  $t
}
function Order($t) { ($names | Sort-Object { $t[$_].Start }) -join ', ' }
# 前面のタブ = セル属性がほかの 2 枚と違う 1 枚。
function Active($t) {
  $g = @($names | Group-Object { $t[$_].Attr } | Where-Object { $_.Count -eq 1 })
  if ($g.Count -eq 1) { $g[0].Group[0] } else { '?' }
}
function TraceLen { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 } }
function Moves($from) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match 'move_tab' } | ForEach-Object { [void]($_ -match 'move_tab from=\d+ to=\d+'); $Matches[0] }) }
function Steps($c0, $c1, $row) { ($c0..$c1 | ForEach-Object { "$_,$row" }) -join ';' }

$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory $work -PassThru
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  Start-Sleep -Seconds 6
  $cc = (Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1).ProcessId
  if (-not $cc) { throw "no ccnest.exe under conhost $($p.Id)" }
  [void](Send $cc 'ctrl-t'); Start-Sleep -Seconds 3
  [void](Send $cc 'ctrl-t'); Start-Sleep -Seconds 3
  $t = $null
  for ($i = 0; $i -lt 10 -and -not $t; $i++) { Start-Sleep -Seconds 2; $s = Send $cc 'row'; $t = Tabs $s }
  "tab row: " + ($s.Row -replace "`0", '').TrimEnd()
  if (-not $t) { throw "tab names did not appear" }
  Check ((Order $t) -eq 'alpha, bravo, cc') "start: alpha, bravo, cc"
  $right = $t['cc'].End - 1

  "A. drag alpha one column at a time to the right end (row 0)"
  $n0 = TraceLen
  $s = Send $cc 'drag' (Steps ($t['alpha'].Start + 2) $right 0); $t = Tabs $s
  "   tab row: " + ($s.Row -replace "`0", '').TrimEnd()
  $m = Moves $n0; "   trace: $($m -join ' / ')"
  Check ((Order $t) -eq 'bravo, cc, alpha') "order is bravo, cc, alpha"
  Check (($m -join '/') -eq 'move_tab from=0 to=1/move_tab from=1 to=2') "exactly 2 moves (no back-and-forth)"
  Check ((Active $t) -eq 'alpha') "the dragged tab is the active tab"

  "B. drag alpha to the left end in a single step"
  $n0 = TraceLen
  $s = Send $cc 'drag' "$($t['alpha'].Start + 2),0;$($t['bravo'].Start),0"; $t = Tabs $s
  "   tab row: " + ($s.Row -replace "`0", '').TrimEnd()
  $m = Moves $n0; "   trace: $($m -join ' / ')"
  Check ((Order $t) -eq 'alpha, bravo, cc') "order is alpha, bravo, cc"
  Check (($m -join '/') -eq 'move_tab from=2 to=0') "one move from=2 to=0"

  "C. press bravo, lower the pointer into the pane (row 6), drag right along row 6"
  $n0 = TraceLen
  $c0 = $t['bravo'].Start + 3
  $s = Send $cc 'drag' ("$c0,0;$c0,3;" + (Steps $c0 $right 6)); $t = Tabs $s
  "   tab row: " + ($s.Row -replace "`0", '').TrimEnd()
  $m = Moves $n0; "   trace: $($m -join ' / ')"
  Check ((Order $t) -eq 'alpha, cc, bravo') "order is alpha, cc, bravo"
  Check (($m -join '/') -eq 'move_tab from=1 to=2') "one move from=1 to=2"
  Check ((Active $t) -eq 'bravo') "the dragged tab is the active tab"

  "D. click alpha without dragging"
  $n0 = TraceLen
  $s = Send $cc 'drag' "$($t['alpha'].Start + 2),0"; $t = Tabs $s
  $m = Moves $n0
  Check ((Order $t) -eq 'alpha, cc, bravo') "order unchanged"
  Check ($m.Count -eq 0) "no move_tab line"
  Check ((Active $t) -eq 'alpha') "the click still switches to alpha"

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
