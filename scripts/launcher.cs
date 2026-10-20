using System;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Text;
using System.Windows.Forms;

// 能力核心 · 原生启动器
// 为什么需要它（两个必须在任何 JS 执行之前生效的修正）：
//   1) 清除 ELECTRON_RUN_AS_NODE：本机全局存在该变量（实测「空串」与「0」同样触发），
//      Electron 在原生层即判定为 node 模式，主进程 main.js 根本没有机会执行，
//      表现为双击秒退、零窗口零日志。JS 侧无法补救，只能在启动器里处理。
//   2) 设置 CC_PACKAGED=1：让应用按打包态解析数据目录（release/capability-core-data），
//      避免双击时数据被写进应用包内部 resources/app/data。
// 行为：设置好环境 → 以分离进程拉起 Electron 本体 → 自身立即退出。
class Program {
    const char QUOTE = (char)34;
    const string QUOTE_STR = "\\\u0022";
    const string QUOTE_ESC = "\\\u005C\\\u0022";

    [STAThread]
    static int Main(string[] args) {
        string selfDir;
        try { selfDir = Path.GetDirectoryName(Path.GetFullPath(Assembly.GetExecutingAssembly().Location)); }
        catch (Exception ex) { ShowError("无法定位自身路径：" + ex.Message); return 2; }

        string gui = Path.Combine(selfDir, "CapabilityCore-GUI.exe");
        if (!File.Exists(gui)) { ShowError("找不到主程序：" + gui); return 3; }

        try {
            Environment.SetEnvironmentVariable("ELECTRON_RUN_AS_NODE", null);
            Environment.SetEnvironmentVariable("CC_PACKAGED", "1");
        } catch (Exception ex) { ShowError("无法设置启动环境：" + ex.Message); return 4; }

        string workDir = Path.GetFullPath(Path.Combine(selfDir, ".."));

        try {
            var psi = new ProcessStartInfo(gui, QuoteArgs(args));
            psi.UseShellExecute = false;
            psi.WorkingDirectory = workDir;
            psi.CreateNoWindow = true;
            Process.Start(psi);
        } catch (Exception ex) { ShowError("启动失败：" + ex.Message); return 5; }
        return 0;
    }

    // .NET 4.0 没有 ArgumentList，手工加引号转义
    static string QuoteArgs(string[] args) {
        if (args == null || args.Length == 0) return string.Empty;
        var sb = new StringBuilder();
        foreach (var a in args) {
            if (sb.Length > 0) sb.Append(' ');
            sb.Append(QUOTE);
            if (!String.IsNullOrEmpty(a)) sb.Append(a.Replace(QUOTE_STR, QUOTE_ESC));
            sb.Append(QUOTE);
        }
        return sb.ToString();
    }

    static void ShowError(string message) {
        try { MessageBox.Show(message, "能力核心 · 启动失败", MessageBoxButtons.OK, MessageBoxIcon.Error); }
        catch { try { Console.Error.WriteLine(message); } catch { } }
    }
}
