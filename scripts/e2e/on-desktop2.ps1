# Runs a command in THIS console after moving the console window to virtual desktop 2, so an
# E2E window never shows up on the desktop the user works on and never takes the focus.
#
# Use it as the first process of a hidden conhost:
#   Start-Process conhost.exe -WindowStyle Hidden -PassThru -WorkingDirectory $work -ArgumentList `
#     "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$PSScriptRoot\on-desktop2.ps1`" `"<exe or .cmd>`" <args>"
#
# How it works: Windows has no public API to move ANOTHER process's window between virtual
# desktops, but IVirtualDesktopManager.MoveWindowToDesktop accepts a window of the calling
# process, and a conhost window counts as owned by the first process attached to the console.
# So this script has to be that first process (not a child of cmd.exe). The window starts
# hidden, is moved, and only then shown without activation (SW_SHOWNOACTIVATE).
#
# - Desktop 2 = the second entry of VirtualDesktopIDs (the Win+Tab order).
# - Only one desktop, or the move fails: the window stays hidden. The E2E scripts read the
#   screen and send input through the console API, which works on a hidden console too.
# - Not for Windows Terminal windows (owned by WindowsTerminal.exe; they cannot be moved).
# - CCNEST_E2E_DESKTOP=current: do not move, show the window where it is (to watch a run).
# ASCII only (no BOM needed).
$ErrorActionPreference = 'Stop'
if ($args.Count -lt 1) { Write-Error 'usage: on-desktop2.ps1 <command> [args...]'; exit 2 }

try {
  Add-Type @"
using System; using System.Runtime.InteropServices;
[ComImport, Guid("a5cd92ff-29be-454c-8d04-d82879fb3f1b"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IVirtualDesktopManager {
  [PreserveSig] int IsWindowOnCurrentVirtualDesktop(IntPtr hwnd, out int onCurrent);
  [PreserveSig] int GetWindowDesktopId(IntPtr hwnd, out Guid desktopId);
  [PreserveSig] int MoveWindowToDesktop(IntPtr hwnd, ref Guid desktopId);
}
public static class Desk {
  [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  // 0 = moved. Anything else = HRESULT of the failed call.
  public static int MoveOwnConsole(Guid target) {
    var mgr = (IVirtualDesktopManager)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("aa509086-5ca9-4c25-8f95-589d3c07b48a")));
    return mgr.MoveWindowToDesktop(GetConsoleWindow(), ref target);
  }
  public static void ShowNoActivate() { ShowWindow(GetConsoleWindow(), 4); }
}
"@
  if ($env:CCNEST_E2E_DESKTOP -eq 'current') {
    [Desk]::ShowNoActivate()
  } else {
    $ids = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops' -ErrorAction SilentlyContinue).VirtualDesktopIDs
    if ($ids -and $ids.Length -ge 32) {
      $second = New-Object Guid (, [byte[]]$ids[16..31])
      if ([Desk]::MoveOwnConsole($second) -eq 0) { [Desk]::ShowNoActivate() }
    }
  }
} catch {
  # Could not move: keep the window hidden and run the command anyway.
}
# This variable is for this script only; the command must not see it.
Remove-Item env:CCNEST_E2E_DESKTOP -ErrorAction SilentlyContinue

$cmd = $args[0]
$rest = @($args | Select-Object -Skip 1)
& $cmd @rest
exit $LASTEXITCODE
