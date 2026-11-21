# タブをドラッグで並べ替える (v0.1.37) ＋ 引いている最中に浮かせる (v0.1.38) の実機確認。
# 使い捨て ccnest を conhost で起動し (偽 claude = タブごとに違う名前を出して待つだけの cmd)、
# その conhost の入力に WriteConsoleInputW でキーとマウスを直接入れる (SendInput は使わない
# ＝ユーザーのキーボード・マウス操作と衝突しない)。ユーザーの ccnest には触らない。
#
# タブ 3 枚 (alpha / bravo-long-tab-name / cc = 幅がばらばら) を作って:
#  E. alpha を押して 6 桁だけ右へ (離さない) → alpha の文字が 6 桁右に浮いている・並びは
#     動いていない → 離すと元の位置に収まる・move_tab は出ない
#  A. alpha を 1 桁ずつ右端まで引く → bravo, cc, alpha。move_tab は 2 行だけ (往復しない)
#  B. alpha を 1 回の Drag で左端まで引く → alpha, bravo, cc。move_tab from=2 to=0
#  C. bravo を押してペインの行まで下げ、その行のまま右端まで引く → alpha, cc, bravo
#  D. alpha をクリックだけ → 並びは変わらない・move_tab は出ない・alpha が前面
# 判定はタブバー (画面 1 行目) の名前の並びと、CCNEST_INPUT_TRACE の `move_tab from=A to=B`。
#
# -Wt を付けると Windows Terminal の新しい窓で起こし、引いている最中の見た目を PrintWindow で
# 2 枚撮る (浮いた直後／隣を越えた後)。判定は E だけ。-NoAnim は CCNEST_TAB_ANIM=off の確認
# (浮かない・並べ替えは同じ)。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [switch]$Wt, [switch]$NoAnim,
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-drag-tab-shots'),
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
  # Path = "col,row;col,row;..."。
  #  drag    = 最初の点で左ボタンを押し、残りの点へ 1 つずつ引いて (40 ms おき＝1 点ごとに
  #            描画が入る)、最後の点で離す。点が 1 つならクリック
  #  hold    = drag と同じだが離さない (引いている最中の画面を読む／撮るため)
  #  more    = 押したままの続き (押さずに、点へ 1 つずつ引く。離さない)
  #  release = 最初の点で離すだけ
  $pts = @()
  if ($Path) { $pts = @($Path -split ';' | ForEach-Object { , @($_ -split ',' | ForEach-Object { [int]$_ }) }) }
  switch ($Inner) {
    'row' { }
    'ctrl-t' { [K]::CtrlKey('t') }
    { $_ -in 'drag', 'hold', 'more' } {
      $first = 0
      if ($Inner -ne 'more') {
        [K]::Mouse($pts[0][0], $pts[0][1], 1, 0)
        Start-Sleep -Milliseconds 120
        $first = 1
      }
      for ($i = $first; $i -lt $pts.Count; $i++) {
        [K]::Mouse($pts[$i][0], $pts[$i][1], 1, 1)
        Start-Sleep -Milliseconds 40
      }
      if ($Inner -eq 'drag') {
        $last = $pts[$pts.Count - 1]
        [K]::Mouse($last[0], $last[1], 0, 0)
      }
    }
    'release' { [K]::Mouse($pts[0][0], $pts[0][1], 0, 0) }
  }
  # すべり (140 ms) が終わってから読む。
  Start-Sleep -Milliseconds 400
  $attrs = $null
  $row = [K]::Row(0, [ref]$attrs)
  [void][K]::FreeConsole()
  # 1 行目 = タブバーの文字、2 行目 = セル属性 (16 進・空白区切り)。
  $lines = @($row, (($attrs | ForEach-Object { '{0:x}' -f ($_ -band 0xff) }) -join ' '))
  [IO.File]::WriteAllLines($Out, $lines, (New-Object Text.UTF8Encoding $false))
  exit 0
}

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$tag = 'ccnest-e2e-drag-tab-' + (Get-Date -Format 'HHmmss')
$work = Join-Path $env:TEMP $tag
New-Item -ItemType Directory -Force $work | Out-Null
$counter = Join-Path $work 'n.txt'
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
# 環境変数は窓の中で決める (WT は呼び出し元の環境変数を引き継がないことがある)。
$run = Join-Path $work 'run.cmd'
$runLines = @(
  '@echo off',
  'set CCNEST_PID=',
  'set CCNEST_JP_TITLE=off',
  'set CCNEST_USAGE=off',
  'set CCNEST_INPUT_TRACE=1',
  "set CCNEST_CLAUDE_BIN=$shim"
)
if ($NoAnim) { $runLines += 'set CCNEST_TAB_ANIM=off' }
$runLines += "`"$exe`""
Set-Content $run $runLines -Encoding ASCII
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
    $t[$n] = @{ Start = $i - 1; End = $i + $n.Length; Colon = $i; Attr = $s.Attrs[$i + 1] }
  }
  $t
}
function Text($s) { ($s.Row -replace "`0", '').TrimEnd() }
function Order($t) { ($names | Sort-Object { $t[$_].Start }) -join ', ' }
# 前面のタブ = セル属性がほかの 2 枚と違う 1 枚。
function Active($t) {
  $g = @($names | Group-Object { $t[$_].Attr } | Where-Object { $_.Count -eq 1 })
  if ($g.Count -eq 1) { $g[0].Group[0] } else { '?' }
}
function TraceLen { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 } }
function Moves($from) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match 'move_tab' } | ForEach-Object { [void]($_ -match 'move_tab from=\d+ to=\d+'); $Matches[0] }) }
function Steps($c0, $c1, $row) { ($c0..$c1 | ForEach-Object { "$_,$row" }) -join ';' }

$hwnd = [IntPtr]::Zero
if ($Wt) {
  Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
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
  New-Item -ItemType Directory -Force $OutDir | Out-Null
}
# PrintWindow(PW_RENDERFULLCONTENT) = ほかの窓に隠れていても撮れる (前面に出さない)。
# 最小化されていると真っ黒になるので、前面にせず元の大きさへ戻してから撮る。
function Shot($name) {
  if ([W]::IsIconic($hwnd)) { [void][W]::ShowWindow($hwnd, 4); Start-Sleep -Milliseconds 500 }
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($hwnd, [ref]$r)
  $full = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [Drawing.Graphics]::FromImage($full)
  $hdc = $g.GetHdc()
  [void][W]::PrintWindow($hwnd, $hdc, 2)
  $g.ReleaseHdc($hdc); $g.Dispose()
  # タブバーのあたり (窓の上から 150px) だけ残す。
  $h = [Math]::Min(150, $full.Height)
  $crop = $full.Clone((New-Object Drawing.Rectangle 0, 0, $full.Width, $h), $full.PixelFormat)
  $p = Join-Path $OutDir $name
  $crop.Save($p, [Drawing.Imaging.ImageFormat]::Png)
  $crop.Dispose(); $full.Dispose()
  "   shot: $p ($((Get-Item $p).Length) bytes)"
}

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$p = $null; $cc = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  if ($Wt) {
    Start-Process wt.exe -ArgumentList @('-w', 'new', '--size', '110,14', '-d', "`"$work`"", 'cmd.exe', '/c', "`"$run`"")
  } else {
    # Hidden start; on-desktop2.ps1 moves the window to virtual desktop 2 before showing it.
    $p = Start-Process -FilePath "conhost.exe" -ArgumentList "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$PSScriptRoot\on-desktop2.ps1`" `"$run`"" -WorkingDirectory $work -WindowStyle Hidden -PassThru
  }
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $cc = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1).Id
  } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw "no new ccnest.exe" }
  if ($Wt) {
    $deadline = (Get-Date).AddSeconds(15)
    do { Start-Sleep -Milliseconds 500; $hwnd = [W]::Find($tag) } while ($hwnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
    if ($hwnd -eq [IntPtr]::Zero) { throw "window '$tag' not found" }
    [void][W]::MoveWindow($hwnd, 0, 0, 1500, 420, $true)
  }
  Start-Sleep -Seconds 5
  [void](Send $cc 'ctrl-t'); Start-Sleep -Seconds 3
  [void](Send $cc 'ctrl-t'); Start-Sleep -Seconds 3
  $t = $null
  for ($i = 0; $i -lt 10 -and -not $t; $i++) { Start-Sleep -Seconds 2; $s = Send $cc 'row'; $t = Tabs $s }
  "tab row: " + (Text $s)
  if (-not $t) { throw "tab names did not appear" }
  Check ((Order $t) -eq 'alpha, bravo, cc') "start: alpha, bravo, cc"
  $right = $t['cc'].End - 1

  "E. press alpha and pull it 6 columns to the right (button still down)"
  $n0 = TraceLen
  $c0 = $t['alpha'].Start + 2
  $at0 = $t['alpha'].Colon
  $s = Send $cc 'hold' (Steps $c0 ($c0 + 6) 0)
  "   tab row: " + (Text $s)
  $at = $s.Row.IndexOf(':alpha')
  if ($NoAnim) {
    Check ($at -eq $at0) "CCNEST_TAB_ANIM=off: alpha stays in its slot (col $at0 -> $at)"
  } else {
    Check ($at -eq $at0 + 6) "alpha floats under the pointer (col $at0 -> $at, expected $($at0 + 6))"
    Check ($s.Row.IndexOf('1:alpha') -ge 0) "it is still tab 1 (no reorder yet)"
  }
  if ($Wt) { Shot 'drag-float-1-lifted.png' }
  if ($Wt -and -not $NoAnim) {
    # 隣 (bravo) を越える所まで引き、すべりが終わってから撮る。
    # (cc のラベルは " <絵文字> 3:cc " なので、cc の数字の 7 桁左は bravo のタブの右寄り。)
    $over = $t['cc'].Start - 7
    $s = Send $cc 'more' (Steps ($c0 + 7) $over 0)
    "   tab row: " + (Text $s)
    Shot 'drag-float-2-passed.png'
    Check ((Moves $n0) -join '/' -eq 'move_tab from=0 to=1') "passing bravo moved alpha once (from=0 to=1)"
    $s = Send $cc 'release' "$over,0"; $t = Tabs $s
    "   tab row: " + (Text $s)
    Check ((Order $t) -eq 'bravo, alpha, cc') "after release: bravo, alpha, cc"
    Shot 'drag-float-3-dropped.png'
  } else {
    $s = Send $cc 'release' "$($c0 + 6),0"; $t = Tabs $s
    "   tab row: " + (Text $s)
    Check ($t['alpha'].Colon -eq $at0) "after release alpha is back in its slot (col $($t['alpha'].Colon))"
    Check ((Order $t) -eq 'alpha, bravo, cc') "order unchanged"
    Check ((Moves $n0).Count -eq 0) "no move_tab line"
  }

  if (-not $Wt) {
    "A. drag alpha one column at a time to the right end (row 0)"
    $n0 = TraceLen
    $s = Send $cc 'drag' (Steps ($t['alpha'].Start + 2) $right 0); $t = Tabs $s
    "   tab row: " + (Text $s)
    $m = Moves $n0; "   trace: $($m -join ' / ')"
    Check ((Order $t) -eq 'bravo, cc, alpha') "order is bravo, cc, alpha"
    Check (($m -join '/') -eq 'move_tab from=0 to=1/move_tab from=1 to=2') "exactly 2 moves (no back-and-forth)"
    Check ((Active $t) -eq 'alpha') "the dragged tab is the active tab"
    Check ($t['alpha'].End - 1 -eq $right) "alpha settled at the right end (col $($t['alpha'].End - 1))"

    "B. drag alpha to the left end in a single step"
    $n0 = TraceLen
    $s = Send $cc 'drag' "$($t['alpha'].Start + 2),0;$($t['bravo'].Start),0"; $t = Tabs $s
    "   tab row: " + (Text $s)
    $m = Moves $n0; "   trace: $($m -join ' / ')"
    Check ((Order $t) -eq 'alpha, bravo, cc') "order is alpha, bravo, cc"
    Check (($m -join '/') -eq 'move_tab from=2 to=0') "one move from=2 to=0"

    "C. press bravo, lower the pointer into the pane (row 6), drag right along row 6"
    $n0 = TraceLen
    $c0 = $t['bravo'].Start + 3
    $s = Send $cc 'drag' ("$c0,0;$c0,3;" + (Steps $c0 $right 6)); $t = Tabs $s
    "   tab row: " + (Text $s)
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
  }

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($cc) { Kill-Tree $cc }
  if ($p) { Kill-Tree $p.Id }
  if ($hwnd -ne [IntPtr]::Zero) { [void][W]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
  Start-Sleep -Milliseconds 800
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
