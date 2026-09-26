#Requires -Version 5.1
<#
  ccnest middle-click-closes-tab E2E (v0.1.13).

  Scenario A (classic console, conhost.exe): launch the freshly built ccnest in its OWN
  console window with CCNEST_INPUT_TRACE=1 and `claude` hidden from PATH (shell panes only),
  build 3 tabs (the last one split into 2 panes), then middle-click the leftmost tab three
  times. Expect: "close_tab idx=0 tabs_left=2/1/0" in the input trace, the cmd.exe child
  count dropping 4 -> 3 -> 2 -> 0, ccnest exiting cleanly on the last close, no crash log.

  Scenario B (Windows Terminal): open a NEW WT window with a unique name/title, calibrate the
  cell grid from two probe middle-clicks (the trace reports @col,row), then middle-click the
  only tab's label at row 0. Expect: Down(Middle) delivered through WT/ConPTY, one
  "close_tab idx=0 tabs_left=0", ccnest exits, the WT window closes.

  SAFETY: every injected key/click is gated on the foreground window being OUR window
  (and, for WT, its title carrying our unique token). Abort otherwise.
#>
param(
  [string]$Exe = 'C:\Users\mitam\Desktop\work\90_other\ccnest\target\release\ccnest.exe',
  [string]$Cwd = 'C:\Users\mitam\Desktop\work\90_other\ccnest',
  [int]$StartupWaitSec = 4,
  [switch]$SkipConhost,
  [switch]$SkipWt
)
$ErrorActionPreference = 'Stop'
$code = @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class Mmb {
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KINPUT { public uint type; public KEYBDINPUT ki; public long pad; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct MINPUT { public uint type; public MOUSEINPUT mi; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll", SetLastError=true, EntryPoint="SendInput")] static extern uint SendInputK(uint n, KINPUT[] inputs, int size);
  [DllImport("user32.dll", SetLastError=true, EntryPoint="SendInput")] static extern uint SendInputM(uint n, MINPUT[] inputs, int size);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr hWnd);
  [DllImport("user32.dll")] static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
  [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hWnd, out RECT r);
  [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr hWnd, ref POINT p);
  [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hWnd);
  const uint INPUT_KEYBOARD = 1, INPUT_MOUSE = 0, KEYEVENTF_KEYUP = 2;
  const uint MOUSEEVENTF_MIDDLEDOWN = 0x0020, MOUSEEVENTF_MIDDLEUP = 0x0040;
  public static int KSize() { return Marshal.SizeOf(typeof(KINPUT)); }
  public static int MSize() { return Marshal.SizeOf(typeof(MINPUT)); }
  public static uint PidOf(IntPtr hWnd) { uint pid; GetWindowThreadProcessId(hWnd, out pid); return pid; }
  public static uint ForegroundPid() { return PidOf(GetForegroundWindow()); }
  public static string Title(IntPtr hWnd) { var sb = new StringBuilder(512); GetWindowText(hWnd, sb, 512); return sb.ToString(); }
  static uint _unused;
  public static bool Focus(IntPtr hWnd) {
    KINPUT[] alt = new KINPUT[2];
    alt[0].type = INPUT_KEYBOARD; alt[0].ki.wVk = 0x12; alt[0].ki.dwFlags = 0;
    alt[1].type = INPUT_KEYBOARD; alt[1].ki.wVk = 0x12; alt[1].ki.dwFlags = KEYEVENTF_KEYUP;
    SendInputK(2, alt, KSize());
    ShowWindow(hWnd, 9);
    BringWindowToTop(hWnd);
    if (SetForegroundWindow(hWnd)) return true;
    uint fgThread = GetWindowThreadProcessId(GetForegroundWindow(), out _unused);
    uint myThread = GetCurrentThreadId();
    AttachThreadInput(myThread, fgThread, true);
    bool ok = SetForegroundWindow(hWnd);
    AttachThreadInput(myThread, fgThread, false);
    return ok;
  }
  // modifier down, key down, key up, modifier up (mod = 0 for a plain key)
  public static uint Chord(ushort mod, ushort vk) {
    int n = mod == 0 ? 2 : 4;
    KINPUT[] a = new KINPUT[n];
    int i = 0;
    if (mod != 0) { a[i].type = INPUT_KEYBOARD; a[i].ki.wVk = mod; a[i].ki.dwFlags = 0; i++; }
    a[i].type = INPUT_KEYBOARD; a[i].ki.wVk = vk; a[i].ki.dwFlags = 0; i++;
    a[i].type = INPUT_KEYBOARD; a[i].ki.wVk = vk; a[i].ki.dwFlags = KEYEVENTF_KEYUP; i++;
    if (mod != 0) { a[i].type = INPUT_KEYBOARD; a[i].ki.wVk = mod; a[i].ki.dwFlags = KEYEVENTF_KEYUP; }
    return SendInputK((uint)n, a, KSize());
  }
  public static uint MiddleClickAt(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(40);
    MINPUT[] d = new MINPUT[1]; d[0].type = INPUT_MOUSE; d[0].mi.dwFlags = MOUSEEVENTF_MIDDLEDOWN;
    uint r = SendInputM(1, d, MSize());
    System.Threading.Thread.Sleep(60);
    MINPUT[] u = new MINPUT[1]; u[0].type = INPUT_MOUSE; u[0].mi.dwFlags = MOUSEEVENTF_MIDDLEUP;
    SendInputM(1, u, MSize());
    return r;
  }
  // client rect of hWnd in screen (physical) pixels: L,T,R,B
  public static int[] ClientOnScreen(IntPtr hWnd) {
    RECT r; GetClientRect(hWnd, out r);
    POINT p; p.X = 0; p.Y = 0; ClientToScreen(hWnd, ref p);
    return new int[] { p.X, p.Y, p.X + r.R, p.Y + r.B };
  }
}
'@
Add-Type -TypeDefinition $code
[void][Mmb]::SetProcessDPIAware()
if ([Mmb]::KSize() -ne 40) { throw ("KINPUT size {0}, expected 40" -f [Mmb]::KSize()) }
if ([Mmb]::MSize() -ne 40) { throw ("MINPUT size {0}, expected 40" -f [Mmb]::MSize()) }

$traceDir = Join-Path $env:APPDATA 'ccnest'
$ilog = Join-Path $traceDir 'input-trace.log'
$outDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$report = New-Object System.Collections.Generic.List[string]
function Say([string]$s) { Write-Host $s; $report.Add($s) }
function Now() { return (Get-Date).ToString('HH:mm:ss.fff') }
function TraceLines() { if (Test-Path $ilog) { return @(Get-Content $ilog) } else { return @() } }
function CountMatches([string]$pat) { return @(TraceLines | Where-Object { $_ -match $pat }).Count }
function CrashLogs() { return @(Get-ChildItem $traceDir -Filter 'crash-*.log' -ErrorAction SilentlyContinue | ForEach-Object { $_.Name }) }
$crashBefore = CrashLogs

$VK_CONTROL = [uint16]0x11; $VK_MENU = [uint16]0x12; $VK_T = [uint16]0x54; $VK_D = [uint16]0x44; $VK_LEFT = [uint16]0x25; $VK_Q = [uint16]0x51

function Descendants([int]$root, $all) {
  $set = New-Object System.Collections.Generic.HashSet[int]
  [void]$set.Add($root)
  $queue = New-Object System.Collections.Generic.Queue[int]
  $queue.Enqueue($root)
  while ($queue.Count -gt 0) {
    $cur = $queue.Dequeue()
    foreach ($c in ($all | Where-Object { $_.ParentProcessId -eq $cur })) {
      if ($set.Add([int]$c.ProcessId)) { $queue.Enqueue([int]$c.ProcessId) }
    }
  }
  return $set
}
# Pane shells = cmd.exe processes started after our launch whose command line is the bare
# ComSpec (the launcher .bat's own cmd.exe carries ".bat" and is excluded). Reported with
# their parent pid so the ConPTY parent linkage is visible in the log.
$script:launchAt = Get-Date
function PaneShells() {
  $all = Get-CimInstance Win32_Process
  $shells = @($all | Where-Object { $_.Name -eq 'cmd.exe' -and $_.CreationDate -gt $script:launchAt -and $_.CommandLine -notmatch '\.bat' })
  $desc = ($shells | ForEach-Object { "{0}<-{1}" -f $_.ProcessId, $_.ParentProcessId }) -join ' '
  return @($shells.Count, $desc)
}
function CmdChildrenOf([int]$ccnestPid) {
  $r = PaneShells
  return ("{0} [{1}]" -f $r[0], $r[1])
}
function Alive([int]$p) { return [bool](Get-Process -Id $p -ErrorAction SilentlyContinue) }

function WriteBat([string]$path, [string]$title, [bool]$modeCon) {
  $lines = @('@echo off', "title $title")
  if ($modeCon) { $lines += 'mode con: cols=120 lines=30' }
  $lines += 'set CCNEST_INPUT_TRACE=1'
  # Force the shell fallback (cmd.exe panes, no real Claude sessions): the user environment
  # sets CCNEST_CLAUDE_BIN to the launcher shim, which resolve_claude_bin honours before PATH,
  # so point it at a file that does not exist AND hide `claude` from PATH.
  $lines += 'set CCNEST_CLAUDE_BIN=C:\ccnest-e2e-no-claude\claude.exe'
  $lines += 'set PATH=C:\Windows\System32;C:\Windows'
  $lines += ('"{0}" "{1}"' -f $Exe, $Cwd)
  Set-Content -Path $path -Value $lines -Encoding OEM
}

# ---- gated injection helpers -------------------------------------------------------------
$script:hwnd = [IntPtr]::Zero
$script:titleToken = $null
function FgOk() {
  $fg = [Mmb]::GetForegroundWindow()
  if ($fg -ne $script:hwnd) { return $false }
  if ($script:titleToken) { return ([Mmb]::Title($fg) -like "*$($script:titleToken)*") }
  return $true
}
function EnsureFocus() {
  for ($try = 0; $try -lt 6; $try++) {
    if (FgOk) { return }
    [void][Mmb]::Focus($script:hwnd)
    Start-Sleep -Milliseconds 400
  }
  $fg = [int][Mmb]::ForegroundPid()
  $name = (Get-Process -Id $fg -ErrorAction SilentlyContinue).Name
  throw "could not focus our window; foreground belongs to pid $fg ($name) title '$([Mmb]::Title([Mmb]::GetForegroundWindow()))' - refusing to inject"
}
function Key([uint16]$mod, [uint16]$vk, [int]$settleMs) {
  EnsureFocus
  if (-not (FgOk)) { throw 'focus lost right before key injection - abort' }
  [void][Mmb]::Chord($mod, $vk)
  Start-Sleep -Milliseconds $settleMs
}
function Click([int]$x, [int]$y, [int]$settleMs) {
  EnsureFocus
  if (-not (FgOk)) { throw 'focus lost right before click injection - abort' }
  [void][Mmb]::MiddleClickAt($x, $y)
  Start-Sleep -Milliseconds $settleMs
}
# parse the LAST "Mouse(Down(Middle)@c,r)" in the trace -> @(c, r)
function LastMiddleDown() {
  $m = @(TraceLines | Select-String -Pattern 'Down\(Middle\)@(\d+),(\d+)' -AllMatches | ForEach-Object { $_.Matches } )
  if ($m.Count -eq 0) { return $null }
  $last = $m[$m.Count - 1]
  return @([int]$last.Groups[1].Value, [int]$last.Groups[2].Value)
}

$results = [ordered]@{}

# ============================== Scenario A: classic console ================================
if (-not $SkipConhost) {
  Say ("=== A. conhost scenario start {0}" -f (Now))
  if (Test-Path $ilog) { Remove-Item $ilog -Force }
  $bat = Join-Path $env:TEMP 'ccnest-mmb-conhost.bat'
  WriteBat $bat 'ccnest-mmb-conhost' $true
  $script:launchAt = Get-Date
  $hostProc = Start-Process -FilePath "$env:WINDIR\System32\conhost.exe" -ArgumentList $bat -PassThru
  Start-Sleep -Seconds $StartupWaitSec
  $all = Get-CimInstance Win32_Process
  $tree = Descendants $hostProc.Id $all
  $proc = $all | Where-Object { $_.ExecutablePath -eq $Exe -and $tree.Contains([int]$_.ProcessId) } | Select-Object -First 1
  if (-not $proc) { Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue; throw "our ccnest is not running under conhost after $StartupWaitSec s" }
  $ccPid = [int]$proc.ProcessId
  $hw = [IntPtr]::Zero
  for ($i = 0; $i -lt 20 -and $hw -eq [IntPtr]::Zero; $i++) {
    foreach ($pid2 in $tree) {
      $p = Get-Process -Id $pid2 -ErrorAction SilentlyContinue
      if ($p -and $p.MainWindowHandle -ne [IntPtr]::Zero) { $hw = $p.MainWindowHandle; break }
    }
    if ($hw -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 250 }
  }
  if ($hw -eq [IntPtr]::Zero) { Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue; throw 'no console window in our tree' }
  if (-not $tree.Contains([int][Mmb]::PidOf($hw))) { throw 'console window owner outside our tree - refusing' }
  $script:hwnd = $hw; $script:titleToken = $null
  try {
    EnsureFocus
    $cr = [Mmb]::ClientOnScreen($hw)
    $cellW = ($cr[2] - $cr[0]) / 120.0; $cellH = ($cr[3] - $cr[1]) / 30.0
    Say ("A: ccnest pid={0} console client={1},{2}-{3},{4} cell={5:N1}x{6:N1}px cmd children={7}" -f $ccPid, $cr[0], $cr[1], $cr[2], $cr[3], $cellW, $cellH, (CmdChildrenOf $ccPid))
    # fail fast: the first injected key must show up in the input trace
    $before = CountMatches 'Press'
    Key $VK_CONTROL $VK_T 1500
    $ok = $false
    for ($w = 0; $w -lt 12; $w++) { if ((CountMatches 'Press') -gt $before) { $ok = $true; break }; Start-Sleep -Milliseconds 250 }
    if (-not $ok) { throw 'first injected key (Ctrl+T) never reached ccnest (trace unchanged) - abort' }
    Key $VK_CONTROL $VK_T 1500          # 3 tabs, active = idx 2
    Key $VK_CONTROL $VK_D 1500          # split the active (last) tab -> 2 panes
    $n0 = CmdChildrenOf $ccPid
    Say ("A: after Ctrl+T x2 + Ctrl+D: pane shells={0} (expect 4)" -f $n0)
    $rowY = [int]($cr[1] + $cellH * 0.5)
    $colX = [int]($cr[0] + $cellW * 2.5)    # column 2: inside the leftmost tab label whatever its width
    $seq = @()
    foreach ($k in 1..3) {
      $closesBefore = CountMatches 'close_tab'
      Click $colX $rowY 900
      $md = LastMiddleDown
      $closeLine = @(TraceLines | Where-Object { $_ -match 'close_tab' })
      $newest = if ($closeLine.Count -gt $closesBefore) { ($closeLine[$closeLine.Count - 1] -replace '^\S+\s+', '') } else { '(no close_tab line)' }
      $alive = Alive $ccPid
      $n = CmdChildrenOf $ccPid
      $seq += ("click{0}: Down(Middle)@{1} -> {2}; pane shells={3}; ccnest alive={4}" -f $k, ($(if ($md) { "$($md[0]),$($md[1])" } else { 'none' })), $newest, $n, $alive)
      Say ("A: " + $seq[-1])
    }
    Start-Sleep -Milliseconds 1500
    $exited = -not (Alive $ccPid)
    $hostExited = -not (Alive $hostProc.Id)
    $results['A_exited_after_last_close'] = $exited
    $results['A_host_window_closed'] = $hostExited
    $results['A_close_lines'] = @(TraceLines | Where-Object { $_ -match 'close_tab' } | ForEach-Object { $_ -replace '^\S+\s+', '' })
    $results['A_middle_downs'] = CountMatches 'Down\(Middle\)'
    $results['A_pending_arrow_flush'] = CountMatches 'pending_arrow_flush'
    Say ("A: ccnest exited={0} host window closed={1} pending_arrow_flush={2}" -f $exited, $hostExited, $results['A_pending_arrow_flush'])
    Copy-Item $ilog (Join-Path $outDir 'input-trace-conhost.log') -Force -ErrorAction SilentlyContinue
  } finally {
    if (Alive $ccPid) { Say 'A: cleanup: ccnest still alive -> killing'; Stop-Process -Id $ccPid -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 500
    $now = Get-CimInstance Win32_Process
    foreach ($pid2 in $tree) {
      if ($pid2 -eq $PID) { continue }
      $p = $now | Where-Object { $_.ProcessId -eq $pid2 }
      if ($p -and $p.Name -match '^(ccnest|cmd|conhost)') { Stop-Process -Id $pid2 -Force -ErrorAction SilentlyContinue }
    }
    Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue
    Remove-Item $bat -Force -ErrorAction SilentlyContinue
  }
}

# ============================== Scenario B: Windows Terminal ===============================
if (-not $SkipWt) {
  Say ("=== B. Windows Terminal scenario start {0}" -f (Now))
  if (Test-Path $ilog) { Remove-Item $ilog -Force }
  $token = 'ccnest-mmb-' + (Get-Date -Format 'HHmmss')
  $bat2 = Join-Path $env:TEMP ($token + '.bat')
  WriteBat $bat2 $token $false
  $wtBefore = @(Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'WindowsTerminal.exe' } | ForEach-Object { [int]$_.ProcessId })
  $launchAt = Get-Date
  $script:launchAt = $launchAt
  # -w <unique name>: no window with that name exists -> a NEW window, never the user's.
  $wtArgs = @('-w', $token, 'new-tab', '--title', $token, '--suppressApplicationTitle', '-d', $Cwd, 'cmd', '/c', $bat2)
  Start-Process -FilePath 'wt.exe' -ArgumentList $wtArgs | Out-Null
  Start-Sleep -Seconds ($StartupWaitSec + 2)
  $hw = [IntPtr]::Zero
  for ($i = 0; $i -lt 20 -and $hw -eq [IntPtr]::Zero; $i++) {
    $hw = [Mmb]::FindWindow('CASCADIA_HOSTING_WINDOW_CLASS', $token)
    if ($hw -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 500 }
  }
  if ($hw -eq [IntPtr]::Zero) { throw "no WT window titled '$token' found - not injecting anything" }
  $wtPid = [int][Mmb]::PidOf($hw)
  $wtIsNew = -not ($wtBefore -contains $wtPid)
  Say ("B: WT window hwnd={0} pid={1} newProcess={2}" -f $hw, $wtPid, $wtIsNew)
  $all = Get-CimInstance Win32_Process
  $cands = @($all | Where-Object { $_.ExecutablePath -eq $Exe -and $_.CreationDate -gt $launchAt })
  if ($cands.Count -ne 1) { throw ("expected exactly 1 new ccnest process, found {0} - abort" -f $cands.Count) }
  $ccPid = [int]$cands[0].ProcessId
  $script:hwnd = $hw; $script:titleToken = $token
  try {
    EnsureFocus
    $cr = [Mmb]::ClientOnScreen($hw)
    Say ("B: ccnest pid={0} WT client={1},{2}-{3},{4} cmd children={5}" -f $ccPid, $cr[0], $cr[1], $cr[2], $cr[3], (CmdChildrenOf $ccPid))
    # probe 1 / probe 2 (middle-clicks inside the pane area are no-ops for a cmd pane)
    # scalars on purpose: in PowerShell the comma binds tighter than +, so
    # @($a + 240, $b + 120) would parse as $a + @(240, $b) + 120
    $p1x = [int](($cr[0] + $cr[2]) / 2); $p1y = [int](($cr[1] + $cr[3]) / 2)
    $p2x = [int]($p1x + 240);            $p2y = [int]($p1y + 120)
    Click $p1x $p1y 700
    $m1 = LastMiddleDown
    if (-not $m1) { throw 'probe 1: no Down(Middle) reached ccnest through WT - middle button is NOT delivered (or focus/geometry wrong)' }
    Click $p2x $p2y 700
    $m2 = LastMiddleDown
    if (-not $m2 -or ($m2[0] -eq $m1[0]) -or ($m2[1] -eq $m1[1])) { throw "probe 2 did not yield a distinct cell (m1=$m1 m2=$m2)" }
    $cellW = ($p2x - $p1x) / [double]($m2[0] - $m1[0]); $cellH = ($p2y - $p1y) / [double]($m2[1] - $m1[1])
    $orgX = $p1x - ($m1[0] + 0.5) * $cellW; $orgY = $p1y - ($m1[1] + 0.5) * $cellH
    Say ("B: probes @{0},{1} and @{2},{3} -> cell={4:N2}x{5:N2}px origin={6:N0},{7:N0}" -f $m1[0], $m1[1], $m2[0], $m2[1], $cellW, $cellH, $orgX, $orgY)
    $tabX = [int]($orgX + 2.5 * $cellW); $tabY = [int]($orgY + 0.5 * $cellH)
    # The two-probe estimate is quantised to whole cells (a 4-vs-5 row difference is a 25%
    # error), so converge with feedback: the trace tells us which cell we actually hit, and a
    # middle-click on a pane row is a no-op for a cmd pane. Target = col 2, row 0.
    $closesBefore = CountMatches 'close_tab'
    $newest = '(no close_tab line)'
    $md = $null
    for ($attempt = 1; $attempt -le 5; $attempt++) {
      Click $tabX $tabY 1200
      $md = LastMiddleDown
      $closeLine = @(TraceLines | Where-Object { $_ -match 'close_tab' })
      if ($closeLine.Count -gt $closesBefore) { $newest = ($closeLine[$closeLine.Count - 1] -replace '^\S+\s+', ''); break }
      if (-not $md) { Say "B: attempt ${attempt}: click at $tabX,$tabY produced no Down(Middle) (outside the terminal?) - nudging down"; $tabY += [int]$cellH; continue }
      Say ("B: attempt {0}: aimed col2,row0 but hit @{1},{2} - correcting" -f $attempt, $md[0], $md[1])
      $tabY = [int]($tabY - $md[1] * $cellH)
      $tabX = [int]($tabX - ($md[0] - 2) * $cellW)
    }
    Start-Sleep -Milliseconds 1500
    $exited = -not (Alive $ccPid)
    $winGone = -not [Mmb]::IsWindow($hw)
    $results['B_tab_click'] = ("Down(Middle)@{0} -> {1}" -f ($(if ($md) { "$($md[0]),$($md[1])" } else { 'none' })), $newest)
    $results['B_exited_after_close'] = $exited
    $results['B_wt_window_closed'] = $winGone
    $results['B_middle_downs'] = CountMatches 'Down\(Middle\)'
    $results['B_pending_arrow_flush'] = CountMatches 'pending_arrow_flush'
    Say ("B: tab click: {0}; ccnest exited={1}; WT window gone={2}; middle downs={3}; pending_arrow_flush={4}" -f $results['B_tab_click'], $exited, $winGone, $results['B_middle_downs'], $results['B_pending_arrow_flush'])
    Copy-Item $ilog (Join-Path $outDir 'input-trace-wt.log') -Force -ErrorAction SilentlyContinue
  } finally {
    if (Alive $ccPid) { Say 'B: cleanup: ccnest still alive -> killing'; Stop-Process -Id $ccPid -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 800
    if ($wtIsNew -and (Alive $wtPid) -and [Mmb]::IsWindow($hw)) { Say 'B: cleanup: our new WT window still open -> closing its process'; Stop-Process -Id $wtPid -Force -ErrorAction SilentlyContinue }
    Remove-Item $bat2 -Force -ErrorAction SilentlyContinue
  }
}

$crashAfter = CrashLogs
$newCrash = @($crashAfter | Where-Object { $crashBefore -notcontains $_ })
$results['new_crash_logs'] = $newCrash
Say ("=== new crash logs: {0}" -f ($(if ($newCrash.Count) { $newCrash -join ',' } else { 'none' })))
$results | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $outDir 'mmb-e2e-results.json') -Encoding UTF8
$report | Set-Content -Path (Join-Path $outDir 'mmb-e2e-report.txt') -Encoding UTF8
Say '=== done'
