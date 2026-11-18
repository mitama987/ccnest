# Fake claude for click-extend-selection.ps1: prints numbered lines and waits.
# -MouseMode also turns on mouse reporting (DECSET 1000 + SGR 1006), which is what
# makes ccnest hold a plain left press back instead of starting a selection.
# ASCII only (no BOM needed).
param([switch]$MouseMode)
[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false
$out = [Console]::Out
$out.Write("`r`n")
0..13 | ForEach-Object { $out.Write(("LN{0:d2}-0123456789abcdefghijklmnopqrstuvwxyz`r`n" -f $_)) }
if ($MouseMode) {
  # ConPTY only passes a mouse-mode request on to the terminal (ccnest) when the
  # app reads VT input (ENABLE_VIRTUAL_TERMINAL_INPUT = 0x200 on stdin), as a real
  # TUI such as Claude Code does. Without it the request stops inside ConPTY.
  Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class V {
  [DllImport("kernel32.dll")] public static extern IntPtr GetStdHandle(int n);
  [DllImport("kernel32.dll")] public static extern bool GetConsoleMode(IntPtr h, out uint m);
  [DllImport("kernel32.dll")] public static extern bool SetConsoleMode(IntPtr h, uint m);
}
"@
  $h = [V]::GetStdHandle(-10); $m = [uint32]0
  [void][V]::GetConsoleMode($h, [ref]$m)
  [void][V]::SetConsoleMode($h, ($m -bor 0x200))
  $esc = [char]27
  $out.Write("$esc[?1000h$esc[?1006h")
}
$out.Flush()
Start-Sleep -Seconds 600
