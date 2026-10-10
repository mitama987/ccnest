param(
  [string]$Out = "$PSScriptRoot\ccnest-statusbar-v0.1.22.png",
  [int]$WaitSec = 10
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int cx, bool repaint);
}
"@
[void][W]::SetProcessDPIAware()
$exe = "C:\Users\mitam\scoop\persist\rustup\.cargo\bin\ccnest.exe"
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory "C:\Users\mitam\Desktop\work\90_other\ccnest" -PassThru
Start-Sleep -Seconds $WaitSec
$c = Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1
if (-not $c) { throw "no ccnest child (pid $($p.Id))" }
$h = (Get-Process -Id $c.ProcessId).MainWindowHandle
if ($h -eq [IntPtr]::Zero) { throw "no window handle (pid $($c.ProcessId))" }
$r = New-Object W+RECT
[void][W]::GetWindowRect($h, [ref]$r)
# 位置だけ (0,0) に寄せる (サイズは変えない)。画面外の部分は撮れないので画面内にクリップ。
[void][W]::MoveWindow($h, 0, 0, $r.R - $r.L, $r.B - $r.T, $true)
[void][W]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 2500
[void][W]::GetWindowRect($h, [ref]$r)
$scr = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$w = [Math]::Min($r.R, $scr.Width) - $r.L; $hh = [Math]::Min($r.B, $scr.Height) - $r.T
"window: $($r.L),$($r.T) - $($r.R),$($r.B)  screen: $($scr.Width)x$($scr.Height)"
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
# 自分が起動した conhost とその子孫 (ccnest / claude / cmd) だけ止める
function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
Kill-Tree $p.Id
Get-Item $Out | Format-List FullName, Length
