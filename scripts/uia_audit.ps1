# Screen-reader audit: walks every child window of a top-level window through
# MSAA (the API NVDA and JAWS query for Win32 controls) and fails if any
# tab-stop control has no accessible name.
param(
    [Parameter(Mandatory)][string]$WindowTitle,
    [int]$TimeoutSec = 15
)
Add-Type -ReferencedAssemblies Accessibility -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using Accessibility;

public static class Msaa {
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr h, EnumProc f, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern int GetWindowLongW(IntPtr h, int i);
    [DllImport("user32.dll")] static extern IntPtr GetParent(IntPtr h);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("oleacc.dll")] static extern int AccessibleObjectFromWindow(IntPtr h, uint id, ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out object o);
    [DllImport("oleacc.dll", CharSet = CharSet.Unicode)] static extern uint GetRoleTextW(uint role, StringBuilder s, uint n);

    public static IntPtr Find(string title) { return FindWindowW(null, title); }

    public static List<string[]> Walk(IntPtr top) {
        var rows = new List<string[]>();
        EnumChildWindows(top, (h, l) => {
            if (!IsWindowVisible(h)) return true;
            var cls = new StringBuilder(256); GetClassNameW(h, cls, 256);
            bool tabstop = (GetWindowLongW(h, -16) & 0x00010000) != 0;
            Guid iid = new Guid("618736E0-3C3D-11CF-810C-00AA00389B71");
            object o; string name = "", role = "?";
            if (AccessibleObjectFromWindow(h, 0xFFFFFFFC, ref iid, out o) == 0) {
                var acc = (IAccessible)o;
                try { name = acc.get_accName(0) ?? ""; } catch {}
                try { var rs = new StringBuilder(64); GetRoleTextW(Convert.ToUInt32(acc.get_accRole(0)), rs, 64); role = rs.ToString(); } catch {}
            }
            int depth = 0; for (var p = GetParent(h); p != top && p != IntPtr.Zero; p = GetParent(p)) depth++;
            rows.Add(new[] { depth.ToString(), cls.ToString(), role, name, tabstop ? "1" : "0" });
            return true;
        }, IntPtr.Zero);
        return rows;
    }
}
'@

$deadline = (Get-Date).AddSeconds($TimeoutSec)
do {
    $top = [Msaa]::Find($WindowTitle)
    if ($top -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 300 }
} while ($top -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
if ($top -eq [IntPtr]::Zero) { Write-Output "FAIL: window '$WindowTitle' not found"; exit 2 }

$unnamed = 0
foreach ($r in [Msaa]::Walk($top)) {
    $flag = ''
    if ($r[4] -eq '1' -and [string]::IsNullOrWhiteSpace($r[3])) { $flag = '  <-- NO NAME'; $unnamed++ }
    Write-Output ("{0}{1} [{2}] name='{3}' tabstop={4}{5}" -f ('  ' * [int]$r[0]), $r[2], $r[1], $r[3], $r[4], $flag)
}
if ($unnamed -gt 0) { Write-Output "FAIL: $unnamed tab-stop control(s) without an accessible name"; exit 1 }
Write-Output "PASS: every tab-stop control has an accessible name"
