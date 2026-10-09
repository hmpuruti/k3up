; Builds the K3 Up setup program. Paths are relative to this file's folder:
;   makensis -DVERSION=0.1.0 -DBINARIES=../../target/release packaging/windows/installer.nsi
Unicode true
!include "MUI2.nsh"
!include "x64.nsh"

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef BINARIES
  !define BINARIES "../../target/release"
!endif
!ifndef OUTFILE
  !define OUTFILE "k3up-setup-x86_64.exe"
!endif

!define PRODUCT "K3 Up"
!define PUBLISHER "K3"
!define APP "k3up-desktop.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\K3Up"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
; The agent runs as a Windows service for every user, which needs administrator rights.
RequestExecutionLevel admin
; Always Program Files: the service runs these programs as SYSTEM, so they must sit where
; only administrators can write.
InstallDir "$PROGRAMFILES64\K3 Up"
SetCompressor /SOLID lzma
BrandingText "${PRODUCT} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "FileDescription" "${PRODUCT} Setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright (c) K3. MIT License."

!define MUI_ICON "k3up.ico"
!define MUI_UNICON "k3up.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP}"
!define MUI_FINISHPAGE_RUN_TEXT "Open K3 Up"
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Create a desktop shortcut"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION DesktopShortcut

!insertmacro MUI_PAGE_LICENSE "../../LICENSE"
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Open desktop apps lock their files. The service is stopped by `k3up agent install` and
; `agent uninstall`, which wait for it to stop its workloads.
!macro StopDesktop
  nsExec::Exec 'taskkill /F /IM ${APP}'
  Pop $0
  Sleep 500
!macroend

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "K3 Up requires 64-bit Windows." /SD IDOK
    Abort
  ${EndIf}
  UserInfo::GetAccountType
  Pop $0
  ${If} $0 != "admin"
    MessageBox MB_ICONSTOP "K3 Up installs a Windows service. Run setup as an administrator." /SD IDOK
    Abort
  ${EndIf}
  SetRegView 64
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd

Function DesktopShortcut
  CreateShortcut "$DESKTOP\K3 Up.lnk" "$INSTDIR\${APP}" "" "$INSTDIR\k3up.ico"
FunctionEnd

; Earlier versions installed for one user under %LOCALAPPDATA% and started the agent at
; login. Remove that copy, keeping its data, so two agents do not run the same programs.
Function RemovePerUserInstall
  SetShellVarContext current
  DeleteRegValue HKCU "${RUN_KEY}" "K3 Up"
  StrCpy $1 "$LOCALAPPDATA\Programs\K3 Up"
  ${If} ${FileExists} "$1\Uninstall.exe"
    ExecWait '"$1\Uninstall.exe" /S _?=$1'
    Delete "$1\Uninstall.exe"
    RMDir "$1"
  ${EndIf}
  ; The per-user agent may have been started from a portable copy instead.
  nsExec::Exec 'taskkill /F /IM k3up-agent.exe /FI "USERNAME ne NT AUTHORITY\SYSTEM"'
  Pop $0
  SetShellVarContext all
FunctionEnd

Section "K3 Up"
  SectionIn RO
  SetShellVarContext all
  !insertmacro StopDesktop
  Call RemovePerUserInstall

  ; `k3up agent install` copies the programs from its own folder into Program Files, then
  ; installs or updates the service, starts it and adds Program Files\K3 Up to the system
  ; PATH. An existing service is stopped first and keeps its workloads.
  InitPluginsDir
  SetOutPath "$PLUGINSDIR\bin"
  File "${BINARIES}/${APP}"
  File "${BINARIES}/k3up-agent.exe"
  File "${BINARIES}/k3up.exe"
  ; k3up.exe is a console program; nsExec runs it without a console window flashing.
  nsExec::ExecToLog '"$PLUGINSDIR\bin\k3up.exe" agent install'
  Pop $0
  ${If} $0 != 0
    SetOutPath "$TEMP"
    RMDir /r "$PLUGINSDIR\bin"
    MessageBox MB_ICONSTOP "The K3 Up service could not be installed. See Show details for the reason." /SD IDOK
    Abort
  ${EndIf}
  SetOutPath "$INSTDIR"
  RMDir /r "$PLUGINSDIR\bin"

  File "k3up.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\K3 Up.lnk" "$INSTDIR\${APP}" "" "$INSTDIR\k3up.ico"

  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\k3up.ico"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
  SectionGetSize 0 $0
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "EstimatedSize" $0
SectionEnd

Section "Uninstall"
  SetShellVarContext all
  !insertmacro StopDesktop
  ; Stops every workload, waits for the service to exit, and removes it.
  nsExec::ExecToLog '"$INSTDIR\k3up.exe" agent uninstall'
  Pop $0
  nsExec::ExecToLog '"$INSTDIR\k3up.exe" path remove --system "$INSTDIR"'
  Pop $0
  Delete "$SMPROGRAMS\K3 Up.lnk"
  Delete "$DESKTOP\K3 Up.lnk"
  Delete "$INSTDIR\${APP}"
  Delete "$INSTDIR\k3up-agent.exe"
  Delete "$INSTDIR\k3up.exe"
  Delete "$INSTDIR\k3up.ico"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  ReadEnvStr $1 "ProgramData"
  MessageBox MB_YESNO|MB_ICONQUESTION "Also delete the K3 Up workloads, logs and history of every user?" /SD IDNO IDNO keep
    RMDir /r "$1\K3 Up"
  keep:
SectionEnd
