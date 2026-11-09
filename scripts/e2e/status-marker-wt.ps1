# タブの状態マーカーを Windows Terminal の実画面で確かめる (v0.1.34)。
# conhost 経由の status-marker.ps1 は絵文字が U+FFFD に潰れて 🟢/🟨 を読めないので、
# 使い捨て ccnest を WT の新しい窓で起こし、本物の claude に短い指示を送って
#   1. 入力待ち (🟨 のはず)  2. 実行中 (🟢 のはず)  3. 終了後 (🟨 のはず)
# の 3 枚を PrintWindow で撮る (他の窓に隠れていても描ける＝ユーザー操作中でも前面化しない)。
# 新しい一時フォルダだと claude の「このフォルダを信頼しますか」が出るので、起動フォルダは
# 信頼済みのリポ配下 target/ (gitignore) に作る。絵文字の同定は目視 (スクショを見る)。
# ゆうさんの ccnest には触らない。Claude に 1 回だけ短い指示を送る (プラン利用量をごく僅かに使う)。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-status-marker-wt')
)
$ErrorActionPreference = 'Stop'
$e2e = Join-Path $PSScriptRoot 'status-marker.ps1'
New-Item -ItemType Directory -Force $OutDir | Out-Null
Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$env:CCNEST_JP_TITLE = 'off'

Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
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

$tag = 'ccnest-green-' + (Get-Date -Format 'HHmmss')
# claude trust dialog appears for a brand-new temp folder, so use a subfolder of the trusted repo (target/ is gitignored)
$work = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..\target')).Path $tag
New-Item -ItemType Directory -Force $work | Out-Null
$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$screenFile = Join-Path $work 'screen.txt'

function Shot($h, $name) {
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($h, [ref]$r)
  $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [Drawing.Graphics]::FromImage($bmp)
  $hdc = $g.GetHdc()
  [void][W]::PrintWindow($h, $hdc, 2)   # PW_RENDERFULLCONTENT: 他の窓に隠れていても描ける
  $g.ReleaseHdc($hdc)
  $p = Join-Path $OutDir $name
  $bmp.Save($p, [Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  "shot: $p ($((Get-Item $p).Length) bytes)"
}
function Inner($what, $ccId, $text) {
  $args = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$e2e`"", '-Inner', $what, '-ProcId', $ccId, '-Out', "`"$screenFile`"")
  if ($text) { $args += @('-Text', "`"$text`"") }
  Start-Process powershell.exe -ArgumentList $args -WindowStyle Hidden -Wait
  Get-Content $screenFile -Raw -Encoding UTF8
}
function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }

$cc = $null; $hwnd = [IntPtr]::Zero
try {
  Start-Process wt.exe -ArgumentList @('-w', 'new', '--size', '100,22', '-d', "`"$work`"", "`"$Exe`"")
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $cc = Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1
  } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw 'no new ccnest' }
  "ccnest pid: $($cc.Id)"
  $deadline = (Get-Date).AddSeconds(15)
  do { Start-Sleep -Milliseconds 500; $hwnd = [W]::Find($tag) } while ($hwnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
  if ($hwnd -eq [IntPtr]::Zero) { throw "window '$tag' not found" }
  [void][W]::MoveWindow($hwnd, 0, 0, 1400, 560, $true)

  # claude のプロンプトが出るまで待つ (conhost 側のバッファで判定)
  $deadline = (Get-Date).AddSeconds(40)
  do { Start-Sleep -Seconds 1; $s = Inner 'screen' $cc.Id } while ($s -notmatch '[>\u276F]\s' -and (Get-Date) -lt $deadline)
  Start-Sleep -Seconds 3
  Shot $hwnd 'status-marker-1-idle.png'

  [void](Inner 'type' $cc.Id 'Print the numbers 1 to 400, one per line, no other text.')
  $SPIN = [char]0x2026 + ' *\((\d+h )?(\d+m )?\d+s( |\))'
  $deadline = (Get-Date).AddSeconds(40)
  do { Start-Sleep -Milliseconds 500; $s = Inner 'screen' $cc.Id } while ($s -notmatch $SPIN -and (Get-Date) -lt $deadline)
  $spinLine = ($s -split "`n") | Where-Object { $_ -match $SPIN } | Select-Object -First 1
  if ($spinLine) { "spinner (conhost view): " + $spinLine.Trim() } else { "NO SPINNER within 40s; conhost screen:"; $s }
  Start-Sleep -Seconds 3   # 2 秒 tick をまたぐ
  Shot $hwnd 'status-marker-2-running.png'
  "tab row (conhost view): " + (($s -split "`n")[0]).TrimEnd()

  $deadline = (Get-Date).AddSeconds(120)
  do { Start-Sleep -Seconds 1; $s = Inner 'screen' $cc.Id } while ($s -match $SPIN -and (Get-Date) -lt $deadline)
  Start-Sleep -Seconds 3
  Shot $hwnd 'status-marker-3-done.png'
} finally {
  if ($cc) { Kill-Tree $cc.Id }
  if ($hwnd -ne [IntPtr]::Zero) { [void][W]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
  Start-Sleep -Seconds 1
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
