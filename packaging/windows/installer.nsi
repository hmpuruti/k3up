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
; Installs for the current user, like the agent itself, so no administrator prompt.
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\K3 Up"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
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
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Running programs lock their files. Ending the agent also ends its workloads through their
; Job Objects; the agent starts them again when it next starts.
!macro StopK3Up
  nsExec::Exec 'taskkill /F /IM ${APP}'
  Pop $0
  nsExec::Exec 'taskkill /F /IM k3up-agent.exe'
  Pop $0
  Sleep 500
!macroend

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "K3 Up requires 64-bit Windows." /SD IDOK
    Abort
  ${EndIf}
FunctionEnd

Function DesktopShortcut
  CreateShortcut "$DESKTOP\K3 Up.lnk" "$INSTDIR\${APP}" "" "$INSTDIR\k3up.ico"
FunctionEnd

Section "K3 Up"
  SectionIn RO
  !insertmacro StopK3Up
  SetOutPath "$INSTDIR"
  File "${BINARIES}/${APP}"
  File "${BINARIES}/k3up-agent.exe"
  File "${BINARIES}/k3up.exe"
  File "k3up.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\K3 Up.lnk" "$INSTDIR\${APP}" "" "$INSTDIR\k3up.ico"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\k3up.ico"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  SectionGetSize 0 $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0

  ; Start the agent now and at every login, so workloads run even before the app is opened.
  ExecWait '"$INSTDIR\${APP}" --register-agent'
SectionEnd

Section "Uninstall"
  !insertmacro StopK3Up
  DeleteRegValue HKCU "${RUN_KEY}" "K3 Up"
  Delete "$SMPROGRAMS\K3 Up.lnk"
  Delete "$DESKTOP\K3 Up.lnk"
  Delete "$INSTDIR\${APP}"
  Delete "$INSTDIR\k3up-agent.exe"
  Delete "$INSTDIR\k3up.exe"
  Delete "$INSTDIR\k3up.ico"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  MessageBox MB_YESNO|MB_ICONQUESTION "Also delete your K3 Up workloads, logs and history?" /SD IDNO IDNO keep
    RMDir /r "$LOCALAPPDATA\K3 Up"
  keep:
SectionEnd
