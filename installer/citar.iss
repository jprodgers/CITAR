; Windows installer for CITAR.
;
;   python -m PyInstaller installer/citar.spec --noconfirm
;   ISCC.exe /DAppVersion=0.1.0 installer\citar.iss
;
; Produces installer\output\CITAR-<version>-setup.exe.
;
; The result needs no Python, no command line and no administrator: it installs per-user so that
; somebody on a locked-down work machine can still run it, and so an upgrade does not need an
; elevation prompt. Everything CITAR writes goes to %LOCALAPPDATA%\CITAR and is deliberately left
; behind by the uninstaller — saved games and benchmark results are the person's work, not the
; program's files.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

#define AppName       "CITAR"
#define AppLongName   "CITAR - Civ Inspired Tool for AI Research"
#define AppPublisher  "Jimmie Rodgers"
#define AppURL        "https://github.com/jprodgers/CITAR"
#define AppExeName    "citar-play.exe"
#define CliExeName    "citar.exe"

[Setup]
AppId={{9C1D2A54-5F1B-4A9D-9B5E-7C3F9A2E1D40}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
VersionInfoVersion={#AppVersion}

; Per-user, so no elevation prompt. A user without administrator rights is precisely the person
; most likely to want a downloadable installer instead of pip.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir=output
OutputBaseFilename=CITAR-{#AppVersion}-setup
SetupIconFile=citar.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppLongName}

; LZMA2 on a folder of Python bytecode and DLLs is worth roughly a third of the download.
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

LicenseFile=..\LICENSE
InfoBeforeFile=welcome.txt
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "addtopath"; Description: "Add the &citar command to my PATH"; \
  GroupDescription: "Command line:"; \
  Flags: unchecked

[Files]
; The whole PyInstaller folder. recursesubdirs picks up the ruleset, the web client and the
; migrations, which citar.paths resolves relative to the executable.
Source: "..\dist\CITAR\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Comment: "Play CITAR"
Name: "{group}\CITAR setup"; Filename: "{app}\{#CliExeName}"; Parameters: "setup"; \
  Comment: "Find a model and configure CITAR"
Name: "{group}\Check my CITAR installation"; Filename: "{app}\{#CliExeName}"; Parameters: "doctor"; \
  Comment: "Report what is configured and what is missing"
Name: "{group}\Saved games and settings"; Filename: "{localappdata}\CITAR"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Run]
; Offered, not forced: "run it now" is the right default, but somebody installing from a script or
; onto a machine they are preparing for somebody else should be able to decline.
Filename: "{app}\{#AppExeName}"; Description: "Start {#AppName} now"; \
  Flags: nowait postinstall skipifsilent

[UninstallDelete]
; PyInstaller's own byte-cache, which is created after installation and would otherwise leave the
; program directory behind.
Type: filesandordirs; Name: "{app}\__pycache__"

[Code]
const
  EnvironmentKey = 'Environment';

function NeedsAddPath(Param: string): boolean;
var
  OriginalPath: string;
begin
  if not RegQueryStringValue(HKCU, EnvironmentKey, 'Path', OriginalPath) then
  begin
    Result := True;
    exit;
  end;
  // Semicolons on both sides so that a directory whose name merely contains this one does not
  // count as a match.
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OriginalPath) + ';') = 0;
end;

procedure AddToPath();
var
  OriginalPath: string;
begin
  if not NeedsAddPath(ExpandConstant('{app}')) then exit;
  if not RegQueryStringValue(HKCU, EnvironmentKey, 'Path', OriginalPath) then
    OriginalPath := '';
  if (OriginalPath <> '') and (Copy(OriginalPath, Length(OriginalPath), 1) <> ';') then
    OriginalPath := OriginalPath + ';';
  RegWriteExpandStringValue(HKCU, EnvironmentKey, 'Path', OriginalPath + ExpandConstant('{app}'));
end;

procedure RemoveFromPath();
var
  OriginalPath, Target: string;
  Position: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvironmentKey, 'Path', OriginalPath) then exit;
  Target := ';' + ExpandConstant('{app}');
  Position := Pos(Uppercase(Target), Uppercase(OriginalPath));
  if Position = 0 then
  begin
    // It may be the first entry, with no leading semicolon.
    Target := ExpandConstant('{app}') + ';';
    Position := Pos(Uppercase(Target), Uppercase(OriginalPath));
    if Position = 0 then exit;
  end;
  Delete(OriginalPath, Position, Length(Target));
  RegWriteExpandStringValue(HKCU, EnvironmentKey, 'Path', OriginalPath);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath();
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  StatePath: string;
begin
  if CurUninstallStep <> usPostUninstall then exit;

  RemoveFromPath();

  // The user's games, settings, benchmark runs and reports. Never deleted without being asked:
  // somebody uninstalling to fix a problem, or to move to the pip version, would not expect to
  // lose a multi-day benchmark run.
  StatePath := ExpandConstant('{localappdata}\CITAR');
  if DirExists(StatePath) then
  begin
    if MsgBox('Delete your CITAR saved games, settings and results as well?' + #13#10#13#10
              + StatePath + #13#10#13#10
              + 'Choose No to keep them for a future installation.',
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(StatePath, True, True, True);
  end;
end;
