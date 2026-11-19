; SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
; SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>
;
; The Windows installer (Inno Setup 6). Built by .github/workflows/release.yml:
;   ISCC /DAppVersion=1.4.0 /DNumVersion=1.4.0 /DSourceDir=<package folder>
;        /DOutputDir=<folder> /DOutputName=<file name without .exe> bageo.iss
;
; Installs for the signed-in user only (no administrator needed) into
; %LOCALAPPDATA%\Programs\Bageo, with a "Bageo" shortcut on the desktop and in
; the Start menu. The shortcut opens Bageo Status, which starts Bageo in the
; background. Bageo keeps its data in the "data" folder there; removing the
; program leaves that folder alone.
;
; Windows Firewall: Bageo opens its console to the local network
; (http://bageo.local/). Adding a firewall rule needs an administrator, which
; this per-user install does not ask for, so Windows asks once ("Allow
; access") when Bageo first starts: choose Private networks (see the
; troubleshooting guide, "bageo.local does not open").

#ifndef AppVersion
  #error Pass /DAppVersion=...
#endif
#ifndef NumVersion
  #define NumVersion "0.0.0"
#endif

[Setup]
AppId={{6B1E3C52-8A4D-4F7B-9C2E-5D0A7B3F1E84}
AppName=Bageo
AppVersion={#AppVersion}
AppVerName=Bageo {#AppVersion}
AppPublisher=h4lo-void
AppPublisherURL=https://github.com/h4lo-void/bageo-call
VersionInfoVersion={#NumVersion}
DefaultDirName={localappdata}\Programs\Bageo
DisableDirPage=yes
DefaultGroupName=Bageo
DisableProgramGroupPage=yes
DisableWelcomePage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
SetupIconFile=..\icons\icon.ico
UninstallDisplayIcon={app}\bageo-status.exe
UninstallDisplayName=Bageo
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
; Bageo and Bageo Status may be running during an update or removal.
CloseApplications=force
RestartApplications=no
ShowLanguageDialog=no
LanguageDetectionMethod=uilanguage

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
#if FileExists(CompilerPath + "Languages\ChineseTraditional.isl")
Name: "zhtw"; MessagesFile: "compiler:Languages\ChineseTraditional.isl"
#endif
#if FileExists(CompilerPath + "Languages\ChineseSimplified.isl")
Name: "zhcn"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
#endif
Name: "ja"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "ko"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"
Name: "pt"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autodesktop}\Bageo"; Filename: "{app}\bageo-status.exe"; WorkingDir: "{app}"
Name: "{autoprograms}\Bageo"; Filename: "{app}\bageo-status.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\bageo-status.exe"; WorkingDir: "{app}"; Description: "{cm:LaunchProgram,Bageo}"; Flags: nowait postinstall skipifsilent
