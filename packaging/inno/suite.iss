; docxy desktop suite — Inno Setup installer.
;
; Per-user (no UAC): installs suite.exe, a Start-Menu shortcut, and OPTIONAL
; file associations for .docx / .xlsx. Compile in CI with:
;   iscc /DAppVersion=%VER% /DSrcDir=<staging> packaging\inno\suite.iss
; where <staging> holds suite.exe and docxy.ico.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SrcDir
  #define SrcDir "..\..\suite\target\release"
#endif
#ifndef IcoDir
  #define IcoDir "..\..\suite\docxy\assets"
#endif
#define AppName "docxy"
#define AppExe "suite.exe"
#define Publisher "yeroo"

[Setup]
AppId={{B3D8A1F2-4C6E-4A9B-9E21-7F0D2C5A8E11}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#Publisher}
AppPublisherURL=https://github.com/yeroo/docxy
DefaultDirName={autopf}\docxy
DefaultGroupName=docxy
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\docxy.ico
UninstallDisplayName=docxy (desktop suite)
OutputBaseFilename=docxy-suite-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile={#IcoDir}\docxy.ico
VersionInfoVersion={#AppVersion}
VersionInfoDescription=docxy desktop suite installer

[Files]
Source: "{#SrcDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#IcoDir}\docxy.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\docxy"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\docxy.ico"
Name: "{userdesktop}\docxy"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\docxy.ico"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "assocxlsx"; Description: "Add docxy to the 'Open with' list for .xlsx spreadsheets"; GroupDescription: "File associations:"
Name: "assocdocx"; Description: "Add docxy to the 'Open with' list for .docx documents"; GroupDescription: "File associations:"

[Registry]
; A ProgId per file type that opens suite.exe with the file path, registered
; per-user (HKCU) so nothing machine-wide is touched. We add them to the file
; extension's OpenWithProgids (docxy shows under "Open with") rather than seizing
; the default — Windows 10/11 requires the user to confirm the default handler.
;
; --- Spreadsheets (.xlsx) ---
Root: HKCU; Subkey: "Software\Classes\docxy.xlsx"; ValueType: string; ValueData: "Excel Workbook (docxy)"; Flags: uninsdeletekey; Tasks: assocxlsx
Root: HKCU; Subkey: "Software\Classes\docxy.xlsx\DefaultIcon"; ValueType: string; ValueData: "{app}\docxy.ico,0"; Tasks: assocxlsx
Root: HKCU; Subkey: "Software\Classes\docxy.xlsx\shell\open\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: assocxlsx
Root: HKCU; Subkey: "Software\Classes\.xlsx\OpenWithProgids"; ValueType: string; ValueName: "docxy.xlsx"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assocxlsx
; --- Documents (.docx) ---
Root: HKCU; Subkey: "Software\Classes\docxy.docx"; ValueType: string; ValueData: "Word Document (docxy)"; Flags: uninsdeletekey; Tasks: assocdocx
Root: HKCU; Subkey: "Software\Classes\docxy.docx\DefaultIcon"; ValueType: string; ValueData: "{app}\docxy.ico,0"; Tasks: assocdocx
Root: HKCU; Subkey: "Software\Classes\docxy.docx\shell\open\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: assocdocx
Root: HKCU; Subkey: "Software\Classes\.docx\OpenWithProgids"; ValueType: string; ValueName: "docxy.docx"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assocdocx
; --- Register the app so it appears in Settings ▸ Default apps ---
Root: HKCU; Subkey: "Software\docxy\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "docxy"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\docxy\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "docxy desktop document & spreadsheet suite"
Root: HKCU; Subkey: "Software\docxy\Capabilities\FileAssociations"; ValueType: string; ValueName: ".xlsx"; ValueData: "docxy.xlsx"; Tasks: assocxlsx
Root: HKCU; Subkey: "Software\docxy\Capabilities\FileAssociations"; ValueType: string; ValueName: ".docx"; ValueData: "docxy.docx"; Tasks: assocdocx
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "docxy"; ValueData: "Software\docxy\Capabilities"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch docxy now"; Flags: nowait postinstall skipifsilent
