; Installeur Windows de ki-chat (Inno Setup 6).
;
;   iscc installer\ki-chat.iss /DAppVersion=0.1.0
;
; Ce fichier doit garder sa marque d'ordre des octets (BOM UTF-8) : sans
; elle, Inno le relit dans la page de code ANSI du système et les accents des
; libellés arrivent en charabia dans l'assistant.
;
; Deux partis pris qui se tiennent l'un l'autre :
;
; 1. Installation dans le profil de l'utilisateur, pas dans « Program Files ».
;    Aucune élévation n'est demandée — ni à l'installation, ni ensuite. C'est
;    ce qui permet à l'application de se mettre à jour toute seule : elle a le
;    droit d'écrire dans son propre dossier. Posée dans « Program Files »,
;    chaque mise à jour réclamerait un UAC, donc n'aurait jamais lieu.
;
; 2. Rien à installer à côté. L'exécutable est lié à la bibliothèque C
;    statiquement : il ne réclame que des DLL livrées avec Windows. Pas de
;    « Visual C++ Redistributable » à pousser, pas de composant qui manque
;    chez l'un et pas chez l'autre.

#define AppName "ki-chat"
#define AppPublisher "ki-chat"
#define AppUrl "https://github.com/Redik123/ki-chat"

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
; Chemin du binaire, surchargeable : la compilation croisée le range sous le
; nom de la cible plutôt que directement dans target\release.
#ifndef Binary
  #define Binary "..\target\x86_64-pc-windows-msvc\release\ki-chat.exe"
#endif

; L'icône est rendue par build.rs ; le workflow la dépose ici avant l'appel.
; Absente, Inno met la sienne : ça ne vaut pas un échec de compilation.
#define IconFile AddBackslash(SourcePath) + "ki-chat.ico"

[Setup]
; Identifie le produit d'une version à l'autre : c'est par lui qu'une
; installation existante est reconnue et remplacée au lieu d'être doublée.
; Il ne doit jamais changer.
AppId={{B0EBE6B8-F719-4EC3-92FA-5A733348026C}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases

DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto

; « lowest » : pas de UAC, et {autopf} désigne alors
; %LOCALAPPDATA%\Programs — un dossier où l'application peut se réécrire.
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0

OutputDir=Output
OutputBaseFilename={#AppName}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\{#AppName}.exe
; Sans ça, « Applications installées » affiche « ki-chat version 0.1.0 » —
; un numéro que les mises à jour automatiques rendent faux dès la première,
; puisqu'elles remplacent le binaire sans repasser par l'installeur.
UninstallDisplayName={#AppName}
#if FileExists(IconFile)
SetupIconFile={#IconFile}
#endif

; Une installation par-dessus une version qui tourne écraserait un fichier
; verrouillé : Restart Manager repère l'instance et la ferme d'abord.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "desktopicon"; Description: "Créer un raccourci sur le Bureau"; GroupDescription: "Raccourcis :"

[Files]
Source: "{#Binary}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppName}.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppName}.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppName}.exe"; Description: "Lancer {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Résidus possibles d'une mise à jour interrompue : l'ancien binaire écarté,
; ou un téléchargement resté en plan.
Type: files; Name: "{app}\{#AppName}.old"
Type: files; Name: "{app}\{#AppName}.new"
