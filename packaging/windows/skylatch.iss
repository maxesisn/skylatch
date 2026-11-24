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
DefaultDirName={localappdata}\Programs\Skylatch
DefaultGroupName=Skylatch
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
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
Name: "agentlogin"; Description: "Start Skylatch host agent automatically at login"; Flags: unchecked

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
