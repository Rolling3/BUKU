; BUKU installer (Inno Setup 6).
; Build:  powershell -ExecutionPolicy Bypass -File installer\build.ps1
;         (or ISCC.exe installer\buku.iss after cargo build --release, unsigned)
; build.ps1 passes /DSIGN and a "buku" sign tool when a certificate is set up;
; see docs/SIGNING.md.

#define AppName "BUKU"
#define AppVersion "0.1.0"
#define AppExe "buku.exe"

[Setup]
AppId={{6F3B2C1E-9A4D-4E57-B8C2-5D1E0A7F4B93}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppName}
AppVerName={#AppName} {#AppVersion}
; Per-user install: no administrator rights needed, nothing written outside the profile.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\{#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
SetupIconFile=..\branding\icons\buku.ico
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\target\installer
OutputBaseFilename=BUKU-Setup-{#AppVersion}
LicenseFile=..\LICENSE
; A running BUKU is closed before files are replaced or removed (see [Code]). Everything
; it stores is written to disk as soon as it changes, so closing it loses nothing.
CloseApplications=yes
#ifdef SIGN
SignTool=buku
SignedUninstaller=yes
#endif

[Tasks]
Name: "autostart"; Description: "Start BUKU when I sign in to Windows"

[Files]
#ifdef SIGN
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion sign
#else
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
#endif
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\PRIVACY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"

[Registry]
; The same value BUKU's own "Start with Windows" switch writes, so either can turn it off.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start BUKU now"; Flags: nowait postinstall skipifsilent

[Code]
procedure StopBuku();
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM {#AppExe} /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  // Give Windows a moment to release the file.
  Sleep(500);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopBuku();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopBuku();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    // Also removes the Run value if BUKU's own switch created it.
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
    // Settings, water count, tasks and the shelf list. Files placed on the Shelf
    // are never touched either way.
    DataDir := ExpandConstant('{userappdata}\{#AppName}');
    if DirExists(DataDir) and not UninstallSilent then
      if MsgBox('Also delete your BUKU settings, tasks and shelf list?', mbConfirmation, MB_YESNO) = IDYES then
        DelTree(DataDir, True, True, True);
  end;
end;
