; Inno Setup script for the PDF to Word converter.
; Build: iscc /DSourceDir=<folder with the deployed files> /DAppVersion=0.1.0 installer.iss

#ifndef SourceDir
  #define SourceDir "..\..\dist\pdf2word-windows-x64"
#endif
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{6C1E2D44-7F1B-4E0B-9A63-1D5C0E7A9B21}
AppName=PDF to Word
AppVersion={#AppVersion}
AppPublisher=pdf2word
DefaultDirName={autopf}\PDF to Word
DefaultGroupName=PDF to Word
UninstallDisplayIcon={app}\pdf2word-gui.exe
OutputBaseFilename=pdf2word-setup-{#AppVersion}-x64
OutputDir=..\..\dist
SetupIconFile=..\icons\pdf2word.ico
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
WizardStyle=modern
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "openwith"; Description: "Add ""Convert with PDF to Word"" to the Open with menu of PDF files"; GroupDescription: "Integration:"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\PDF to Word"; Filename: "{app}\pdf2word-gui.exe"
Name: "{group}\Uninstall PDF to Word"; Filename: "{uninstallexe}"
Name: "{autodesktop}\PDF to Word"; Filename: "{app}\pdf2word-gui.exe"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Classes\Applications\pdf2word-gui.exe\shell\open\command"; ValueType: string; ValueData: """{app}\pdf2word-gui.exe"" ""%1"""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\pdf2word-gui.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "PDF to Word"; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.pdf\OpenWithList\pdf2word-gui.exe"; ValueType: string; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith

[Run]
Filename: "{app}\pdf2word-gui.exe"; Description: "{cm:LaunchProgram,PDF to Word}"; Flags: nowait postinstall skipifsilent
