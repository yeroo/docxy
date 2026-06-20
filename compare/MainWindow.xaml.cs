using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Threading;

namespace DocxyCompare;

public partial class MainWindow : Window
{
    private const int CollapsedW = 56;
    private const int ExpandedW = 460;

    private string _repoRoot = "";
    private string _docxyExe = "";
    private Manifest _manifest = new();

    private IntPtr _hwnd;
    private bool _docOpen;
    private bool _collapsed;
    private FileEntry? _selected;

    private Process? _docxy;
    private dynamic? _wordApp;
    private dynamic? _wordDoc;
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
            MessageBox.Show("Could not find corpus/classification.json above this app.\n" +
                            "Run corpus/tools/classify.py first.", "docxy compare");
            return;
        }
        var path = Path.Combine(_repoRoot, "corpus", "classification.json");
        try
        {
            var json = File.ReadAllText(path);
            _manifest = JsonSerializer.Deserialize<Manifest>(json,
                new JsonSerializerOptions { PropertyNameCaseInsensitive = true }) ?? new Manifest();
        }
        catch (Exception ex)
        {
            MessageBox.Show("Failed to read manifest:\n" + ex.Message, "docxy compare");
        }
        _docxyExe = FindDocxyExe(_repoRoot);
        SubTitle.Text = _docxyExe.Length > 0
            ? $"{_manifest.Count} files · docxy + Word side by side"
            : "docxy.exe not found — build it (cargo build)";
    }

    private static string LocateRepo()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            if (File.Exists(Path.Combine(dir.FullName, "corpus", "classification.json")))
                return dir.FullName;
            dir = dir.Parent;
        }
        return "";
    }

    private static string FindDocxyExe(string repo)
    {
        foreach (var rel in new[] { @"target\release\docxy.exe", @"target\debug\docxy.exe" })
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
            f.Tags.Any(t => t.Contains(q, StringComparison.OrdinalIgnoreCase)));
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

    private TreeViewItem Leaf(FileEntry f) => new()
    {
        Header = f.Name,
        Tag = f,
        ToolTip = $"{f.Folder}\n{(f.Tags.Count > 0 ? string.Join(", ", f.Tags) : "(no feature tags)")}",
    };

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

    // ---- launching Word + docxy ----

    private void OpenSideBySide(FileEntry fe)
    {
        if (_docxyExe.Length == 0)
        {
            MessageBox.Show("docxy.exe not found. Build it first: cargo build", "docxy compare");
            return;
        }
        var full = FullPath(fe);
        if (!File.Exists(full))
        {
            MessageBox.Show("Missing file:\n" + full, "docxy compare");
            return;
        }

        CloseChildren();
        StatusText.Text = "opening " + fe.Name + " …";

        var wa = Win32.WorkArea();
        int half = (wa.Width - CollapsedW) / 2;
        var docxyRect = (x: wa.Left + CollapsedW, y: wa.Top, w: half, h: wa.Height);
        var wordRect = (x: wa.Left + CollapsedW + half, y: wa.Top, w: wa.Width - CollapsedW - half, h: wa.Height);

        // docxy in its own console window
        try
        {
            _docxy = Process.Start(new ProcessStartInfo
            {
                FileName = _docxyExe,
                Arguments = $"\"{full}\"",
                UseShellExecute = true,
            });
        }
        catch (Exception ex) { StatusText.Text = "docxy failed: " + ex.Message; }

        // Word via late-bound COM (so we can close it on the next switch)
        StartWord(full);

        // both windows take a moment to appear — poll and position them
        var stem = Path.GetFileNameWithoutExtension(full);
        bool placedDocxy = false, placedWord = false;
        int ticks = 0;
        _placer?.Stop();
        _placer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(120) };
        _placer.Tick += (_, _) =>
        {
            ticks++;
            if (!placedDocxy && _docxy is { HasExited: false })
            {
                _docxy.Refresh();
                var h = _docxy.MainWindowHandle;
                if (h != IntPtr.Zero) { Win32.Place(h, docxyRect.x, docxyRect.y, docxyRect.w, docxyRect.h); placedDocxy = true; }
            }
            if (!placedWord)
            {
                var h = Win32.FindWindowByClassAndTitle("OpusApp", stem);
                if (h != IntPtr.Zero) { Win32.Place(h, wordRect.x, wordRect.y, wordRect.w, wordRect.h); placedWord = true; }
            }
            if ((placedDocxy && placedWord) || ticks > 50)
            {
                _placer!.Stop();
                StatusText.Text = fe.Name + (placedWord ? "" : "  (Word window not found)");
            }
        };
        _placer.Start();

        _docOpen = true;
        Collapse();
    }

    private void StartWord(string path)
    {
        try
        {
            var t = Type.GetTypeFromProgID("Word.Application");
            if (t is null) { StatusText.Text = "Word not installed"; return; }
            _wordApp = Activator.CreateInstance(t);
            _wordApp!.Visible = true;
            _wordApp.WindowState = 0; // wdWindowStateNormal
            // FileName, ConfirmConversions=false, ReadOnly=true
            _wordDoc = _wordApp.Documents.Open(path, false, true);
        }
        catch (Exception ex)
        {
            StatusText.Text = "Word failed: " + ex.Message;
        }
    }

    private void CloseChildren()
    {
        _placer?.Stop();
        try { if (_wordDoc is not null) _wordDoc.Close(false); } catch { }
        try { if (_wordApp is not null) _wordApp.Quit(); } catch { }
        _wordDoc = null;
        _wordApp = null;
        try { if (_docxy is { HasExited: false }) _docxy.Kill(true); } catch { }
        _docxy = null;
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
