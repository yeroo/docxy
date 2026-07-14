using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Threading;

namespace XlsxyCompare;

public partial class MainWindow : Window
{
    private const int CollapsedW = 56;
    private const int ExpandedW = 460;

    private string _repoRoot = "";
    private string _xlsxyExe = "";
    private Manifest _manifest = new();

    private IntPtr _hwnd;
    private bool _docOpen;
    private bool _collapsed;
    private FileEntry? _selected;

    private Process? _xlsxy;
    private IntPtr _xlsxyHwnd;
    private IntPtr _excelHwnd;
    private dynamic? _excelApp;
    private dynamic? _excelBook;
    private DispatcherTimer? _placer;

    public MainWindow()
    {
        InitializeComponent();
        SearchBox.Text = "";
        TryLoadManifest();
        Loaded += (_, _) => BuildTree();
        SourceInitialized += OnSourceInitialized;
        MouseEnter += (_, _) => { if (_docOpen && _collapsed) Expand(); };
        MouseLeave += (_, _) => { if (_docOpen && !_collapsed) Collapse(); };
        Closing += (_, _) => CloseChildren();
    }

    private void OnSourceInitialized(object? sender, EventArgs e)
    {
        _hwnd = new WindowInteropHelper(this).Handle;
        var wa = Win32.WorkArea();
        Win32.Place(_hwnd, wa.Left, wa.Top, ExpandedW, wa.Height); // start expanded, full height
    }

    // ---- manifest + discovery ----

    private void TryLoadManifest()
    {
        _repoRoot = LocateRepo();
        if (_repoRoot.Length == 0)
        {
            MessageBox.Show("Could not find corpus/classification-xlsx.json above this app.\n" +
                            "Run corpus/tools/classify_xlsx.py first.", "xlsxy compare");
            return;
        }
        var path = Path.Combine(_repoRoot, "corpus", "classification-xlsx.json");
        try
        {
            var json = File.ReadAllText(path);
            _manifest = JsonSerializer.Deserialize<Manifest>(json,
                new JsonSerializerOptions { PropertyNameCaseInsensitive = true }) ?? new Manifest();
        }
        catch (Exception ex)
        {
            MessageBox.Show("Failed to read manifest:\n" + ex.Message, "xlsxy compare");
        }
        // Always rebuild xlsxy here so the launcher uses the current binary,
        // independent of whether `dotnet run` re-ran the MSBuild step.
        BuildXlsxy(_repoRoot);
        _xlsxyExe = FindXlsxyExe(_repoRoot);
        SubTitle.Text = _xlsxyExe.Length > 0
            ? $"{_manifest.Count} workbooks · xlsxy + Excel side by side"
            : "xlsxy.exe not found — build it (cargo build)";
    }

    /// Run `cargo build --release -p xlsxy` synchronously so the launched xlsxy
    /// is always current. A no-op (~0.1s) when nothing changed.
    private static void BuildXlsxy(string repo)
    {
        var cargoHome = Environment.GetEnvironmentVariable("CARGO_HOME")
            ?? Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".cargo");
        var cargo = Path.Combine(cargoHome, "bin", "cargo.exe");
        if (!File.Exists(cargo)) return; // no toolchain — use whatever exists
        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = cargo,
                WorkingDirectory = repo,
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            foreach (var a in new[] { "build", "--release", "-p", "xlsxy" })
                psi.ArgumentList.Add(a);
            Process.Start(psi)?.WaitForExit(180_000);
        }
        catch { /* best effort */ }
    }

    private static string LocateRepo()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            if (File.Exists(Path.Combine(dir.FullName, "corpus", "classification-xlsx.json")))
                return dir.FullName;
            dir = dir.Parent;
        }
        return "";
    }

    private static string FindXlsxyExe(string repo)
    {
        foreach (var rel in new[] { @"target\release\xlsxy.exe", @"target\debug\xlsxy.exe" })
        {
            var p = Path.Combine(repo, rel);
            if (File.Exists(p)) return p;
        }
        return "";
    }

    // ---- tree ----

    private void Search_Changed(object sender, System.Windows.Controls.TextChangedEventArgs e) => BuildTree();
    private void Grouping_Changed(object sender, RoutedEventArgs e) => BuildTree();

    private IEnumerable<FileEntry> FilteredFiles()
    {
        var q = (SearchBox.Text ?? "").Trim();
        if (q.Length == 0) return _manifest.Files;
        return _manifest.Files.Where(f =>
            f.Name.Contains(q, StringComparison.OrdinalIgnoreCase) ||
            f.Folder.Contains(q, StringComparison.OrdinalIgnoreCase) ||
            f.Category.Contains(q, StringComparison.OrdinalIgnoreCase) ||
            f.Tags.Any(t => t.Contains(q, StringComparison.OrdinalIgnoreCase)) ||
            f.Functions.Any(fn => fn.Contains(q, StringComparison.OrdinalIgnoreCase)));
    }

    private void BuildTree()
    {
        if (Tree is null) return;
        Tree.Items.Clear();
        var files = FilteredFiles().ToList();
        if (GroupTag.IsChecked == true) BuildByTag(files);
        else if (GroupFolder.IsChecked == true) BuildByFolder(files);
        else BuildByCategory(files);
    }

    private TreeViewItem Group(string header, int count) => new()
    {
        Header = $"{header}  ({count})",
        FontWeight = System.Windows.FontWeights.SemiBold,
        Foreground = System.Windows.Media.Brushes.White,
    };

    private TreeViewItem Leaf(FileEntry f)
    {
        var tip = (f.Folder.Length > 0 ? f.Folder + "\n" : "")
            + (f.Tags.Count > 0 ? string.Join(", ", f.Tags) : "(no feature tags)")
            + (f.Functions.Count > 0 ? "\n\nfunctions: " + string.Join(", ", f.Functions) : "");
        return new()
        {
            Header = f.Name,
            Tag = f,
            ToolTip = tip,
        };
    }

    private void BuildByCategory(List<FileEntry> files)
    {
        foreach (var g in files.GroupBy(f => f.Category).OrderBy(g => g.Key, StringComparer.OrdinalIgnoreCase))
        {
            var node = Group(g.Key, g.Count());
            foreach (var f in g.OrderBy(f => f.Folder).ThenBy(f => f.Name, StringComparer.OrdinalIgnoreCase))
                node.Items.Add(Leaf(f));
            if (SearchBox.Text?.Length > 0) node.IsExpanded = true;
            Tree.Items.Add(node);
        }
    }

    private void BuildByTag(List<FileEntry> files)
    {
        // order tags by the manifest's (count-desc) order, then any extras
        var order = _manifest.Tags.Keys.ToList();
        var byTag = new Dictionary<string, List<FileEntry>>();
        foreach (var f in files)
            foreach (var t in f.Tags)
                (byTag.TryGetValue(t, out var l) ? l : byTag[t] = new()).Add(f);
        var untagged = files.Where(f => f.Tags.Count == 0).ToList();

        foreach (var t in order.Where(byTag.ContainsKey).Concat(byTag.Keys.Except(order).OrderBy(x => x)))
        {
            var info = _manifest.Tags.TryGetValue(t, out var ti) ? ti.Doc : "";
            var node = Group(info.Length > 0 ? $"{t} — {info}" : t, byTag[t].Count);
            foreach (var f in byTag[t].OrderBy(f => f.Name, StringComparer.OrdinalIgnoreCase))
                node.Items.Add(Leaf(f));
            if (SearchBox.Text?.Length > 0) node.IsExpanded = true;
            Tree.Items.Add(node);
        }
        if (untagged.Count > 0)
        {
            var node = Group("(no feature tags)", untagged.Count);
            foreach (var f in untagged.OrderBy(f => f.Name, StringComparer.OrdinalIgnoreCase))
                node.Items.Add(Leaf(f));
            Tree.Items.Add(node);
        }
    }

    private void BuildByFolder(List<FileEntry> files)
    {
        var nodes = new Dictionary<string, TreeViewItem>();
        TreeViewItem Ensure(string key, string header)
        {
            if (nodes.TryGetValue(key, out var n)) return n;
            n = new TreeViewItem
            {
                Header = header,
                FontWeight = System.Windows.FontWeights.SemiBold,
                Foreground = System.Windows.Media.Brushes.White,
            };
            nodes[key] = n;
            return n;
        }
        foreach (var f in files.OrderBy(f => f.Folder).ThenBy(f => f.Name, StringComparer.OrdinalIgnoreCase))
        {
            var folder = f.Folder.Length == 0 ? "(root)" : f.Folder;
            var segs = folder.Split('/');
            string acc = "";
            TreeViewItem? parent = null;
            for (int i = 0; i < segs.Length; i++)
            {
                acc = i == 0 ? segs[i] : acc + "/" + segs[i];
                bool isNew = !nodes.ContainsKey(acc);
                var item = Ensure(acc, segs[i]);
                if (isNew)
                {
                    if (parent is null) Tree.Items.Add(item);
                    else parent.Items.Add(item);
                }
                parent = item;
            }
            parent!.Items.Add(Leaf(f));
        }
        if (SearchBox.Text?.Length > 0)
            foreach (TreeViewItem i in Tree.Items) i.IsExpanded = true;
    }

    private void Tree_SelectedItemChanged(object sender, RoutedPropertyChangedEventArgs<object> e)
    {
        if (e.NewValue is TreeViewItem { Tag: FileEntry fe })
        {
            _selected = fe;
            OpenSideBySide(fe);
        }
    }

    /// Absolute path of a corpus file on disk.
    private string FullPath(FileEntry fe) =>
        Path.GetFullPath(Path.Combine(_repoRoot, "corpus",
            fe.Path.Replace('/', Path.DirectorySeparatorChar)));

    private void Tree_KeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if (e.Key == System.Windows.Input.Key.C &&
            (System.Windows.Input.Keyboard.Modifiers & System.Windows.Input.ModifierKeys.Control) != 0)
        {
            CopySelectedPath();
            e.Handled = true;
        }
    }

    private void CopyPath_Click(object sender, RoutedEventArgs e) => CopySelectedPath();

    private void CopySelectedPath()
    {
        if (_selected is null)
        {
            StatusText.Text = "no file selected";
            return;
        }
        var p = FullPath(_selected);
        try
        {
            Clipboard.SetText(p);
            StatusText.Text = "copied path: " + _selected.Name;
        }
        catch (Exception ex)
        {
            StatusText.Text = "copy failed: " + ex.Message;
        }
    }

    // ---- launching Excel + xlsxy ----

    private void OpenSideBySide(FileEntry fe)
    {
        if (_xlsxyExe.Length == 0)
        {
            MessageBox.Show("xlsxy.exe not found. Build it first: cargo build", "xlsxy compare");
            return;
        }
        var full = FullPath(fe);
        if (!File.Exists(full))
        {
            MessageBox.Show("Missing file:\n" + full, "xlsxy compare");
            return;
        }

        CloseChildren();
        StatusText.Text = "opening " + fe.Name + " …";

        var wa = Win32.WorkArea();
        int half = (wa.Width - CollapsedW) / 2;
        var xlsxyRect = (x: wa.Left + CollapsedW, y: wa.Top, w: half, h: wa.Height);
        var excelRect = (x: wa.Left + CollapsedW + half, y: wa.Top, w: wa.Width - CollapsedW - half, h: wa.Height);

        // xlsxy in a fresh Windows Terminal window — unlike a classic console it
        // resizes/reflows freely, so SetWindowPos gives the exact size we want.
        // A unique tab title lets us find (and later close) just this window.
        var stem = Path.GetFileNameWithoutExtension(full);
        var wtTitle = "xlsxy: " + stem;
        try
        {
            var psi = new ProcessStartInfo { FileName = "wt.exe", UseShellExecute = true };
            foreach (var a in new[] { "-w", "new", "nt", "--title", wtTitle, "--suppressApplicationTitle",
                                      _xlsxyExe, full })
                psi.ArgumentList.Add(a);
            _xlsxy = Process.Start(psi);
        }
        catch (Exception ex) { StatusText.Text = "xlsxy failed: " + ex.Message; }

        // Excel via late-bound COM (so we can close it on the next switch)
        StartExcel(full);

        // both windows take a moment to appear — poll and position them
        bool placedXlsxy = false, placedExcel = false;
        int ticks = 0;
        _placer?.Stop();
        _placer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(120) };
        _placer.Tick += (_, _) =>
        {
            ticks++;
            if (!placedXlsxy)
            {
                var h = Win32.FindWindowByClassAndTitle("CASCADIA_HOSTING_WINDOW_CLASS", wtTitle);
                if (h != IntPtr.Zero)
                {
                    _xlsxyHwnd = h;
                    Win32.Place(h, xlsxyRect.x, xlsxyRect.y, xlsxyRect.w, xlsxyRect.h);
                    placedXlsxy = true;
                    // Placing the terminal just raised it to the top; if Excel is
                    // already down, lift it back above so the terminal's cell-snap
                    // spill stays hidden behind Excel.
                    if (placedExcel) Win32.Raise(_excelHwnd);
                }
            }
            if (!placedExcel)
            {
                var h = Win32.FindWindowByClassAndTitle("XLMAIN", stem);
                if (h != IntPtr.Zero)
                {
                    _excelHwnd = h;
                    Win32.Place(h, excelRect.x, excelRect.y, excelRect.w, excelRect.h);
                    placedExcel = true;
                }
            }
            if ((placedXlsxy && placedExcel) || ticks > 50)
            {
                // Final z-order: Excel sits above the terminal so the two tile
                // cleanly with no terminal overlap on Excel's edge.
                if (placedExcel) Win32.Raise(_excelHwnd);
                _placer!.Stop();
                StatusText.Text = fe.Name
                    + (placedXlsxy ? "" : "  (terminal not found)")
                    + (placedExcel ? "" : "  (Excel not found)");
            }
        };
        _placer.Start();

        _docOpen = true;
        Collapse();
    }

    private void StartExcel(string path)
    {
        try
        {
            var t = Type.GetTypeFromProgID("Excel.Application");
            if (t is null) { StatusText.Text = "Excel not installed"; return; }
            _excelApp = Activator.CreateInstance(t);
            _excelApp!.Visible = true;
            _excelApp.DisplayAlerts = false;
            _excelApp.WindowState = -4143; // xlNormal
            // Open(Filename, UpdateLinks=0, ReadOnly=true) — read-only so the
            // corpus copy is never modified.
            _excelBook = _excelApp.Workbooks.Open(path, 0, true);
        }
        catch (Exception ex)
        {
            StatusText.Text = "Excel failed: " + ex.Message;
        }
    }

    private void CloseChildren()
    {
        _placer?.Stop();
        try { if (_excelBook is not null) _excelBook.Close(false); } catch { }
        try { if (_excelApp is not null) _excelApp.Quit(); } catch { }
        _excelBook = null;
        _excelApp = null;
        _excelHwnd = IntPtr.Zero;
        // close the Windows Terminal window we opened (the wt.exe launcher process
        // has already handed off and exited, so close by window handle)
        if (_xlsxyHwnd != IntPtr.Zero)
        {
            try { Win32.PostMessageW(_xlsxyHwnd, Win32.WM_CLOSE, IntPtr.Zero, IntPtr.Zero); } catch { }
            _xlsxyHwnd = IntPtr.Zero;
        }
        try { if (_xlsxy is { HasExited: false }) _xlsxy.Kill(true); } catch { }
        _xlsxy = null;
    }

    // ---- collapse / expand the launcher strip ----

    private void Collapse()
    {
        _collapsed = true;
        ContentPanel.Visibility = Visibility.Collapsed;
        StripPanel.Visibility = Visibility.Visible;
        var wa = Win32.WorkArea();
        Win32.Place(_hwnd, wa.Left, wa.Top, CollapsedW, wa.Height);
    }

    private void Expand()
    {
        _collapsed = false;
        StripPanel.Visibility = Visibility.Collapsed;
        ContentPanel.Visibility = Visibility.Visible;
        var wa = Win32.WorkArea();
        Win32.Place(_hwnd, wa.Left, wa.Top, ExpandedW, wa.Height);
        Win32.SetForegroundWindow(_hwnd);
    }

    private void CloseBtn_Click(object sender, RoutedEventArgs e) => Close();
}
