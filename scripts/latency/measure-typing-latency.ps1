#Requires -Version 5.1
<#
  ccnest keystroke-latency harness v4 (safe focus).

  Measures typing echo while the Claude child is idle -> busy -> idle again, in ONE ccnest
  process, and reports per-keystroke child_render / present / total (see analyze2.py).

  SAFETY (why v3 was rewritten): v3 launched ccnest through `wt -w new`. When Windows
  Terminal reuses an existing process, the new tab lands inside the USER'S window, and
  AppActivate(title) can activate that window while a different tab holds the keyboard.
  In the 13:08 run every injected key missed ccnest entirely (input trace: 0 Press lines).
  v4 therefore:
    1. launches ccnest in its OWN classic console window (conhost.exe <bat>), because on
       Windows 11 the default terminal is Windows Terminal and a plain .bat lands as a TAB
       inside the user's existing WT window - where injected keys hit whatever tab is active,
    2. verifies the foreground window belongs to that process tree before typing,
    3. sends ONE key and aborts unless ccnest's input trace records it (fail fast, so a
       prompt is never typed into an unknown window),
    4. re-verifies the foreground window before every phase.

  Usage:
    measure3.ps1 -Exe <ccnest.exe> -Label r2-before-stream -Mode stream [-AltScreen] [-ClaudeDebug]
    -Mode idle | stream | tool
#>
param(
  [Parameter(Mandatory=$true)][string]$Exe,
  [Parameter(Mandatory=$true)][string]$Label,
  [ValidateSet('idle','stream','tool','flood')][string]$Mode = 'stream',
  [int]$Keys = 19,
  [int]$GapMs = 120,
  [int]$HoldMs = 60,
  [int]$StartupWaitSec = 18,
  [int]$BusyMaxSec = 150,
  [switch]$AltScreen,
  [switch]$ClaudeDebug,
  [switch]$Mouse,
  [string]$Cwd = 'C:\Users\mitam\Desktop\work\90_other\ccnest'
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName Microsoft.VisualBasic
$code = @'
using System;
using System.Runtime.InteropServices;
public static class KeyInject {
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public KEYBDINPUT ki; public long pad; }
  [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
  [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr hWnd);
  [DllImport("user32.dll")] static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
  [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
  public static uint PidOf(IntPtr hWnd) { uint pid; GetWindowThreadProcessId(hWnd, out pid); return pid; }
  // Foreground-lock dance: a synthetic ALT tap releases the lock, then attach our input
  // queue to the current foreground thread so SetForegroundWindow is honoured.
  public static bool Focus(IntPtr hWnd) {
    INPUT[] alt = new INPUT[2];
    alt[0].type = INPUT_KEYBOARD; alt[0].ki.wVk = 0x12; alt[0].ki.dwFlags = 0;
    alt[1].type = INPUT_KEYBOARD; alt[1].ki.wVk = 0x12; alt[1].ki.dwFlags = KEYEVENTF_KEYUP;
    SendInput(2, alt, Marshal.SizeOf(typeof(INPUT)));
    ShowWindow(hWnd, 9);          // SW_RESTORE
    BringWindowToTop(hWnd);
    if (SetForegroundWindow(hWnd)) return true;
    uint fgThread = GetWindowThreadProcessId(GetForegroundWindow(), out _unused);
    uint myThread = GetCurrentThreadId();
    AttachThreadInput(myThread, fgThread, true);
    bool ok = SetForegroundWindow(hWnd);
    AttachThreadInput(myThread, fgThread, false);
    return ok;
  }
  static uint _unused;
  const uint INPUT_KEYBOARD = 1, KEYEVENTF_KEYUP = 2, KEYEVENTF_UNICODE = 4;
  public static int Size() { return Marshal.SizeOf(typeof(INPUT)); }
  public static uint ForegroundPid() { uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid; }
  static uint SendUnicode(char c, uint flags) {
    INPUT[] a = new INPUT[1];
    a[0].type = INPUT_KEYBOARD; a[0].ki.wScan = (ushort)c; a[0].ki.dwFlags = KEYEVENTF_UNICODE | flags;
    return SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
  public static uint SendDown(char c) { return SendUnicode(c, 0); }
  public static uint SendUp(char c) { return SendUnicode(c, KEYEVENTF_KEYUP); }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
  // Move the pointer inside the window: with mouse capture on, ccnest gets a Moved event
  // per pixel step, which is what a trackpad produces while the user is typing.
  public static void Jiggle(IntPtr hWnd, int step) {
    RECT r;
    if (!GetWindowRect(hWnd, out r)) return;
    int cx = (r.L + r.R) / 2, cy = (r.T + r.B) / 2;
    for (int i = 0; i < 6; i++) { SetCursorPos(cx + i * step, cy + i * step); System.Threading.Thread.Sleep(4); }
    for (int i = 6; i > 0; i--) { SetCursorPos(cx + i * step, cy + i * step); System.Threading.Thread.Sleep(4); }
  }
  public static uint SendVk(ushort vk) {
    INPUT[] a = new INPUT[2];
    a[0].type = INPUT_KEYBOARD; a[0].ki.wVk = vk; a[0].ki.dwFlags = 0;
    a[1].type = INPUT_KEYBOARD; a[1].ki.wVk = vk; a[1].ki.dwFlags = KEYEVENTF_KEYUP;
    return SendInput(2, a, Marshal.SizeOf(typeof(INPUT)));
  }
}
'@
Add-Type -TypeDefinition $code
if ([KeyInject]::Size() -ne 40) { throw ("INPUT struct size is {0}, expected 40" -f [KeyInject]::Size()) }

# 19 distinct NON-ASCII symbols, built from code points so this file stays pure ASCII
# (PowerShell 5.1 would otherwise misread it without a BOM). Non-ASCII is the point: in the
# PTY dump every byte >= 0x80 is escaped as <xx>, so the echo of one of these characters can
# never be confused with a VT escape sequence - unlike ASCII symbols such as ~ [ ? > % $,
# which all occur inside CSI/OSC sequences and made v3's matcher useless.
$symbols = -join ([char[]]@(0x2660,0x2665,0x2666,0x2663,0x2605,0x2606,0x266A,0x266B,0x2713,
                            0x2717,0x03A9,0x03BB,0x03BC,0x03C3,0x03A6,0x03A8,0x039E,0x0398,0x03A0))
if ($Keys -gt $symbols.Length) { $Keys = $symbols.Length }

$outDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$traceDir = Join-Path $env:APPDATA 'ccnest'
$log  = Join-Path $traceDir 'latency-trace.log'
$ilog = Join-Path $traceDir 'input-trace.log'
$dlog = Join-Path $traceDir 'pty-dump.log'
foreach ($f in @($log, $ilog, $dlog)) { if (Test-Path $f) { Remove-Item $f -Force } }
$dbgDir = Join-Path $outDir "claude-debug-$Label"
if ($ClaudeDebug) { if (Test-Path $dbgDir) { Remove-Item $dbgDir -Recurse -Force }; New-Item -ItemType Directory -Force $dbgDir | Out-Null }

# ---- launch ccnest in its OWN console window (never Windows Terminal) ----
$bat = Join-Path $env:TEMP ("ccnest-lat-{0}.bat" -f $Label)
$lines = @('@echo off', "title ccnest-lat-$Label", 'mode con: cols=120 lines=30',
           'set CCNEST_LATENCY_TRACE=1', 'set CCNEST_INPUT_TRACE=1', 'set CCNEST_PTY_DUMP=1')
if ($AltScreen)         { $lines += 'set CCNEST_CLAUDE_ALT_SCREEN=1' }
# plan mode would stop at "may I run this?" and never produce output, so tool/flood
# runs start with permissions already granted
if ($Mode -eq 'tool' -or $Mode -eq 'flood') { $lines += 'set CCNEST_CLAUDE_PERMISSION_MODE=off' }
if ($ClaudeDebug)       { $lines += "set CLAUDE_CODE_DEBUG_LOGS_DIR=$dbgDir"; $lines += 'set CLAUDE_CODE_DEBUG_LOG_LEVEL=debug' }
$lines += ('"{0}" "{1}"' -f $Exe, $Cwd)
Set-Content -Path $bat -Value $lines -Encoding OEM
# conhost.exe <bat> forces the classic console host: a real, separate top-level window.
$hostProc = Start-Process -FilePath "$env:WINDIR\System32\conhost.exe" -ArgumentList $bat -PassThru
Start-Sleep -Seconds $StartupWaitSec

# ---- locate our ccnest and build the set of PIDs whose window may hold focus ----
$all = Get-CimInstance Win32_Process
function Descendants([int]$root) {
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
$tree = Descendants $hostProc.Id
$proc = $all | Where-Object { $_.ExecutablePath -eq $Exe -and $tree.Contains([int]$_.ProcessId) } | Select-Object -First 1
if (-not $proc) { Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue; throw "our ccnest ($Exe) is not running under the launcher after $StartupWaitSec s" }
$all = Get-CimInstance Win32_Process
$tree = Descendants $hostProc.Id
$children = @($all | Where-Object { $_.ParentProcessId -eq $proc.ProcessId })
Write-Host ("host pid={0} ccnest pid={1} children={2}" -f $hostProc.Id, $proc.ProcessId, (($children | ForEach-Object { $_.Name }) -join ','))

# The console window belongs to the cmd process inside our tree (conhost hands it the window).
$hwnd = [IntPtr]::Zero
$hwndPid = 0
for ($i = 0; $i -lt 20 -and $hwnd -eq [IntPtr]::Zero; $i++) {
  foreach ($pid2 in $tree) {
    $p = Get-Process -Id $pid2 -ErrorAction SilentlyContinue
    if ($p -and $p.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $p.MainWindowHandle; $hwndPid = $p.Id; break }
  }
  if ($hwnd -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 250 }
}
if ($hwnd -eq [IntPtr]::Zero) { Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue; throw "no console window found in our process tree" }
if (-not $tree.Contains([int][KeyInject]::PidOf($hwnd))) { throw "console window owner is outside our tree - refusing to continue" }
Write-Host ("console hwnd={0} owned by pid={1} ({2})" -f $hwnd, $hwndPid, (Get-Process -Id $hwndPid).Name)

function EnsureFocus() {
  for ($try = 0; $try -lt 5; $try++) {
    [void][KeyInject]::Focus($hwnd)
    Start-Sleep -Milliseconds 400
    if ([KeyInject]::GetForegroundWindow() -eq $hwnd) { return $true }
  }
  $fg = [int][KeyInject]::ForegroundPid()
  $name = (Get-Process -Id $fg -ErrorAction SilentlyContinue).Name
  throw "could not focus our ccnest console window; foreground belongs to pid $fg ($name) - refusing to inject keys"
}
[void](EnsureFocus)
Write-Host "our ccnest console window is focused - safe to type"

function Now() { return (Get-Date).ToString('HH:mm:ss.fff') }
function DumpLen() { if (Test-Path $dlog) { return (Get-Item $dlog).Length } else { return 0 } }
function PressCount() { if (Test-Path $ilog) { return @(Select-String -Path $ilog -Pattern ',Press' -AllMatches).Count } else { return 0 } }
$script:refocus = 0
function SendChar([char]$c) {
  # verify focus before EVERY key: in the 13:32 run something stole the foreground mid-phase
  # and 17 of 19 keys went to another window (ccnest recorded only 2 presses).
  if ([KeyInject]::GetForegroundWindow() -ne $hwnd) {
    $script:refocus++
    [void](EnsureFocus)
    Start-Sleep -Milliseconds 200
  }
  [void][KeyInject]::SendDown($c); Start-Sleep -Milliseconds $HoldMs
  [void][KeyInject]::SendUp($c)
  if ($Mouse) { [KeyInject]::Jiggle($hwnd, 3) }
  Start-Sleep -Milliseconds $GapMs
}
function TypeSymbols([string]$phase) {
  [void](EnsureFocus)
  $t0 = Now
  $dump0 = DumpLen
  $before = PressCount
  SendChar $symbols[0]
  # fail fast: the first key MUST show up in ccnest's input trace, or focus is wrong
  $ok = $false
  for ($w = 0; $w -lt 10; $w++) { if ((PressCount) -gt $before) { $ok = $true; break }; Start-Sleep -Milliseconds 250 }
  if (-not $ok) { throw "phase ${phase}: the first injected key never reached ccnest (input trace unchanged) - aborting before typing anything else" }
  for ($i = 1; $i -lt $Keys; $i++) { SendChar $symbols[$i] }
  $t1 = Now
  $bytes[$phase] = (DumpLen) - $dump0
  $delivered = (PressCount) - $before
  Write-Host ("phase {0}: {1} keys {2} .. {3}  (ccnest saw {4} presses, PTY bytes {5}, refocus {6})" -f `
    $phase, $Keys, $t0, $t1, $delivered, $bytes[$phase], $script:refocus)
  if ($delivered -lt $Keys) { Write-Host ("WARNING: only {0}/{1} keys reached ccnest in phase {2}" -f $delivered, $Keys, $phase) }
  return @($t0, $t1)
}
function ClearPrompt() {
  for ($i = 0; $i -lt ($Keys + 4); $i++) { [void][KeyInject]::SendVk(0x08); Start-Sleep -Milliseconds 35 }
  Start-Sleep -Milliseconds 500
}
function TypeText([string]$text) {
  foreach ($ch in $text.ToCharArray()) { [void][KeyInject]::SendDown($ch); Start-Sleep -Milliseconds 8; [void][KeyInject]::SendUp($ch); Start-Sleep -Milliseconds 14 }
}
$phases = @{}
$cpu = @{}
$bytes = @{}
function SampleCpu([string]$name, [int]$sec) {
  try {
    $c0 = (Get-Process -Id $proc.ProcessId -ErrorAction Stop).TotalProcessorTime.TotalMilliseconds
    Start-Sleep -Seconds $sec
    $c1 = (Get-Process -Id $proc.ProcessId -ErrorAction Stop).TotalProcessorTime.TotalMilliseconds
    $cpu[$name] = [math]::Round(($c1 - $c0) / ($sec * 1000.0) * 100.0, 2)
  } catch { $cpu[$name] = 'n/a' }
}

try {
  # ---- phase 1: idle before
  $phases['idle_before'] = TypeSymbols 'idle_before'
  SampleCpu 'idle_before' 3
  ClearPrompt

  if ($Mode -ne 'idle') {
    switch ($Mode) {
      'stream' { $prompt = 'Print the numbers from 1 to 400, one number per line, digits only, no other text, no code block.' }
      'tool'   { $prompt = 'Run this exact bash command and wait for it to finish: ping -n 45 127.0.0.1' }
      # a command that keeps printing for ~40 s: Claude Code renders a live-updating output
      # preview, which is the "processing" UI the owner sees (a one-shot command finishes too
      # fast, and Claude collapses bulk tool output into a summary instead of streaming it)
      'flood'  { $prompt = 'Run this exact bash command and wait for it to finish: for i in $(seq 1 400); do echo "line $i"; sleep 0.1; done' }
    }
    [void](EnsureFocus)
    TypeText $prompt
    Start-Sleep -Milliseconds 300
    Write-Host ("prompt sent at {0}: {1}" -f (Now), $prompt)
    [void][KeyInject]::SendVk(0x0D)

    $busyStart = $null
    if ($Mode -eq 'tool') {
      Start-Sleep -Seconds 8
      $busyStart = Now
    } else {
      # sustained output, not a single burst: >25 KB across three consecutive 700 ms samples
      $deadline = (Get-Date).AddSeconds(90)
      $window = New-Object System.Collections.Generic.Queue[long]
      $prev = DumpLen
      while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 700
        $cur = DumpLen
        $window.Enqueue($cur - $prev)
        while ($window.Count -gt 3) { [void]$window.Dequeue() }
        $prev = $cur
        $sum = 0; foreach ($v in $window) { $sum += $v }
        if ($window.Count -eq 3 -and $sum -gt 8000) { $busyStart = Now; break }
      }
      if (-not $busyStart) {
        # never abort here: the idle_after phase is the point of the test (does the pane stay
        # slow once Claude has been busy?), so type anyway and record that busy was weak
        Write-Host 'WARNING: no sustained output detected; typing anyway (busy phase may be light)'
        $busyStart = Now
      }
    }
    Write-Host ("busy phase detected at {0}" -f $busyStart)
    $phases['busy'] = TypeSymbols 'busy'
    SampleCpu 'busy' 3

    $endDeadline = (Get-Date).AddSeconds($BusyMaxSec)
    $prev = DumpLen
    while ((Get-Date) -lt $endDeadline) {
      Start-Sleep -Seconds 3
      $cur = DumpLen
      if (($cur - $prev) -lt 1500) { break }
      $prev = $cur
    }
    $phases['busy_end'] = @((Now), (Now))
    Write-Host ("busy phase ended (PTY quiet) at {0}" -f (Now))
    Start-Sleep -Seconds 2
    ClearPrompt

    # ---- phase 3: idle after
    $phases['idle_after'] = TypeSymbols 'idle_after'
    SampleCpu 'idle_after' 3
  }
} finally {
  Stop-Process -Id $proc.ProcessId -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 800
  $now = Get-CimInstance Win32_Process
  foreach ($pid2 in $tree) {
    if ($pid2 -eq $PID) { continue }
    $p = $now | Where-Object { $_.ProcessId -eq $pid2 }
    if ($p -and $p.Name -match '^(ccnest|claude|ccnest-claude-launcher|cmd|conhost)') {
      Stop-Process -Id $pid2 -Force -ErrorAction SilentlyContinue
    }
  }
  # anything spawned by our ccnest after the tree snapshot (extra panes) - orphans otherwise
  foreach ($p in (Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'ccnest-claude-launcher.exe' -or $_.Name -eq 'claude.exe' })) {
    if (-not (Get-CimInstance Win32_Process -Filter "ProcessId=$($p.ParentProcessId)" -ErrorAction SilentlyContinue)) {
      if ($p.CreationDate -gt $hostProc.StartTime) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
    }
  }
  Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue
  Remove-Item $bat -Force -ErrorAction SilentlyContinue

  foreach ($pair in @(@($log,'latency'), @($ilog,'input'), @($dlog,'ptydump'))) {
    if (Test-Path $pair[0]) { Copy-Item $pair[0] (Join-Path $outDir ("{0}-{1}.log" -f $pair[1], $Label)) -Force }
  }
  $meta = [ordered]@{ label=$Label; mode=$Mode; altScreen=[bool]$AltScreen; mouse=[bool]$Mouse; keys=$Keys; symbols=$symbols; phases=$phases; cpu=$cpu; bytes=$bytes }
  $meta | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $outDir "phases-$Label.json") -Encoding UTF8
  Write-Host ("=== {0} done. phases: {1}; cpu: {2}; ptyBytes: {3}" -f $Label,
    (($phases.Keys | ForEach-Object { "$_=" + ($phases[$_] -join '..') }) -join ' | '),
    (($cpu.Keys | ForEach-Object { "$_=$($cpu[$_])%" }) -join ' '),
    (($bytes.Keys | ForEach-Object { "$_=$($bytes[$_])" }) -join ' '))
  if ($ClaudeDebug -and (Test-Path $dbgDir)) {
    $slow = @(Get-ChildItem $dbgDir -Recurse -File -ErrorAction SilentlyContinue | Select-String -Pattern 'Slow render' | ForEach-Object { $_.Line })
    Write-Host ("claude debug: {0} 'Slow render' lines" -f $slow.Count)
    $slow | Select-Object -First 8 | ForEach-Object { Write-Host ("  " + $_) }
  }
}
