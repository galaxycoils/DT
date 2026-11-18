; DT Windows Installer (NSIS 3.13)
; Installs under Program Files, registers Start Menu & Installed Apps entries,
; configures per-user isolation, checks redistributables, and resumes WHPX setup via `dt doctor resume`.

!include "MUI2.nsh"
!include "x64.nsh"

Name "DT ${VERSION}"
OutFile "${OUT_FILE}"
InstallDir "$PROGRAMFILES64\DT"
InstallDirRegKey HKLM "Software\DT" "InstallDir"
RequestExecutionLevel admin

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "DT Core" SecCore
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

Section "Optional WHPX Check & Resume Hook" SecWhpx
  ; Run interactive-user setup check via dt-doctor without leaving elevated context in control of user profile.
  ExecWait '"$INSTDIR\bin\dt-doctor.exe" check --json'
SectionEnd

Section "Uninstall"
  ; Remove DT-owned program files and registrations while preserving user data (%APPDATA%\GrokMuse / %APPDATA%\DT) by default.
  Delete "$SMPROGRAMS\DT\DT.lnk"
  Delete "$SMPROGRAMS\DT\DT Doctor.lnk"
  Delete "$SMPROGRAMS\DT\Uninstall DT.lnk"
  RMDir "$SMPROGRAMS\DT"

  RMDir /r "$INSTDIR\bin"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\Uninstall DT.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\DT"
  DeleteRegKey HKLM "Software\DT"
SectionEnd
