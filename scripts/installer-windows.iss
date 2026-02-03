; Inno Setup Script for Chatty
; Build with: iscc installer-windows.iss

#define MyAppName "Chatty"
#define MyAppPublisher "Marcel Boersma"
#define MyAppURL "https://github.com/boersmamarcel/chatty2"
#define MyAppExeName "chatty.exe"

; Version is passed from CI: iscc /DMyAppVersion=0.1.0 installer-windows.iss
#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

[Setup]
AppId={{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
; Output settings
OutputDir=.
; Use simplified naming convention for auto-updater: chatty-windows-x86_64.exe
OutputBaseFilename=chatty-windows-x86_64
; Compression
Compression=lzma2/ultra64
SolidCompression=yes
; Modern Windows look
WizardStyle=modern
; Require admin for Program Files install
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=dialog
; Uninstaller
UninstallDisplayIcon={app}\{#MyAppExeName}
; Architecture
ArchitecturesInstallIn64BitMode=x64compatible
; Icon
#ifexist "..\assets\app_icon\icon.ico"
SetupIconFile=..\assets\app_icon\icon.ico
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
; Themes
Source: "themes\*.json"; DestDir: "{app}\themes"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Code]
// Close running instances before install
function InitializeSetup(): Boolean;
var
  ResultCode: Integer;
begin
  Result := True;
  // Try to close running instances gracefully
  if CheckForMutexes('ChattyAppMutex') then
  begin
    if MsgBox('Chatty is currently running. Setup will close it to continue.', mbConfirmation, MB_OKCANCEL) = IDOK then
    begin
      Exec('taskkill', '/F /IM chatty.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
      Sleep(1000);
    end
    else
      Result := False;
  end;
end;
