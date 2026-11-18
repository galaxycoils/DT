; DT Windows Installer (NSIS 3.13)
; Installs under Program Files, registers Start Menu & Installed Apps entries,
; offers DT and Doctor shortcuts for the signed-in user.

!include "MUI2.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"

Name "DT ${VERSION}"
OutFile "${OUT_FILE}"
InstallDir "$PROGRAMFILES64\DT"
RequestExecutionLevel admin
!ifdef UNINST_SIGN_CMD
  !uninstfinalize '${UNINST_SIGN_CMD}'
!endif

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Function .onInit
  SetRegView 64
  ReadRegStr $0 HKLM "Software\DT" "InstallDir"
  ${If} $0 != ""
    StrCpy $INSTDIR $0
  ${EndIf}
FunctionEnd

Section "DT Core" SecCore
  SetRegView 64
  SetShellVarContext all
  SetOutPath "$INSTDIR\bin"
  File "${BIN_DIR}\dt.exe"
  File "${BIN_DIR}\gm.exe"
  File "${BIN_DIR}\dt-doctor.exe"
  File "${BIN_DIR}\gm-daemon.exe"
  File "${BIN_DIR}\gm-cu-helper.exe"
  File "${BIN_DIR}\gm-ui.exe"

  SetOutPath "$INSTDIR"
  File "${SRC_ROOT}\LICENSE"
  File "${SRC_ROOT}\THIRD_PARTY_NOTICES.md"
  File "${SRC_ROOT}\README.md"

  WriteUninstaller "$INSTDIR\Uninstall DT.exe"

  ; Start Menu entries
  CreateDirectory "$SMPROGRAMS\DT"
  CreateShortcut "$SMPROGRAMS\DT\DT.lnk" "$INSTDIR\bin\gm-ui.exe" "native"
  CreateShortcut "$SMPROGRAMS\DT\DT Doctor.lnk" "$INSTDIR\bin\dt-doctor.exe" "check"
  CreateShortcut "$SMPROGRAMS\DT\Uninstall DT.lnk" "$INSTDIR\Uninstall DT.exe"

  ; Installed Apps (Add/Remove Programs) registration
  WriteRegStr HKLM "Software\DT" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT" "DisplayName" "DT"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT" "Publisher" "DT"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT" "UninstallString" "$\"$INSTDIR\Uninstall DT.exe$\""
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT" "ModifyPath" "$\"$INSTDIR\bin\dt-doctor.exe$\" repair"
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT" "NoModify" 0
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT" "NoRepair" 0
SectionEnd

Section "Uninstall"
  SetRegView 64
  SetShellVarContext all
  ; Remove DT-owned program files and registrations while preserving user data (%APPDATA%\GrokMuse / %APPDATA%\DT) by default.
  Delete "$SMPROGRAMS\DT\DT.lnk"
  Delete "$SMPROGRAMS\DT\DT Doctor.lnk"
  Delete "$SMPROGRAMS\DT\Uninstall DT.lnk"
  RMDir "$SMPROGRAMS\DT"

  Delete "$INSTDIR\bin\dt.exe"
  Delete "$INSTDIR\bin\gm.exe"
  Delete "$INSTDIR\bin\dt-doctor.exe"
  Delete "$INSTDIR\bin\gm-daemon.exe"
  Delete "$INSTDIR\bin\gm-cu-helper.exe"
  Delete "$INSTDIR\bin\gm-ui.exe"
  RMDir "$INSTDIR\bin"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\Uninstall DT.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT"
  DeleteRegKey HKLM "Software\DT"
SectionEnd
