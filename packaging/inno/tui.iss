; docxy terminal apps (docxy + xlsxy) — Inno Setup installer.
;
; Per-user (no UAC): installs the console editors and adds their folder to the
; user PATH so `docxy file.docx` / `xlsxy file.xlsx` work from any shell.
; Compile:  iscc /DAppVersion=%VER% /DSrcDir=<staging> packaging\inno\tui.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SrcDir
  #define SrcDir "..\..\target\release"
#endif
#define Publisher "yeroo"

[Setup]
AppId={{C4E9B2A3-5D7F-4B0C-8F32-8A1E3D6B9F22}
AppName=docxy CLI
AppVersion={#AppVersion}
AppPublisher={#Publisher}
AppPublisherURL=https://github.com/yeroo/docxy
DefaultDirName={autopf}\docxy-cli
DisableProgramGroupPage=yes
DisableDirPage=auto
UninstallDisplayName=docxy CLI (docxy + xlsxy)
OutputBaseFilename=docxy-tui-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
VersionInfoVersion={#AppVersion}
VersionInfoDescription=docxy terminal editors installer

[Files]
Source: "{#SrcDir}\docxy.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\xlsxy.exe"; DestDir: "{app}"; Flags: ignoreversion

[Tasks]
Name: "addtopath"; Description: "Add docxy + xlsxy to my PATH (recommended)"; GroupDescription: "Command line:"

[Registry]
; Append {app} to the user PATH, once, only when it isn't already there.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
    Flags: preservestringtype; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  // case-insensitive, boundary-aware check so we never add a duplicate
  Result := Pos(';' + Lowercase(Dir) + ';', ';' + Lowercase(OrigPath) + ';') = 0;
end;

procedure RemoveFromPath(Dir: string);
var
  OrigPath, NewPath: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
    exit;
  NewPath := ';' + OrigPath + ';';
  // strip the app dir (any case), then tidy the wrapping separators below
  repeat
    P := Pos(';' + Lowercase(Dir) + ';', Lowercase(NewPath));
    if P > 0 then
      Delete(NewPath, P, Length(Dir) + 1);
  until P = 0;
  // drop the sentinel separators we added
  if (Length(NewPath) > 0) and (NewPath[1] = ';') then Delete(NewPath, 1, 1);
  if (Length(NewPath) > 0) and (NewPath[Length(NewPath)] = ';') then Delete(NewPath, Length(NewPath), 1);
  RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', NewPath);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
