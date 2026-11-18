; Tsubame installer (Inno Setup 6.3+).
;
; Build (tools\release.ps1 does this for you):
;   iscc /DAppVersion=0.1.0 /DSourceDir=<dir holding tsubame.exe [+ tsubame-plughost.exe]> /DOutDir=<out dir> installer\tsubame.iss
; Optional defines:
;   /DNumericVersion=0.1.0.0   4-part numeric version for the exe version resource
;   /DSignTool                 sign the installer AND the uninstaller; needs an iscc
;                              "/Stsubame=<signtool command with $f>" argument
;
; What this installer does:   copies files, Start menu shortcut, optional desktop shortcut,
;                             the HKCU Add/Remove Programs entry (written by Inno itself).
; What it never does:         register a default folder handler, add a shell verb or context
;                             menu entry (the uninstaller only REMOVES one: tsubame-handler.exe
;                             --remove), add a startup/Run entry, create a scheduled task or
;                             service, or touch the user's data (%LOCALAPPDATA%\Tsubame).
;                             Those are opt-in features of the app itself.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef SourceDir
  #error Pass /DSourceDir=<folder with the built exes>
#endif
#ifndef OutDir
  #define OutDir "..\dist"
#endif

[Setup]
; Fixed forever: Inno uses it to find the previous install (upgrade in place, one uninstall entry).
AppId={{B3F1A7C4-5D2E-4C8A-9F0B-7A6E1D3C2B48}
AppName=Tsubame
AppVersion={#AppVersion}
AppVerName=Tsubame {#AppVersion}
AppPublisher=Tsubame
AppPublisherURL=https://github.com/watdafawx/tsubame
AppSupportURL=https://github.com/watdafawx/tsubame/issues
AppUpdatesURL=https://github.com/watdafawx/tsubame/releases
#ifdef NumericVersion
VersionInfoVersion={#NumericVersion}
#endif
VersionInfoDescription=Tsubame setup
; Per-user by default (no UAC). The dialog offers "all users", which then elevates once.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; {autopf} = %LOCALAPPDATA%\Programs when per-user, %ProgramFiles% when all-users.
DefaultDirName={autopf}\Tsubame
DefaultGroupName=Tsubame
DisableProgramGroupPage=yes
DisableDirPage=auto
UsePreviousAppDir=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=tsubame-{#AppVersion}-x64-setup
; Installer's own format; the exes are never packed with UPX or similar.
Compression=lzma2/ultra64
SolidCompression=no
WizardStyle=modern
; The HKCU (or HKLM for all-users) Add/Remove Programs entry and unins000.exe.
Uninstallable=yes
UninstallDisplayName=Tsubame
UninstallDisplayIcon={app}\tsubame.exe
CreateUninstallRegKey=yes
; Restart Manager: closes a running Tsubame before replacing it, and asks to restart it.
CloseApplications=yes
RestartApplications=no
#ifdef SignTool
SignTool=tsubame
SignedUninstaller=yes
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Types]
Name: "compact"; Description: "Tsubame only"
Name: "full"; Description: "Tsubame and the plugin host"
Name: "custom"; Description: "Custom"; Flags: iscustom

[Components]
Name: "main"; Description: "Tsubame"; Types: compact full custom; Flags: fixed
; Optional, off by default (not part of the "compact" type that is preselected).
Name: "plughost"; Description: "Plugin host (tsubame-plughost.exe), needed only for WebAssembly plugins"; Types: full

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\tsubame.exe"; DestDir: "{app}"; Components: main; Flags: ignoreversion
; The fail-safe folder handler (docs\default-file-manager.md): installed always, used only when the
; user switches "Default file manager" on in Settings. tools\release.ps1 insists on it for a real release.
Source: "{#SourceDir}\tsubame-handler.exe"; DestDir: "{app}"; Components: main; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\tsubame-plughost.exe"; DestDir: "{app}"; Components: plughost; Flags: ignoreversion skipifsourcedoesntexist
; Docs are optional files in the release folder.
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Components: main; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SourceDir}\README.txt"; DestDir: "{app}"; Components: main; Flags: ignoreversion skipifsourcedoesntexist
; NOTE: portable.txt is never installed. Its presence makes Tsubame keep its data next to the exe.

[Icons]
Name: "{autoprograms}\Tsubame"; Filename: "{app}\tsubame.exe"; Comment: "Tsubame file browser"
Name: "{autodesktop}\Tsubame"; Filename: "{app}\tsubame.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\tsubame.exe"; Description: "Start Tsubame"; Flags: nowait postinstall skipifsilent unchecked

[UninstallRun]
; Put File Explorer back BEFORE any file is deleted (docs\default-file-manager.md, "Uninstall"):
; removes Tsubame's folder verb from HKCU if the user switched it on, does nothing otherwise.
; Never run at install time: the installer itself registers nothing.
Filename: "{app}\tsubame-handler.exe"; Parameters: "--remove"; Flags: runhidden skipifdoesntexist; RunOnceId: "RestoreExplorer"

[InstallDelete]
; Leftovers of the in-app updater from before an upgrade-by-installer.
Type: files; Name: "{app}\*.old"
Type: files; Name: "{app}\*.new"
Type: files; Name: "{app}\*.bad"

[UninstallDelete]
; Files the in-app updater creates next to the exes (renamed previous version, half-swapped copies).
; Only these patterns, only inside {app}. The user's data in %LOCALAPPDATA%\Tsubame is never touched.
Type: files; Name: "{app}\tsubame.exe.old"
Type: files; Name: "{app}\tsubame.exe.new"
Type: files; Name: "{app}\tsubame.exe.bad"
Type: files; Name: "{app}\tsubame-plughost.exe.old"
Type: files; Name: "{app}\tsubame-plughost.exe.new"
Type: files; Name: "{app}\tsubame-plughost.exe.bad"
Type: files; Name: "{app}\tsubame-handler.exe.old"
Type: files; Name: "{app}\tsubame-handler.exe.new"
Type: files; Name: "{app}\tsubame-handler.exe.bad"
; The folder itself goes only if empty (Inno's default); nothing recursive.
