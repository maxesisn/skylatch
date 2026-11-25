; SPDX-License-Identifier: Apache-2.0
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\build\windows"
#endif
#ifndef OutputName
  #define OutputName "Skylatch-Setup-x64"
#endif

[Setup]
AppId=io.github.maxesisn.skylatch
AppName=Skylatch
AppVersion={#AppVersion}
AppPublisher=Skylatch
AppPublisherURL=https://github.com/maxesisn/skylatch
; Per-user installs land in %LOCALAPPDATA%\Programs\Skylatch; an install for all users goes to
; Program Files and may register the LocalSystem host agent service.
DefaultDirName={autopf}\Skylatch
DefaultGroupName=Skylatch
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile=..\..\assets\icon\skylatch.ico
UninstallDisplayIcon={app}\skylatch.ico
InfoBeforeFile=licensing.txt
OutputBaseFilename={#OutputName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked
Name: "agentlogin"; Description: "Start Skylatch host agent automatically at login"; Flags: unchecked; Check: not IsAdminInstallMode
Name: "agentservice"; Description: "Run Skylatch host agent as a Windows service (works on UAC prompts and the lock screen)"; Check: IsAdminInstallMode

[Files]
Source: "{#BinDir}\skylatch-ui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\skylatch-client.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\skylatch_client_core.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\skylatch-agent.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\skylatch-host-engine.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\runtime\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\..\assets\icon\skylatch.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-GPL-3.0"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\NOTICE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Skylatch"; Filename: "{app}\skylatch-ui.exe"; WorkingDir: "{app}"; IconFilename: "{app}\skylatch.ico"
Name: "{userdesktop}\Skylatch"; Filename: "{app}\skylatch-ui.exe"; WorkingDir: "{app}"; IconFilename: "{app}\skylatch.ico"; Tasks: desktopicon

[Registry]
; Enroll as the same Windows user. The agent reads its saved configuration at startup.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "SkylatchAgent"; ValueData: """{app}\skylatch-agent.exe"" --engine-bin ""{app}\skylatch-host-engine.exe"""; Flags: uninsdeletevalue; Tasks: agentlogin
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "SkylatchAgent"; Flags: deletevalue; Tasks: not agentlogin

[Run]
Filename: "{app}\skylatch-agent.exe"; Parameters: "service install --start"; StatusMsg: "Starting the Skylatch host agent service..."; Flags: runhidden waituntilterminated; Tasks: agentservice
Filename: "{app}\skylatch-agent.exe"; Parameters: "service uninstall"; Flags: runhidden waituntilterminated; Tasks: not agentservice; Check: IsAdminInstallMode
; Launch the UI after an interactive install (finish-page checkbox, checked by default) and after a
; silent install or self-update (which shows no finish page). runasoriginaluser starts it with the
; pre-UAC credentials of the user who launched Setup, so an all-users install does not run the UI
; elevated; Setup documents it as valid in any [Run] entry and a no-op when not elevated.
Filename: "{app}\skylatch-ui.exe"; Description: "Launch Skylatch"; Flags: nowait postinstall runasoriginaluser skipifsilent
Filename: "{app}\skylatch-ui.exe"; Flags: nowait runasoriginaluser skipifnotsilent

[UninstallRun]
Filename: "{app}\skylatch-agent.exe"; Parameters: "service uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "SkylatchAgentService"; Check: IsAdminInstallMode

[Code]
// The service keeps skylatch-agent.exe and the engine it started open; stop it before files are
// replaced. `net stop` returns once the service has stopped, and fails harmlessly when it is not
// installed. The tray UI ignores WM_CLOSE and the agent has no window, so Inno Setup's own closing of
// applications cannot end them; kill what is left.
procedure KillImage(Image, Extra: String);
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F ' + Extra + '/IM ' + Image, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  if IsAdminInstallMode then
    Exec(ExpandConstant('{sys}\net.exe'), 'stop SkylatchAgent', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  KillImage('skylatch-ui.exe', '/T ');
  KillImage('skylatch-agent.exe', '');
  KillImage('skylatch-host-engine.exe', '');
  Result := '';
end;
