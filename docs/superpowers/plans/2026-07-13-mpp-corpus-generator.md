# MS Project .mpp Corpus Generator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A VSTO AddIn for MS Project that generates a cumulative 46-step corpus of real `.mpp` + MSPDI `.xml` snapshot pairs, published as release zips from a new private `mpp-corpus` GitHub repo, fetchable into docxy's gitignored `corpus/mpp/`.

**Architecture:** All generation logic lives in `MppCorpus.Core` (plain .NET Framework 4.8 class library, typed against the MS Project PIA): a data-driven catalog of 46 `CorpusStep`s applied cumulatively to one live project, a `CorpusBuilder` that saves a `.mpp` + `.xml` pair after every step and records captured state into `manifest.json`. A console `MppCorpus.Runner` drives it headlessly for development; the VSTO `MppCorpus.AddIn` is a thin ribbon button + progress/abort dialog over the same `CorpusBuilder`.

**Tech Stack:** C# / .NET Framework 4.8 · MS Project PIA (`Microsoft.Office.Interop.MSProject`) · VSTO (VS2022 Community + Office/SharePoint development workload) · xunit for pure logic · System.Text.Json · `gh` CLI for repo/release · MS Project (Office16, installed) as the generation engine.

**Spec:** `docs/superpowers/specs/2026-07-13-mpp-corpus-generator-design.md` (docxy repo).

## Global Constraints

- Target framework **net48** everywhere; C# with `LangVersion` `latest`.
- Pinned project start: **Monday 2025-01-06 08:00**; pinned author string `mpp-corpus generator`; no volatile content (no `DateTime.Now`, no machine names).
- Step list is **pinned at 46 steps** with the exact slugs from the spec; adjacent snapshots must differ by exactly one feature.
- Snapshot naming: `NN-slug.mpp` / `NN-slug.xml` (`NN` = zero-padded index).
- Any step failure **stops generation** with an error naming the step; already-written snapshots stay on disk.
- New repo is **private**: `yeroo/mpp-corpus` at `C:\Users\boris_kudriashov\Source\mpp-corpus`.
- MS Project automation needs the **interactive desktop**; MS Project strings assume an **English (en-US) Project UI** (duration units `ed`, rates `50/h`).
- Non-goals: no MPP8/MPP9 generation, no CI generation, no decoding work.
- docxy-side changes go on the `claude/yppxy-project` branch.
- Commit after every task (both repos have git).

---

### Task 1: Install the Office/VSTO workload and pin the PIA path

**Files:**
- None in-repo (machine setup); records the PIA path used by Task 3.

**Interfaces:**
- Produces: `$(MSProjectPiaPath)` — the absolute path to `Microsoft.Office.Interop.MSProject.dll`, pinned into `addin/Directory.Build.props` in Task 3.

- [ ] **Step 1: Check what's already present** (PIA may already be in the GAC because Project is installed)

Run:
```powershell
Get-ChildItem "C:\Windows\assembly\GAC_MSIL\Microsoft.Office.Interop.MSProject" -Recurse -Filter *.dll -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName
Get-ChildItem "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Shared\Visual Studio Tools for Office\PIA" -Recurse -Filter Microsoft.Office.Interop.MSProject.dll -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName
```
Expected: at least one path once Step 2 completes (the GAC one may exist already).

- [ ] **Step 2: Install the workload** (needs admin elevation — run from an elevated PowerShell; a few-GB download)

Run:
```powershell
& "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe" modify --installPath "C:\Program Files\Microsoft Visual Studio\2022\Community" --add Microsoft.VisualStudio.Workload.Office --includeRecommended --passive --norestart
```
Wait for the installer to exit (`--passive` shows progress UI but needs no clicks).

- [ ] **Step 3: Verify the VSTO build targets and workload landed**

Run:
```powershell
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
& $vswhere -all -products * -requires Microsoft.VisualStudio.Workload.Office -format json | ConvertFrom-Json | ForEach-Object { "OFFICE WORKLOAD: $($_.displayName)" }
Test-Path "C:\Program Files\Microsoft Visual Studio\2022\Community\MSBuild\Microsoft\VisualStudio\v17.0\OfficeTools\Microsoft.VisualStudio.Tools.Office.targets"
```
Expected: `OFFICE WORKLOAD: Visual Studio Community 2022` and `True`. (If the targets file sits under a slightly different subpath, locate it with `Get-ChildItem ...\MSBuild -Recurse -Filter Microsoft.VisualStudio.Tools.Office.targets` and note the actual path.)

- [ ] **Step 4: Re-run Step 1 and record the PIA path**

Prefer the `...\Visual Studio Tools for Office\PIA\Office15\Microsoft.Office.Interop.MSProject.dll` path; the GAC path is the fallback. Record it — Task 3 pins it as `$(MSProjectPiaPath)`.

- [ ] **Step 5: Verify dotnet SDK + net48 targeting pack + gh auth** (needed by Tasks 2–3)

Run:
```powershell
dotnet --version
Test-Path "${env:ProgramFiles(x86)}\Reference Assemblies\Microsoft\Framework\.NETFramework\v4.8"
gh auth status
```
Expected: a dotnet version (if missing: `winget install Microsoft.DotNet.SDK.8`), `True` (if missing, add the ".NET Framework 4.8 targeting pack" component via the same VS installer), and a logged-in gh account with access to `yeroo`.

---

### Task 2: Create the private mpp-corpus repo and scaffold

**Files:**
- Create: `C:\Users\boris_kudriashov\Source\mpp-corpus\README.md`
- Create: `C:\Users\boris_kudriashov\Source\mpp-corpus\.gitignore`
- Create: `C:\Users\boris_kudriashov\Source\mpp-corpus\snapshots\.gitkeep`

**Interfaces:**
- Produces: the private GitHub repo `yeroo/mpp-corpus` with `main` pushed; local clone at `C:\Users\boris_kudriashov\Source\mpp-corpus` (all later tasks work there).

- [ ] **Step 1: Create the local repo and scaffold files**

`.gitignore`:
```gitignore
bin/
obj/
.vs/
*.user
*.pfx
```

`README.md`:
```markdown
# mpp-corpus (private)

A generated corpus of real Microsoft Project `.mpp` files for developing the
`mppread` decoder (yeroo/docxy). One project is built cumulatively, one feature
per step (46 steps); after every step a snapshot pair is saved:

- `snapshots/NN-slug.mpp` — real binary .mpp (current MPP generation)
- `snapshots/NN-slug.xml` — the same state as MSPDI XML (the documented oracle)

Adjacent snapshots differ by exactly one feature, so a feature can be localized
by diffing snapshot N against N+1 per CFB stream; `manifest.json` describes
each step and pins expected values captured from the live project.

## Regenerating

Requires Windows + MS Project (desktop, interactive) and VS2022 with the
Office workload. Either click **Corpus ▸ Generate corpus** in MS Project
(the `addin/` VSTO add-in) or run headlessly:

    addin\MppCorpus.Runner\bin\Release\MppCorpus.Runner.exe --out snapshots

## Licensing

All files are generated by this repo's own code driving a licensed MS Project
install; the contents are ours. The repo stays private out of caution.
```

Run:
```powershell
New-Item -ItemType Directory -Force "C:\Users\boris_kudriashov\Source\mpp-corpus\snapshots" | Out-Null
New-Item -ItemType File "C:\Users\boris_kudriashov\Source\mpp-corpus\snapshots\.gitkeep" | Out-Null
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" init -b main
```
(Then write the two files above with the exact content shown.)

- [ ] **Step 2: Create the private GitHub repo and push**

Run:
```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "scaffold: private .mpp corpus repo"
gh repo create yeroo/mpp-corpus --private --source "C:\Users\boris_kudriashov\Source\mpp-corpus" --push --description "Generated MS Project .mpp corpus for mppread (private)"
```

- [ ] **Step 3: Verify**

Run: `gh repo view yeroo/mpp-corpus --json isPrivate,defaultBranchRef`
Expected: `"isPrivate": true`, default branch `main`.

---

### Task 3: MppCorpus.Core skeleton — naming + manifest model (TDD)

**Files:**
- Create: `addin/Directory.Build.props`
- Create: `addin/MppCorpus.sln` (via `dotnet new sln`)
- Create: `addin/MppCorpus.Core/MppCorpus.Core.csproj`
- Create: `addin/MppCorpus.Core/Naming.cs`
- Create: `addin/MppCorpus.Core/Manifest.cs`
- Test: `addin/MppCorpus.Core.Tests/MppCorpus.Core.Tests.csproj`
- Test: `addin/MppCorpus.Core.Tests/NamingTests.cs`
- Test: `addin/MppCorpus.Core.Tests/ManifestTests.cs`

(All paths in Tasks 3–12 are relative to `C:\Users\boris_kudriashov\Source\mpp-corpus\`.)

**Interfaces:**
- Produces: `Naming.SnapshotName(int index, string slug) -> string` (`"06-unicode-name"`); manifest model types `CorpusManifest`, `StepRecord`, `ProjectState`, `TaskState`, `LinkState`, `ResourceState`, `AssignmentState`; `ManifestWriter.ToJson(CorpusManifest) -> string`.

- [ ] **Step 1: Create solution + projects**

`addin/Directory.Build.props` (pin the PIA path recorded in Task 1 Step 4):
```xml
<Project>
  <PropertyGroup>
    <MSProjectPiaPath>C:\Program Files (x86)\Microsoft Visual Studio\Shared\Visual Studio Tools for Office\PIA\Office15\Microsoft.Office.Interop.MSProject.dll</MSProjectPiaPath>
    <LangVersion>latest</LangVersion>
  </PropertyGroup>
</Project>
```

`addin/MppCorpus.Core/MppCorpus.Core.csproj`:
```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net48</TargetFramework>
    <RootNamespace>MppCorpus.Core</RootNamespace>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="System.Text.Json" Version="8.0.5" />
    <Reference Include="Microsoft.Office.Interop.MSProject">
      <HintPath>$(MSProjectPiaPath)</HintPath>
      <EmbedInteropTypes>true</EmbedInteropTypes>
    </Reference>
  </ItemGroup>
</Project>
```

`addin/MppCorpus.Core.Tests/MppCorpus.Core.Tests.csproj`:
```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net48</TargetFramework>
    <IsPackable>false</IsPackable>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="xunit" Version="2.9.2" />
    <PackageReference Include="xunit.runner.visualstudio" Version="2.8.2" />
    <PackageReference Include="Microsoft.NET.Test.Sdk" Version="17.11.1" />
  </ItemGroup>
  <ItemGroup>
    <ProjectReference Include="..\MppCorpus.Core\MppCorpus.Core.csproj" />
  </ItemGroup>
</Project>
```

Run (from `addin\`):
```powershell
dotnet new sln -n MppCorpus
dotnet sln add MppCorpus.Core MppCorpus.Core.Tests
```

- [ ] **Step 2: Write the failing tests**

`NamingTests.cs`:
```csharp
using MppCorpus.Core;
using Xunit;

public class NamingTests
{
    [Fact]
    public void PadsIndexToTwoDigits() => Assert.Equal("06-unicode-name", Naming.SnapshotName(6, "unicode-name"));

    [Fact]
    public void KeepsTwoDigitIndexes() => Assert.Equal("46-progress", Naming.SnapshotName(46, "progress"));
}
```

`ManifestTests.cs`:
```csharp
using MppCorpus.Core;
using Xunit;

public class ManifestTests
{
    [Fact]
    public void SerializesStepsWithStateInStableOrder()
    {
        var m = new CorpusManifest
        {
            GeneratorVersion = "0.1",
            ProjectVersion = "16.0",
            FormatsEmitted = new[] { "mpp", "mspdi-xml" },
            OldFormatProbe = "unsupported",
        };
        m.Steps.Add(new StepRecord
        {
            Index = 1, Slug = "empty", Description = "d", Changed = "c",
            State = new ProjectState
            {
                Tasks = { new TaskState { Uid = 1, Name = "S03 design widget", Start = "2025-01-06T08:00", Finish = "2025-01-08T17:00", OutlineLevel = 1, DurationMinutes = 1440 } },
                Links = { new LinkState { PredUid = 1, SuccUid = 2, Type = 1, LagMinutes = 960 } },
            },
        });
        var json = ManifestWriter.ToJson(m);
        Assert.Contains("\"slug\": \"empty\"", json);
        Assert.Contains("\"S03 design widget\"", json);
        Assert.Contains("\"lagMinutes\": 960", json);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run (from `addin\`): `dotnet test`
Expected: FAIL — `Naming`/`CorpusManifest` do not exist (compile errors count as the failing state).

- [ ] **Step 4: Implement**

`Naming.cs`:
```csharp
namespace MppCorpus.Core
{
    public static class Naming
    {
        public static string SnapshotName(int index, string slug) => $"{index:00}-{slug}";
    }
}
```

`Manifest.cs`:
```csharp
using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace MppCorpus.Core
{
    public sealed class CorpusManifest
    {
        public string GeneratorVersion { get; set; }
        public string ProjectVersion { get; set; }
        public string[] FormatsEmitted { get; set; }
        public string OldFormatProbe { get; set; }
        public List<StepRecord> Steps { get; set; } = new List<StepRecord>();
    }

    public sealed class StepRecord
    {
        public int Index { get; set; }
        public string Slug { get; set; }
        public string Description { get; set; }
        public string Changed { get; set; }
        public ProjectState State { get; set; }
    }

    public sealed class ProjectState
    {
        public List<TaskState> Tasks { get; set; } = new List<TaskState>();
        public List<LinkState> Links { get; set; } = new List<LinkState>();
        public List<ResourceState> Resources { get; set; } = new List<ResourceState>();
        public List<AssignmentState> Assignments { get; set; } = new List<AssignmentState>();
    }

    public sealed class TaskState
    {
        public int Uid { get; set; }
        public string Name { get; set; }
        public string Start { get; set; }
        public string Finish { get; set; }
        public int OutlineLevel { get; set; }
        public int DurationMinutes { get; set; }
        public bool Milestone { get; set; }
        public bool Summary { get; set; }
    }

    public sealed class LinkState
    {
        public int PredUid { get; set; }
        public int SuccUid { get; set; }
        public int Type { get; set; }
        public int LagMinutes { get; set; }
    }

    public sealed class ResourceState
    {
        public int Uid { get; set; }
        public string Name { get; set; }
        public int Type { get; set; }
    }

    public sealed class AssignmentState
    {
        public int TaskUid { get; set; }
        public int ResourceUid { get; set; }
        public double Units { get; set; }
    }

    public static class ManifestWriter
    {
        private static readonly JsonSerializerOptions Options = new JsonSerializerOptions
        {
            WriteIndented = true,
            PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        };

        public static string ToJson(CorpusManifest manifest) => JsonSerializer.Serialize(manifest, Options);
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `dotnet test`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "core: snapshot naming + manifest model (TDD)"
```

---

### Task 4: Step catalog skeleton — 46 pinned steps (TDD)

**Files:**
- Create: `addin/MppCorpus.Core/CorpusStep.cs`
- Create: `addin/MppCorpus.Core/StepCatalog.cs`
- Test: `addin/MppCorpus.Core.Tests/StepCatalogTests.cs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `CorpusStep { int Index; string Slug; string Description; string Changed; Action<Microsoft.Office.Interop.MSProject.Application> Apply; }`; `StepCatalog.Steps` — `IReadOnlyList<CorpusStep>` of exactly 46 entries in index order. Tasks 6–12 fill in the `Apply` bodies (initially `NotImplemented`).

- [ ] **Step 1: Write the failing tests**

`StepCatalogTests.cs`:
```csharp
using System.Linq;
using MppCorpus.Core;
using Xunit;

public class StepCatalogTests
{
    [Fact]
    public void HasExactly46Steps() => Assert.Equal(46, StepCatalog.Steps.Count);

    [Fact]
    public void IndexesAreContiguousFromOne() =>
        Assert.Equal(Enumerable.Range(1, 46), StepCatalog.Steps.Select(s => s.Index));

    [Fact]
    public void SlugsAreUniqueKebabCase()
    {
        Assert.Equal(46, StepCatalog.Steps.Select(s => s.Slug).Distinct().Count());
        Assert.All(StepCatalog.Steps, s => Assert.Matches("^[a-z0-9]+(-[a-z0-9]+)*$", s.Slug));
    }

    [Fact]
    public void SpecSlugsArePinned()
    {
        Assert.Equal("empty", StepCatalog.Steps[0].Slug);
        Assert.Equal("link-lag", StepCatalog.Steps[27].Slug);
        Assert.Equal("progress", StepCatalog.Steps[45].Slug);
    }

    [Fact]
    public void EveryStepHasDescriptionChangedAndApply() =>
        Assert.All(StepCatalog.Steps, s =>
        {
            Assert.False(string.IsNullOrWhiteSpace(s.Description));
            Assert.False(string.IsNullOrWhiteSpace(s.Changed));
            Assert.NotNull(s.Apply);
        });
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `dotnet test`
Expected: FAIL — `StepCatalog` does not exist.

- [ ] **Step 3: Implement the skeleton**

`CorpusStep.cs`:
```csharp
using System;
using MSProject = Microsoft.Office.Interop.MSProject;

namespace MppCorpus.Core
{
    public sealed class CorpusStep
    {
        public int Index { get; }
        public string Slug { get; }
        public string Description { get; }
        public string Changed { get; }
        public Action<MSProject.Application> Apply { get; }

        public CorpusStep(int index, string slug, string description, string changed, Action<MSProject.Application> apply)
        {
            Index = index; Slug = slug; Description = description; Changed = changed; Apply = apply;
        }
    }
}
```

`StepCatalog.cs` — all 46 entries with the spec's slugs/descriptions; every `Apply` is `app => throw new NotImplementedException("wired in a later task")` for now. The 46 (index, slug) pairs, verbatim from the spec: 1 `empty`, 2 `properties`, 3 `first-task`, 4 `more-tasks`, 5 `milestone`, 6 `unicode-name`, 7 `task-notes`, 8 `outline-2`, 9 `outline-deep`, 10 `manual-task`, 11 `inactive-task`, 12 `recurring-task`, 13 `split-task`, 14 `estimated-duration`, 15 `elapsed-duration`, 16 `deadline`, 17 `priority`, 18 `constraint-snet`, 19 `constraint-fnlt`, 20 `constraint-mso`, 21 `task-calendar`, 22 `hyperlink`, 23 `custom-fields`, 24 `link-fs`, 25 `link-ss`, 26 `link-ff`, 27 `link-sf`, 28 `link-lag`, 29 `link-lead`, 30 `multi-pred`, 31 `calendar-hours`, 32 `calendar-6day`, 33 `calendar-holiday`, 34 `resource-work`, 35 `resource-material`, 36 `resource-cost`, 37 `resource-rates`, 38 `resource-calendar`, 39 `assign-single`, 40 `assign-multi`, 41 `task-types`, 42 `work-contour`, 43 `assignment-delay`, 44 `baseline`, 45 `baseline1`, 46 `progress`. Take `Description` from the spec's step list lines and write a one-clause `Changed` for each (e.g. index 28: description "FS + 2d lag (top decode gap)", changed "added tasks 'S28 pred'/'S28 succ' linked FS with 960 min lag").

Structure:
```csharp
using System;
using System.Collections.Generic;
using MSProject = Microsoft.Office.Interop.MSProject;

namespace MppCorpus.Core
{
    public static class StepCatalog
    {
        public static IReadOnlyList<CorpusStep> Steps { get; } = Build();

        private static IReadOnlyList<CorpusStep> Build()
        {
            var steps = new List<CorpusStep>
            {
                new CorpusStep(1, "empty", "new project; pinned start date (Mon 2025-01-06), Standard calendar, pinned author",
                    "created blank project, ProjectStart=2025-01-06 08:00, author pinned", NotYet),
                // ... entries 2..46, same shape ...
            };
            return steps;
        }

        private static readonly Action<MSProject.Application> NotYet =
            app => throw new NotImplementedException("wired in a later task");
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `dotnet test`
Expected: PASS (9 tests).

- [ ] **Step 5: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "core: pinned 46-step catalog skeleton (TDD)"
```

---

### Task 5: CorpusBuilder + StateCapture + console Runner

**Files:**
- Create: `addin/MppCorpus.Core/CorpusBuilder.cs`
- Create: `addin/MppCorpus.Core/StateCapture.cs`
- Create: `addin/MppCorpus.Runner/MppCorpus.Runner.csproj`
- Create: `addin/MppCorpus.Runner/Program.cs`

**Interfaces:**
- Consumes: `StepCatalog.Steps`, `Naming.SnapshotName`, `ManifestWriter.ToJson`, manifest model.
- Produces: `CorpusBuilder.Run(MSProject.Application app, string snapshotsDir, string manifestPath, Action<string> report, Func<bool> cancelled, int throughIndex = int.MaxValue) -> CorpusManifest` (throws `CorpusStepException` naming the failed step); `StateCapture.Capture(MSProject.Project) -> ProjectState`; `MppCorpus.Runner.exe --out <dir> [--through N]`.

- [ ] **Step 1: Implement StateCapture**

`StateCapture.cs`:
```csharp
using System;
using System.Collections.Generic;
using System.Globalization;
using MSProject = Microsoft.Office.Interop.MSProject;

namespace MppCorpus.Core
{
    public static class StateCapture
    {
        public static ProjectState Capture(MSProject.Project p)
        {
            var state = new ProjectState();
            var seenLinks = new HashSet<(int, int)>();
            foreach (MSProject.Task t in p.Tasks)
            {
                if (t == null) continue; // blank rows enumerate as null
                state.Tasks.Add(new TaskState
                {
                    Uid = t.UniqueID,
                    Name = t.Name,
                    Start = Iso(t.Start),
                    Finish = Iso(t.Finish),
                    OutlineLevel = (int)t.OutlineLevel,
                    DurationMinutes = Convert.ToInt32(t.Duration, CultureInfo.InvariantCulture),
                    Milestone = (bool)t.Milestone,
                    Summary = (bool)t.Summary,
                });
                foreach (MSProject.TaskDependency d in t.TaskDependencies)
                {
                    var key = (d.From.UniqueID, d.To.UniqueID);
                    if (seenLinks.Add(key))
                        state.Links.Add(new LinkState
                        {
                            PredUid = d.From.UniqueID,
                            SuccUid = d.To.UniqueID,
                            Type = (int)d.Type,
                            LagMinutes = Convert.ToInt32(d.Lag, CultureInfo.InvariantCulture),
                        });
                }
                foreach (MSProject.Assignment a in t.Assignments)
                    state.Assignments.Add(new AssignmentState
                    {
                        TaskUid = a.TaskUniqueID,
                        ResourceUid = a.ResourceUniqueID,
                        Units = Convert.ToDouble(a.Units, CultureInfo.InvariantCulture),
                    });
            }
            foreach (MSProject.Resource r in p.Resources)
            {
                if (r == null) continue;
                state.Resources.Add(new ResourceState { Uid = r.UniqueID, Name = r.Name, Type = (int)r.Type });
            }
            return state;
        }

        private static string Iso(object comDate) =>
            comDate is DateTime dt ? dt.ToString("yyyy-MM-dd'T'HH:mm", CultureInfo.InvariantCulture) : null;
    }
}
```

- [ ] **Step 2: Implement CorpusBuilder**

`CorpusBuilder.cs`:
```csharp
using System;
using System.Globalization;
using System.IO;
using MSProject = Microsoft.Office.Interop.MSProject;

namespace MppCorpus.Core
{
    public sealed class CorpusStepException : Exception
    {
        public CorpusStepException(CorpusStep step, Exception inner)
            : base($"step {step.Index:00} '{step.Slug}' failed: {inner.Message}", inner) { }
    }

    public static class CorpusBuilder
    {
        public static CorpusManifest Run(MSProject.Application app, string snapshotsDir, string manifestPath,
            Action<string> report, Func<bool> cancelled, int throughIndex = int.MaxValue)
        {
            Directory.CreateDirectory(snapshotsDir);
            app.DisplayAlerts = false;
            app.ScreenUpdating = false;
            try
            {
                var manifest = new CorpusManifest
                {
                    GeneratorVersion = "0.1",
                    ProjectVersion = app.Version,
                    FormatsEmitted = new[] { "mpp", "mspdi-xml" },
                    OldFormatProbe = OldFormatProbe(app),
                };
                foreach (var step in StepCatalog.Steps)
                {
                    if (step.Index > throughIndex) break;
                    if (cancelled()) throw new OperationCanceledException("generation aborted by user");
                    report($"step {step.Index:00} {step.Slug}");
                    try { step.Apply(app); }
                    catch (Exception ex) when (!(ex is OperationCanceledException)) { throw new CorpusStepException(step, ex); }
                    var baseName = Naming.SnapshotName(step.Index, step.Slug);
                    app.FileSaveAs(Path.Combine(snapshotsDir, baseName + ".mpp"), MSProject.PjFileFormat.pjMPP);
                    app.FileSaveAs(Path.Combine(snapshotsDir, baseName + ".xml"), MSProject.PjFileFormat.pjXML);
                    manifest.Steps.Add(new StepRecord
                    {
                        Index = step.Index, Slug = step.Slug,
                        Description = step.Description, Changed = step.Changed,
                        State = StateCapture.Capture(app.ActiveProject),
                    });
                }
                File.WriteAllText(manifestPath, ManifestWriter.ToJson(manifest));
                report($"done: {manifest.Steps.Count} snapshots, manifest at {manifestPath}");
                return manifest;
            }
            finally
            {
                app.DisplayAlerts = true;
                app.ScreenUpdating = true;
            }
        }

        // Project 2013+ (major version >= 15) saves only the current .mpp generation;
        // record the fact instead of probing enum values that no longer exist.
        private static string OldFormatProbe(MSProject.Application app)
        {
            var major = int.Parse(app.Version.Split('.')[0], CultureInfo.InvariantCulture);
            return major >= 15
                ? $"unsupported: Project {app.Version} saves only the current .mpp generation"
                : $"untested: Project {app.Version} predates 2013 — extend the probe before trusting";
        }
    }
}
```

- [ ] **Step 3: Implement the Runner**

`MppCorpus.Runner/MppCorpus.Runner.csproj`:
```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net48</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="..\MppCorpus.Core\MppCorpus.Core.csproj" />
    <Reference Include="Microsoft.Office.Interop.MSProject">
      <HintPath>$(MSProjectPiaPath)</HintPath>
      <EmbedInteropTypes>true</EmbedInteropTypes>
    </Reference>
  </ItemGroup>
</Project>
```

`Program.cs`:
```csharp
using System;
using MppCorpus.Core;
using MSProject = Microsoft.Office.Interop.MSProject;

internal static class Program
{
    private static int Main(string[] args)
    {
        string outDir = null;
        int through = int.MaxValue;
        for (int i = 0; i < args.Length; i++)
        {
            if (args[i] == "--out" && i + 1 < args.Length) outDir = args[++i];
            else if (args[i] == "--through" && i + 1 < args.Length) through = int.Parse(args[++i]);
        }
        if (outDir == null)
        {
            Console.Error.WriteLine("usage: MppCorpus.Runner --out <snapshotsDir> [--through N]");
            return 2;
        }
        var manifestPath = System.IO.Path.Combine(System.IO.Path.GetDirectoryName(System.IO.Path.GetFullPath(outDir)) ?? outDir, "manifest.json");
        var app = (MSProject.Application)Activator.CreateInstance(Type.GetTypeFromProgID("MSProject.Application"));
        try
        {
            CorpusBuilder.Run(app, outDir, manifestPath, Console.WriteLine, () => false, through);
            return 0;
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine("FAILED: " + ex.Message);
            return 1;
        }
        finally
        {
            app.FileCloseAll(MSProject.PjSaveType.pjDoNotSave);
            app.Quit(MSProject.PjSaveType.pjDoNotSave);
        }
    }
}
```

- [ ] **Step 4: Build everything, run unit tests**

Run (from `addin\`):
```powershell
dotnet sln add MppCorpus.Runner
dotnet build
dotnet test
```
Expected: build succeeds, all tests still PASS. (No COM run yet — every `Apply` still throws; the runner gets its first real run in Task 6.)

- [ ] **Step 5: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "core: CorpusBuilder + StateCapture + headless runner"
```

---

### Task 6: Step helpers + group A (steps 01–02) — first real generation

**Files:**
- Create: `addin/MppCorpus.Core/StepHelpers.cs`
- Modify: `addin/MppCorpus.Core/StepCatalog.cs` (replace `NotYet` for steps 1–2)

**Interfaces:**
- Produces (used by all later step groups): `StepHelpers.Start` (2025-01-06 08:00), `StepHelpers.Day` (=480), `P(app)`, `Add(app, name)`, `ByTask(app, name)`, `ByResource(app, name)`, `T(h, m)`.

- [ ] **Step 1: Implement helpers**

`StepHelpers.cs`:
```csharp
using System;
using MSProject = Microsoft.Office.Interop.MSProject;

namespace MppCorpus.Core
{
    internal static class StepHelpers
    {
        public static readonly DateTime Start = new DateTime(2025, 1, 6, 8, 0, 0); // Monday
        public const int Day = 480; // working minutes per day (8h)

        public static MSProject.Project P(MSProject.Application app) => app.ActiveProject;

        public static MSProject.Task Add(MSProject.Application app, string name) => P(app).Tasks.Add(name);

        public static MSProject.Task ByTask(MSProject.Application app, string name)
        {
            foreach (MSProject.Task t in P(app).Tasks)
                if (t != null && t.Name == name) return t;
            throw new InvalidOperationException("task not found: " + name);
        }

        public static MSProject.Resource ByResource(MSProject.Application app, string name)
        {
            foreach (MSProject.Resource r in P(app).Resources)
                if (r != null && r.Name == name) return r;
            throw new InvalidOperationException("resource not found: " + name);
        }

        // A time-of-day value for calendar shifts (the date part is ignored by Project).
        public static DateTime T(int hour, int minute) => new DateTime(2025, 1, 6, hour, minute, 0);
    }
}
```

- [ ] **Step 2: Wire steps 01–02** (in `StepCatalog.cs`, using `using static MppCorpus.Core.StepHelpers;`)

```csharp
new CorpusStep(1, "empty",
    "new project; pinned start date (Mon 2025-01-06), Standard calendar, pinned author",
    "created blank project; ProjectStart=2025-01-06 08:00; author pinned",
    app =>
    {
        app.FileNew();
        var p = P(app);
        p.ProjectStart = Start;
        ((dynamic)p.BuiltinDocumentProperties)["Author"].Value = "mpp-corpus generator";
    }),
new CorpusStep(2, "properties",
    "title, subject, author, company, comments",
    "set Title/Subject/Company/Comments document properties and project notes",
    app =>
    {
        var dp = (dynamic)P(app).BuiltinDocumentProperties;
        dp["Title"].Value = "MPP corpus";
        dp["Subject"].Value = "Feature tour";
        dp["Company"].Value = "yppxy";
        dp["Comments"].Value = "Cumulative one-feature-per-step corpus";
        P(app).ProjectNotes = "Generated corpus project";
    }),
```

- [ ] **Step 3: Build + unit tests, then first real COM run**

Run (from `addin\`):
```powershell
dotnet build && dotnet test
$out = "$env:TEMP\mpp-smoke"
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
.\MppCorpus.Runner\bin\Debug\net48\MppCorpus.Runner.exe --out $out --through 2
Get-ChildItem $out
```
Expected: MS Project launches and quits; `01-empty.mpp`, `01-empty.xml`, `02-properties.mpp`, `02-properties.xml` exist; `manifest.json` next to the folder. If Project shows a modal dialog on `FileSaveAs` overwrite or format, note the dialog text and suppress it (that is what `DisplayAlerts = false` is for — if a specific alert still appears, handle it before proceeding).

- [ ] **Step 4: Cross-check with mppread** (docxy checkout, usual vcvars env)

Run (from `C:\Users\boris_kudriashov\Source\docxy`):
```powershell
cargo run -p mppread --example streams -- "$env:TEMP\mpp-smoke\01-empty.mpp"
```
Expected: CFB container parses; metadata prints (Title/Author from steps 1–2 in the `02-properties` file). Name/task decode may fail on this newest-generation MPP — that's a known gap this corpus exists to close, not a task failure.

- [ ] **Step 5: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "steps: shell group (01 empty, 02 properties); first end-to-end run"
```

---

### Task 7: Group B — task features (steps 03–23)

**Files:**
- Modify: `addin/MppCorpus.Core/StepCatalog.cs` (replace `NotYet` for steps 3–23)

**Interfaces:**
- Consumes: `StepHelpers` (Task 6). Later groups reference tasks created here only via `ByTask` with the exact names below.

- [ ] **Step 1: Wire steps 03–23**

Every step adds its own artifacts, named `SNN <thing>` so the step is greppable in both the binary and the XML. Complete bodies (descriptions/`Changed` texts follow the same pattern as Task 6 — description from the spec line, `Changed` naming the exact artifacts):

```csharp
// 3 first-task
app => { var t = Add(app, "S03 design widget"); t.Duration = 3 * Day; }
// 4 more-tasks
app =>
{
    foreach (var (name, days) in new[] { ("S04 gather parts", 1), ("S04 assemble", 2), ("S04 test", 5), ("S04 ship", 10) })
    { var t = Add(app, name); t.Duration = days * Day; }
}
// 5 milestone
app => { var t = Add(app, "S05 kickoff done"); t.Duration = 0; }
// 6 unicode-name (~200 chars, Latin+Cyrillic+CJK+emoji — stresses Var2Data strings)
app =>
{
    var name = "S06 длинное имя задачи 日本語のタスク名 🚀 " + string.Concat(Enumerable.Repeat("padding-", 20)) + "конец";
    Add(app, name).Duration = Day;
}
// 7 task-notes
app => { var t = Add(app, "S07 notes task"); t.Duration = Day; t.Notes = "First line.\nSecond line — unicode ✓ and «quotes»."; }
// 8 outline-2
app =>
{
    var parent = Add(app, "S08 phase");   parent.OutlineLevel = 1;
    var a = Add(app, "S08 child A");      a.OutlineLevel = 2; a.Duration = Day;
    var b = Add(app, "S08 child B");      b.OutlineLevel = 2; b.Duration = 2 * Day;
}
// 9 outline-deep (depth 4)
app =>
{
    var l1 = Add(app, "S09 epic");    l1.OutlineLevel = 1;
    var l2 = Add(app, "S09 feature"); l2.OutlineLevel = 2;
    var l3 = Add(app, "S09 story");   l3.OutlineLevel = 3;
    var l4 = Add(app, "S09 subtask"); l4.OutlineLevel = 4; l4.Duration = Day;
}
// 10 manual-task
app => { var t = Add(app, "S10 manual task"); t.OutlineLevel = 1; t.Manual = true; t.Duration = 2 * Day; }
// 11 inactive-task
app => { var t = Add(app, "S11 inactive task"); t.OutlineLevel = 1; t.Duration = Day; t.Active = false; }
// 12 recurring-task — Project's OM cannot set the native Recurring flag; build the
// structural equivalent (summary + 4 pinned weekly occurrences) and say so in Changed.
app =>
{
    var summary = Add(app, "S12 weekly sync"); summary.OutlineLevel = 1;
    for (int i = 0; i < 4; i++)
    {
        var occ = Add(app, $"S12 weekly sync {i + 1}");
        occ.OutlineLevel = 2;
        occ.Duration = 60;
        occ.ConstraintType = MSProject.PjConstraint.pjMSO;
        occ.ConstraintDate = Start.AddDays(7 * i).AddHours(1); // 09:00 each Monday
    }
}
// 13 split-task (4d task, Tuesday carved out -> two segments)
app =>
{
    var t = Add(app, "S13 split task"); t.OutlineLevel = 1; t.Duration = 4 * Day;
    t.Split(Start.AddDays(1), Start.AddDays(2));
}
// 14 estimated-duration
app => { var t = Add(app, "S14 estimated"); t.OutlineLevel = 1; t.Duration = 2 * Day; t.Estimated = true; }
// 15 elapsed-duration (string form is what carries the elapsed unit)
app => { var t = Add(app, "S15 elapsed"); t.OutlineLevel = 1; ((dynamic)t).Duration = "3ed"; }
// 16 deadline
app => { var t = Add(app, "S16 deadline task"); t.OutlineLevel = 1; t.Duration = Day; t.Deadline = Start.AddDays(14); }
// 17 priority
app => { var t = Add(app, "S17 priority task"); t.OutlineLevel = 1; t.Duration = Day; t.Priority = 900; }
// 18 constraint-snet
app => { var t = Add(app, "S18 snet task"); t.OutlineLevel = 1; t.Duration = Day;
         t.ConstraintType = MSProject.PjConstraint.pjSNET; t.ConstraintDate = Start.AddDays(7); }
// 19 constraint-fnlt
app => { var t = Add(app, "S19 fnlt task"); t.OutlineLevel = 1; t.Duration = Day;
         t.ConstraintType = MSProject.PjConstraint.pjFNLT; t.ConstraintDate = Start.AddDays(21).AddHours(9); }
// 20 constraint-mso
app => { var t = Add(app, "S20 mso task"); t.OutlineLevel = 1; t.Duration = Day;
         t.ConstraintType = MSProject.PjConstraint.pjMSO; t.ConstraintDate = Start.AddDays(10); }
// 21 task-calendar (built-in "24 Hours")
app => { var t = Add(app, "S21 around the clock"); t.OutlineLevel = 1; t.Duration = Day; t.Calendar = "24 Hours"; }
// 22 hyperlink
app => { var t = Add(app, "S22 linked task"); t.OutlineLevel = 1; t.Duration = Day;
         t.Hyperlink = "yppxy spec"; t.HyperlinkAddress = "https://example.org/yppxy"; }
// 23 custom-fields
app =>
{
    var t = Add(app, "S23 custom fields"); t.OutlineLevel = 1; t.Duration = Day;
    t.Text1 = "corpus-text"; t.Number1 = 42; t.Flag1 = true; t.Date1 = Start.AddDays(30);
}
```

Add `using System.Linq;` and `using MSProject = Microsoft.Office.Interop.MSProject;` to `StepCatalog.cs` as needed. If a property name doesn't compile against the PIA (e.g. `Task.Calendar`, `PjConstraint` member spellings), fix to the PIA's actual member — the intent of each line is pinned by the `Changed` text, not the exact spelling.

- [ ] **Step 2: Build + unit tests + run through step 23**

Run (from `addin\`):
```powershell
dotnet build && dotnet test
Remove-Item -Recurse -Force "$env:TEMP\mpp-smoke" -ErrorAction SilentlyContinue
.\MppCorpus.Runner\bin\Debug\net48\MppCorpus.Runner.exe --out "$env:TEMP\mpp-smoke" --through 23
(Get-ChildItem "$env:TEMP\mpp-smoke" -Filter *.mpp).Count
```
Expected: exit 0, count = 23.

- [ ] **Step 3: Spot-check the XML oracle**

Run:
```powershell
Select-String -Path "$env:TEMP\mpp-smoke\23-custom-fields.xml" -Pattern "S06|S13 split|S15 elapsed|S23 custom" -Encoding utf8 | Select-Object -First 8
```
Expected: matches for all four (the unicode name, the split task, the elapsed task, the custom-fields task). Also verify the split visually once: open `13-split-task.xml` and confirm the S13 task has two `<TimephasedData>`/stop-resume markers (a split task shows `Stop`/`Resume` dates).

- [ ] **Step 4: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "steps: task-feature group (03-23)"
```

---

### Task 8: Group C — links (steps 24–30)

**Files:**
- Modify: `addin/MppCorpus.Core/StepCatalog.cs` (steps 24–30)

**Interfaces:**
- Consumes: `StepHelpers`.

- [ ] **Step 1: Wire steps 24–30**

```csharp
// Local helper at the top of the Build() method:
Action<MSProject.Application, string, MSProject.PjTaskLinkType, object> link = (app, prefix, type, lag) =>
{
    var a = Add(app, prefix + " pred"); a.OutlineLevel = 1; a.Duration = 2 * Day;
    var b = Add(app, prefix + " succ"); b.OutlineLevel = 1; b.Duration = 2 * Day;
    b.LinkPredecessors(a, type, lag);
};

// 24 link-fs
app => link(app, "S24", MSProject.PjTaskLinkType.pjFinishToStart, 0)
// 25 link-ss
app => link(app, "S25", MSProject.PjTaskLinkType.pjStartToStart, 0)
// 26 link-ff
app => link(app, "S26", MSProject.PjTaskLinkType.pjFinishToFinish, 0)
// 27 link-sf
app => link(app, "S27", MSProject.PjTaskLinkType.pjStartToFinish, 0)
// 28 link-lag (top decode gap: 2d = 960 min)
app => link(app, "S28", MSProject.PjTaskLinkType.pjFinishToStart, 960)
// 29 link-lead (-1d = -480 min)
app => link(app, "S29", MSProject.PjTaskLinkType.pjFinishToStart, -480)
// 30 multi-pred
app =>
{
    var a = Add(app, "S30 pred A"); a.OutlineLevel = 1; a.Duration = Day;
    var b = Add(app, "S30 pred B"); b.OutlineLevel = 1; b.Duration = 2 * Day;
    var c = Add(app, "S30 join");   c.OutlineLevel = 1; c.Duration = Day;
    c.LinkPredecessors(a, MSProject.PjTaskLinkType.pjFinishToStart, 0);
    c.LinkPredecessors(b, MSProject.PjTaskLinkType.pjFinishToStart, 0);
}
```

- [ ] **Step 2: Build + tests + run through step 30, verify lag in oracle**

Run (from `addin\`):
```powershell
dotnet build && dotnet test
Remove-Item -Recurse -Force "$env:TEMP\mpp-smoke" -ErrorAction SilentlyContinue
.\MppCorpus.Runner\bin\Debug\net48\MppCorpus.Runner.exe --out "$env:TEMP\mpp-smoke" --through 30
Select-String -Path "$env:TEMP\mpp-smoke\30-multi-pred.xml" -Pattern "<LinkLag>9600</LinkLag>|<LinkLag>-4800</LinkLag>" | Select-Object -First 4
```
Expected: exit 0; both lag values found — MSPDI stores lag in **tenths of a minute**, so 960 min appears as `9600` and −480 as `-4800`. Also check `manifest.json` step 28 has `"lagMinutes": 960`.

- [ ] **Step 3: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "steps: link group (24-30) incl. lag/lead oracle pair"
```

---

### Task 9: Group D — calendars (steps 31–33)

**Files:**
- Modify: `addin/MppCorpus.Core/StepCatalog.cs` (steps 31–33)

**Interfaces:**
- Consumes: `StepHelpers` (`T(h,m)` for shift times).

- [ ] **Step 1: Wire steps 31–33**

```csharp
// 31 calendar-hours — edit Standard working times (this reschedules everything;
// that IS the feature; the paired XML records the new truth)
app =>
{
    var cal = P(app).BaseCalendars["Standard"];
    foreach (var d in new[] { MSProject.PjWeekday.pjMonday, MSProject.PjWeekday.pjTuesday,
                              MSProject.PjWeekday.pjWednesday, MSProject.PjWeekday.pjThursday,
                              MSProject.PjWeekday.pjFriday })
    {
        var wd = cal.WeekDays[d];
        wd.Shift1.Start = T(8, 30); wd.Shift1.Finish = T(12, 0);
        wd.Shift2.Start = T(13, 0); wd.Shift2.Finish = T(17, 30);
    }
}
// 32 calendar-6day — new base calendar with working Saturdays, used by a task
app =>
{
    app.BaseCalendarCreate("SixDay", "Standard");
    var cal = P(app).BaseCalendars["SixDay"];
    var sat = cal.WeekDays[MSProject.PjWeekday.pjSaturday];
    sat.Working = true;
    sat.Shift1.Start = T(9, 0); sat.Shift1.Finish = T(13, 0);
    var t = Add(app, "S32 sixday task"); t.OutlineLevel = 1; t.Duration = 3 * Day; t.Calendar = "SixDay";
}
// 33 calendar-holiday — exception in Standard (Type 1 = daily/one-shot)
app =>
{
    var cal = P(app).BaseCalendars["Standard"];
    cal.Exceptions.Add(1, Start.AddDays(9), Start.AddDays(9), "S33 founders day");
}
```

- [ ] **Step 2: Build + tests + run through 33, verify exception in oracle**

Run (from `addin\`):
```powershell
dotnet build && dotnet test
Remove-Item -Recurse -Force "$env:TEMP\mpp-smoke" -ErrorAction SilentlyContinue
.\MppCorpus.Runner\bin\Debug\net48\MppCorpus.Runner.exe --out "$env:TEMP\mpp-smoke" --through 33
Select-String -Path "$env:TEMP\mpp-smoke\33-calendar-holiday.xml" -Pattern "S33 founders day|SixDay" | Select-Object -First 4
```
Expected: exit 0; both names appear in the MSPDI calendar section.

- [ ] **Step 3: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "steps: calendar group (31-33)"
```

---

### Task 10: Group E — resources (steps 34–38)

**Files:**
- Modify: `addin/MppCorpus.Core/StepCatalog.cs` (steps 34–38)

**Interfaces:**
- Consumes: `StepHelpers.ByResource`. Produces resources `Alice` (work), `S35 cement` (material), `S36 travel budget` (cost) used by Tasks 11–12.

- [ ] **Step 1: Wire steps 34–38**

```csharp
// 34 resource-work
app => { var r = P(app).Resources.Add("Alice"); r.Initials = "AL"; }
// 35 resource-material
app => { var r = P(app).Resources.Add("S35 cement");
         r.Type = MSProject.PjResourceTypes.pjResourceTypeMaterial; r.MaterialLabel = "bags"; }
// 36 resource-cost
app => { var r = P(app).Resources.Add("S36 travel budget");
         r.Type = MSProject.PjResourceTypes.pjResourceTypeCost; }
// 37 resource-rates (en-US rate strings; MaxUnits 3 = 300%)
app =>
{
    var r = ByResource(app, "Alice");
    r.MaxUnits = 3;
    ((dynamic)r).StandardRate = "50/h";
    ((dynamic)r).OvertimeRate = "75/h";
    r.CostPerUse = 100;
}
// 38 resource-calendar — vacation day for Alice
app =>
{
    var r = ByResource(app, "Alice");
    r.CalendarObject.Exceptions.Add(1, Start.AddDays(16), Start.AddDays(16), "S38 vacation");
}
```

- [ ] **Step 2: Build + tests + run through 38, verify in oracle**

Run (from `addin\`):
```powershell
dotnet build && dotnet test
Remove-Item -Recurse -Force "$env:TEMP\mpp-smoke" -ErrorAction SilentlyContinue
.\MppCorpus.Runner\bin\Debug\net48\MppCorpus.Runner.exe --out "$env:TEMP\mpp-smoke" --through 38
Select-String -Path "$env:TEMP\mpp-smoke\38-resource-calendar.xml" -Pattern "Alice|bags|S38 vacation" | Select-Object -First 6
```
Expected: exit 0; all three appear. `manifest.json` step 36 state shows three resources with types 0/1/2 (work/material/cost).

- [ ] **Step 3: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "steps: resource group (34-38)"
```

---

### Task 11: Group F — assignments (steps 39–43)

**Files:**
- Modify: `addin/MppCorpus.Core/StepCatalog.cs` (steps 39–43)

**Interfaces:**
- Consumes: resource `Alice` (Task 10), `StepHelpers`.

- [ ] **Step 1: Wire steps 39–43**

```csharp
// 39 assign-single
app =>
{
    var t = Add(app, "S39 build"); t.OutlineLevel = 1; t.Duration = 2 * Day;
    t.Assignments.Add(t.ID, ByResource(app, "Alice").ID, 1);
}
// 40 assign-multi (adds Bob as a second work resource; Changed notes this)
app =>
{
    P(app).Resources.Add("Bob");
    var t = Add(app, "S40 pair work"); t.OutlineLevel = 1; t.Duration = 2 * Day;
    t.Assignments.Add(t.ID, ByResource(app, "Alice").ID, 0.5);
    t.Assignments.Add(t.ID, ByResource(app, "Bob").ID, 0.5);
}
// 41 task-types
app =>
{
    var fw = Add(app, "S41 fixed work"); fw.OutlineLevel = 1; fw.Duration = 2 * Day;
    fw.Type = MSProject.PjTaskFixedType.pjFixedWork;
    var fd = Add(app, "S41 fixed duration"); fd.OutlineLevel = 1; fd.Duration = 2 * Day;
    fd.Type = MSProject.PjTaskFixedType.pjFixedDuration; fd.EffortDriven = false;
}
// 42 work-contour
app =>
{
    var t = Add(app, "S42 contoured"); t.OutlineLevel = 1; t.Duration = 4 * Day;
    t.Assignments.Add(t.ID, ByResource(app, "Alice").ID, 1);
    foreach (MSProject.Assignment a in t.Assignments) a.WorkContour = MSProject.PjWorkContour.pjBackLoaded;
}
// 43 assignment-delay
app =>
{
    var t = Add(app, "S43 delayed start"); t.OutlineLevel = 1; t.Duration = 2 * Day;
    t.Assignments.Add(t.ID, ByResource(app, "Alice").ID, 1);
    foreach (MSProject.Assignment a in t.Assignments) ((dynamic)a).Delay = 480;
}
```
(As in Task 7: if an enum member spelling differs in the PIA — e.g. `PjWorkContour` members — fix to the PIA's actual name; the `Changed` text pins the intent.)

- [ ] **Step 2: Build + tests + run through 43, verify in oracle**

Run (from `addin\`):
```powershell
dotnet build && dotnet test
Remove-Item -Recurse -Force "$env:TEMP\mpp-smoke" -ErrorAction SilentlyContinue
.\MppCorpus.Runner\bin\Debug\net48\MppCorpus.Runner.exe --out "$env:TEMP\mpp-smoke" --through 43
Select-String -Path "$env:TEMP\mpp-smoke\43-assignment-delay.xml" -Pattern "S40 pair work|S42 contoured|Bob" | Select-Object -First 6
```
Expected: exit 0; matches found; `manifest.json` step 40 shows two 0.5-unit assignments on the `S40 pair work` task.

- [ ] **Step 3: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "steps: assignment group (39-43)"
```

---

### Task 12: Group G — tracking (steps 44–46), full 46-step run

**Files:**
- Modify: `addin/MppCorpus.Core/StepCatalog.cs` (steps 44–46)

**Interfaces:**
- Consumes: tasks `S39 build`, `S24 pred` (earlier groups), `app.BaselineSave`.

- [ ] **Step 1: Wire steps 44–46**

```csharp
// 44 baseline
app => app.BaselineSave(true)
// 45 baseline1 (Into: 1 = Baseline1)
app => app.BaselineSave(true, Into: 1)
// 46 progress — the one deliberate mutation of earlier artifacts (spec: "% complete
// and actual start on several tasks")
app =>
{
    ByTask(app, "S24 pred").PercentComplete = 100;
    var t = ByTask(app, "S39 build");
    t.PercentComplete = 50;
}
```
(`BaselineSave`'s COM signature is `BaselineSave(All, Copy, Into, From, To)` — if named args fight the PIA, pass positionally: `app.BaselineSave(true, Type.Missing, 1)`.)

- [ ] **Step 2: Full run — all 46 steps**

Run (from `addin\`):
```powershell
dotnet build && dotnet test
Remove-Item -Recurse -Force "$env:TEMP\mpp-full" -ErrorAction SilentlyContinue
.\MppCorpus.Runner\bin\Debug\net48\MppCorpus.Runner.exe --out "$env:TEMP\mpp-full"
(Get-ChildItem "$env:TEMP\mpp-full" -Filter *.mpp).Count
(Get-ChildItem "$env:TEMP\mpp-full" -Filter *.xml).Count
```
Expected: exit 0; 46 and 46; manifest lists 46 steps with `OldFormatProbe` recorded.

- [ ] **Step 3: Verify baselines + progress in the oracle**

Run:
```powershell
Select-String -Path "$env:TEMP\mpp-full\46-progress.xml" -Pattern "<Baseline>|<PercentComplete>100</PercentComplete>|<PercentComplete>50</PercentComplete>" | Select-Object -First 6
```
Expected: baseline blocks and both percent-complete values present.

- [ ] **Step 4: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "steps: tracking group (44-46); full 46-step generation works headlessly"
```

---

### Task 13: The VSTO AddIn — ribbon button + progress/abort dialog

**Files:**
- Create: `addin/MppCorpus.AddIn/` — scaffolded by the VS template, then:
- Create: `addin/MppCorpus.AddIn/CorpusRibbon.cs`
- Create: `addin/MppCorpus.AddIn/CorpusRibbon.xml` (embedded resource)
- Create: `addin/MppCorpus.AddIn/ProgressForm.cs`
- Modify: `addin/MppCorpus.AddIn/ThisAddIn.cs`

**Interfaces:**
- Consumes: `CorpusBuilder.Run(app, snapshotsDir, manifestPath, report, cancelled)`.
- Produces: ribbon tab **Corpus** with **Generate corpus** button inside MS Project.

- [ ] **Step 1: Scaffold via the VS template** (interactive, one-time — the VSTO project system's generated plumbing is not worth hand-authoring)

In Visual Studio 2022: **File ▸ New ▸ Project ▸ search "Project VSTO Add-in" (C#)** → name `MppCorpus.AddIn`, location `C:\Users\boris_kudriashov\Source\mpp-corpus\addin\`, **Place solution and project in the same directory: off**, then close VS. Delete the extra `.sln` the wizard made (keep ours) and run from `addin\`:
```powershell
dotnet sln add MppCorpus.AddIn
```
Then add to `MppCorpus.AddIn.csproj` (it is an old-style csproj — edit by hand) inside an `<ItemGroup>`:
```xml
<ProjectReference Include="..\MppCorpus.Core\MppCorpus.Core.csproj">
  <Project>{PUT-THE-GUID-FROM-CORE-CSPROJ-OR-ANY-GUID}</Project>
  <Name>MppCorpus.Core</Name>
</ProjectReference>
```
(SDK-style projects have no ProjectGuid; any fresh GUID works for the old-style reference metadata.)

- [ ] **Step 2: Add the ribbon (XML flavor)**

`CorpusRibbon.xml` (Build Action: **EmbeddedResource**):
```xml
<customUI xmlns="http://schemas.microsoft.com/office/2009/07/customui" onLoad="Ribbon_Load">
  <ribbon>
    <tabs>
      <tab id="tabCorpus" label="Corpus">
        <group id="grpCorpus" label="Corpus">
          <button id="btnGenerate" label="Generate corpus" size="large"
                  imageMso="MacroPlay" onAction="OnGenerate"/>
        </group>
      </tab>
    </tabs>
  </ribbon>
</customUI>
```

`CorpusRibbon.cs`:
```csharp
using System;
using System.IO;
using System.Reflection;
using System.Windows.Forms;
using MppCorpus.Core;
using Office = Microsoft.Office.Core;

namespace MppCorpus.AddIn
{
    [System.Runtime.InteropServices.ComVisible(true)]
    public class CorpusRibbon : Office.IRibbonExtensibility
    {
        public string GetCustomUI(string ribbonID)
        {
            using (var s = Assembly.GetExecutingAssembly().GetManifestResourceStream("MppCorpus.AddIn.CorpusRibbon.xml"))
            using (var r = new StreamReader(s))
                return r.ReadToEnd();
        }

        public void Ribbon_Load(Office.IRibbonUI ribbonUI) { }

        public void OnGenerate(Office.IRibbonControl control)
        {
            using (var dlg = new FolderBrowserDialog { Description = "Output folder — snapshots/ will be created inside" })
            {
                if (dlg.ShowDialog() != DialogResult.OK) return;
                var snapshotsDir = Path.Combine(dlg.SelectedPath, "snapshots");
                var manifestPath = Path.Combine(dlg.SelectedPath, "manifest.json");
                using (var form = new ProgressForm())
                {
                    form.Show();
                    try
                    {
                        CorpusBuilder.Run(Globals.ThisAddIn.Application, snapshotsDir, manifestPath,
                            report: msg => form.Report(msg),
                            cancelled: () => form.CancelRequested);
                        MessageBox.Show("Corpus generated: " + snapshotsDir, "mpp-corpus");
                    }
                    catch (Exception ex)
                    {
                        MessageBox.Show(ex.Message, "mpp-corpus generation FAILED",
                            MessageBoxButtons.OK, MessageBoxIcon.Error);
                    }
                }
            }
        }
    }
}
```

`ProgressForm.cs` (runs on the UI thread; `Report` pumps messages so Cancel stays clickable — no worker thread, COM stays on the STA):
```csharp
using System.Windows.Forms;

namespace MppCorpus.AddIn
{
    public sealed class ProgressForm : Form
    {
        private readonly ListBox _log = new ListBox { Dock = DockStyle.Fill };
        private readonly Button _cancel = new Button { Text = "Abort", Dock = DockStyle.Bottom };
        public bool CancelRequested { get; private set; }

        public ProgressForm()
        {
            Text = "Generating .mpp corpus";
            Width = 480; Height = 360;
            Controls.Add(_log); Controls.Add(_cancel);
            _cancel.Click += (s, e) => CancelRequested = true;
        }

        public void Report(string message)
        {
            _log.Items.Add(message);
            _log.TopIndex = _log.Items.Count - 1;
            Application.DoEvents(); // keep the form live between COM calls
        }
    }
}
```

`ThisAddIn.cs` — add the ribbon override inside the template's `ThisAddIn` partial class:
```csharp
protected override Microsoft.Office.Core.IRibbonExtensibility CreateRibbonExtensibilityObject()
{
    return new CorpusRibbon();
}
```

- [ ] **Step 3: Build and load in MS Project**

Build from VS (F5) once — VSTO's debug registration (`|vstolocal`) registers the add-in for the current user and MS Project starts; accept the trust prompt (**Install**) if shown. Expected: MS Project shows a **Corpus** tab.
(For a non-VS build later: `& "C:\Program Files\Microsoft Visual Studio\2022\Community\MSBuild\Current\Bin\MSBuild.exe" addin\MppCorpus.sln /p:Configuration=Release` — VSTO registration still comes from the F5/VS side or a `register.ps1` writing `HKCU:\Software\Microsoft\Office\MS Project\Addins\MppCorpus.AddIn` with `LoadBehavior=3` and `Manifest=file:///...\MppCorpus.AddIn.vsto|vstolocal`.)

- [ ] **Step 4: Click-test with a small run**

Temporarily this is a full run — the button has no `--through`; that's fine, click **Generate corpus**, pick `%TEMP%\mpp-ribbon`, watch the progress list walk the 46 steps, and verify `snapshots\46-progress.mpp` exists afterwards. Then test **Abort** early in a second run and confirm generation stops with the cancellation error and partial snapshots remain.

- [ ] **Step 5: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "addin: VSTO ribbon (Corpus > Generate corpus) + progress/abort dialog"
```

---

### Task 14: Generate the real corpus + full verification (spec §Verification)

**Files:**
- Create: `snapshots/*.mpp`, `snapshots/*.xml` (92 files), `manifest.json` (repo root)

- [ ] **Step 1: Generate into the repo** via the ribbon (pick `C:\Users\boris_kudriashov\Source\mpp-corpus` as the output folder — `snapshots/` and `manifest.json` land per repo layout). Runner fallback: `MppCorpus.Runner.exe --out C:\Users\boris_kudriashov\Source\mpp-corpus\snapshots`.

- [ ] **Step 2: mppread verification** (docxy checkout, usual vcvars env)

Run (from `C:\Users\boris_kudriashov\Source\docxy`):
```powershell
cargo run -p mppread --example streams   -- ..\mpp-corpus\snapshots\46-progress.mpp
cargo run -p mppread --example inspect   -- ..\mpp-corpus\snapshots\46-progress.mpp
cargo run -p mppread --example tasknames -- ..\mpp-corpus\snapshots\46-progress.mpp
```
Expected: container + storage tree + metadata parse on all three. Task-name decode may fail (newest-generation MPP is a known gap — record what happens; that record is the starting point for the cloud session's decode work). Repeat `streams` on `01-empty.mpp` and `24-link-fs.mpp`.

- [ ] **Step 3: Adjacent-diff sanity check** — prove the corpus's core promise on one pair

Run:
```powershell
cargo run -p mppread --example inspect -- ..\mpp-corpus\snapshots\27-link-sf.mpp  > "$env:TEMP\27.tree"
cargo run -p mppread --example inspect -- ..\mpp-corpus\snapshots\28-link-lag.mpp > "$env:TEMP\28.tree"
git diff --no-index "$env:TEMP\27.tree" "$env:TEMP\28.tree"
```
Expected: a small, readable stream-level diff — the link/lag-bearing streams change, metadata streams show only timestamp noise.

- [ ] **Step 4: Oracle round-trip in yppxy**

Run: `cargo run -p yppxy -- ..\mpp-corpus\snapshots\46-progress.xml`
Expected: yppxy opens the MSPDI file with 40+ tasks, the WBS tree, and links visible. Quit with `q`.

- [ ] **Step 5: Commit the corpus**

```powershell
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" add -A
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" commit -m "corpus: full 46-step snapshot set + manifest (Project 16.0)"
git -C "C:\Users\boris_kudriashov\Source\mpp-corpus" push
```

---

### Task 15: Publish release v0.1.0

- [ ] **Step 1: Zip and release**

Run:
```powershell
$repo = "C:\Users\boris_kudriashov\Source\mpp-corpus"
Compress-Archive -Path "$repo\snapshots", "$repo\manifest.json" -DestinationPath "$repo\mpp-corpus-snapshots-v0.1.0.zip" -Force
gh release create v0.1.0 "$repo\mpp-corpus-snapshots-v0.1.0.zip" --repo yeroo/mpp-corpus --title "Corpus v0.1.0 (46 steps, Project 16.0)" --notes "Cumulative 46-step .mpp + MSPDI snapshot corpus. See manifest.json for per-step expected values."
Remove-Item "$repo\mpp-corpus-snapshots-v0.1.0.zip"
```

- [ ] **Step 2: Verify the asset downloads with auth**

Run:
```powershell
gh release download v0.1.0 --repo yeroo/mpp-corpus --pattern "*.zip" --dir "$env:TEMP\corpus-dl" --clobber
Get-ChildItem "$env:TEMP\corpus-dl"
```
Expected: the zip downloads (auth via gh); contains `snapshots/` + `manifest.json`.

---

### Task 16: docxy — fetch scripts + README pointer

**Files:**
- Create: `corpus/tools/fetch-mpp-corpus.sh`
- Create: `corpus/tools/fetch-mpp-corpus.ps1`
- Modify: `corpus/mpp/README.md` (new section near the top, after the intro)

(Paths relative to `C:\Users\boris_kudriashov\Source\docxy`; branch `claude/yppxy-project`.)

- [ ] **Step 1: Write the fetch scripts**

`corpus/tools/fetch-mpp-corpus.sh`:
```bash
#!/usr/bin/env bash
# Fetch the generated .mpp corpus (private repo — needs gh auth or GITHUB_TOKEN).
set -euo pipefail
cd "$(dirname "$0")/../mpp"
gh release download --repo yeroo/mpp-corpus --pattern 'mpp-corpus-snapshots-*.zip' --clobber
unzip -o mpp-corpus-snapshots-*.zip
rm -f mpp-corpus-snapshots-*.zip
echo "corpus fetched into $(pwd)/snapshots"
```

`corpus/tools/fetch-mpp-corpus.ps1`:
```powershell
# Fetch the generated .mpp corpus (private repo - needs gh auth or GITHUB_TOKEN).
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\mpp")
gh release download --repo yeroo/mpp-corpus --pattern "mpp-corpus-snapshots-*.zip" --clobber
Get-ChildItem "mpp-corpus-snapshots-*.zip" | ForEach-Object {
    Expand-Archive $_.FullName -DestinationPath . -Force
    Remove-Item $_.FullName
}
Write-Host "corpus fetched into $(Get-Location)\snapshots"
```

- [ ] **Step 2: Add the README section** (insert into `corpus/mpp/README.md` after the intro paragraph)

```markdown
## The generated corpus (preferred)

There is now a **self-generated corpus**: yeroo/mpp-corpus (private) builds one
project cumulatively in real MS Project — one feature per step, 46 steps — and
saves a `NN-slug.mpp` + `NN-slug.xml` (MSPDI oracle) pair after every step, so
adjacent snapshots differ by exactly one feature and every binary has a
known-good answer beside it. `manifest.json` pins per-step expected values.

Fetch it into this folder (needs `gh` auth with access to the private repo):

    corpus/tools/fetch-mpp-corpus.sh      # or .ps1 on Windows

The snapshots land in `corpus/mpp/snapshots/` (gitignored like everything else
here). Being the *newest* MPP generation, they are also the test material for
that decode gap below.
```

- [ ] **Step 3: Verify the fetch script end-to-end**

Run (from docxy root, Git Bash): `bash corpus/tools/fetch-mpp-corpus.sh && ls corpus/mpp/snapshots | head -4`
Expected: `01-empty.mpp`, `01-empty.xml`, …

- [ ] **Step 4: Commit**

```powershell
git -C "C:\Users\boris_kudriashov\Source\docxy" add corpus/tools/fetch-mpp-corpus.sh corpus/tools/fetch-mpp-corpus.ps1 corpus/mpp/README.md
git -C "C:\Users\boris_kudriashov\Source\docxy" commit -m "corpus: fetch scripts + README pointer for the generated private .mpp corpus"
```

---

## Notes for the executor

- **PIA member spellings:** the step bodies are written against the MS Project object model as documented for VBA; if a property/enum member differs in the PIA (`Task.Calendar`, `PjWorkContour` members, `BaselineSave` named args), the compiler will say so — fix to the PIA's actual member. The `Changed` string on each step pins the *intent*; never silently drop a feature to make code compile.
- **Modal dialogs:** if a run hangs, MS Project is showing a dialog `DisplayAlerts=false` didn't cover. Bring the window to front, read it, and handle that specific case in code (never by clicking through manually and calling it done).
- **Interactive checkpoints:** Task 1 Step 2 (admin installer), Task 13 Steps 1/3/4 (VS template scaffold, trust prompt, ribbon click-test) need the user at the desktop. Batch questions accordingly.
- **en-US assumption:** `"3ed"`, `"50/h"` are English unit strings. If Project is installed with a different UI language, these throw at their step — replace with locale-appropriate strings and note it in the manifest's `Changed`.
