# 別プロセスのコンソール画面の先頭 N 行を読む (前面化しないのでユーザーの操作と衝突しない)。
param([int]$ProcId, [string]$Out, [int]$Rows = 2)
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class C {
  [DllImport("kernel32.dll")] public static extern bool FreeConsole();
  [DllImport("kernel32.dll")] public static extern bool AttachConsole(uint pid);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr CreateFileW(string n, uint a, uint s, IntPtr sa, uint c, uint f, IntPtr t);
  [StructLayout(LayoutKind.Sequential)] public struct COORD { public short X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct SMALL_RECT { public short L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct INFO { public COORD Size; public COORD Cur; public short Attr; public SMALL_RECT Win; public COORD Max; }
  [DllImport("kernel32.dll")] public static extern bool GetConsoleScreenBufferInfo(IntPtr h, out INFO i);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern bool ReadConsoleOutputCharacterW(IntPtr h, StringBuilder b, uint len, COORD at, out uint read);
  public static string Read(uint pid, int rows) {
    FreeConsole();
    if (!AttachConsole(pid)) return "attach failed";
    IntPtr h = CreateFileW("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    INFO i; GetConsoleScreenBufferInfo(h, out i);
    var sb = new StringBuilder();
    for (int r = 0; r < rows; r++) {
      var b = new StringBuilder(i.Size.X + 1); uint n;
      ReadConsoleOutputCharacterW(h, b, (uint)i.Size.X, new COORD { X = 0, Y = (short)(i.Win.T + r) }, out n);
      sb.AppendLine(b.ToString().TrimEnd());
    }
    FreeConsole();
    return sb.ToString();
  }
}
"@
[IO.File]::WriteAllText($Out, [C]::Read([uint32]$ProcId, $Rows), (New-Object Text.UTF8Encoding $false))
