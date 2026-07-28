; docxy Office COM shims (Excel + Word) — Inno Setup installer.
;
; Bundles the office-shims payload (built by tools\package-shims.ps1) and runs
; its per-user install.ps1 to register the shims in HKCU only — so apps that
; automate Excel/Word over COM keep working on a machine with no Microsoft
; Office. No admin required; nothing machine-wide (HKLM) is touched.
;
; Compile:  iscc /DAppVersion=%VER% /DSrcDir=<office-shims folder> packaging\inno\comshim.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SrcDir
  #define SrcDir "..\..\dist\office-shims"
#endif
#define Publisher "yeroo"

[Setup]
AppId={{D5F0C3B4-6E80-4C1D-9043-9B2F4E7C0033}
AppName=docxy Office COM shims
AppVersion={#AppVersion}
AppPublisher={#Publisher}
AppPublisherURL=https://github.com/yeroo/docxy
DefaultDirName={autopf}\docxy-office-shims
DisableProgramGroupPage=yes
DisableDirPage=auto
UninstallDisplayName=docxy Office COM shims (Excel + Word)
OutputBaseFilename=docxy-comshim-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
VersionInfoVersion={#AppVersion}
VersionInfoDescription=docxy Office COM shims installer

[Files]
; The whole staged payload: xl/word shim exe+dll, mktypelib helpers, .tlb type
; libraries, and the install/uninstall/selftest scripts + README.
Source: "{#SrcDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Run]
; Register the shims (HKCU) after files land. install.ps1 guards against
; stomping an existing mapping, so a machine with real Office is left alone.
Filename: "powershell.exe"; \
    Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\install.ps1"""; \
    WorkingDir: "{app}"; StatusMsg: "Registering the Excel + Word COM shims (per-user)..."; \
    Flags: runhidden waituntilterminated

[UninstallRun]
; Unregister before the files are removed.
Filename: "powershell.exe"; \
    Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\uninstall.ps1"""; \
    WorkingDir: "{app}"; RunOnceId: "UnregisterShims"; \
    Flags: runhidden waituntilterminated
