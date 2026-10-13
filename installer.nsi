; Mike Hopper installer (NSIS 3).
;
; Build with build-installer.ps1, or directly:
;   makensis /DVERSION=0.1.0 installer.nsi
;
; Per-user install (no UAC) into %LOCALAPPDATA%\Programs\Mike Hopper.
; The config lives next to the exe, which the app already expects.

Unicode True
SetCompressor /SOLID lzma
RequestExecutionLevel user

!ifndef VERSION
  !define VERSION "0.0.0"
!endif

!define NAME "Mike Hopper"
!define EXE "mike-hopper.exe"
!define CONFIG "mike-hopper.toml"
!define PUBLISHER "Michael Davidson"
!define URL "https://github.com/"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${NAME}"
!define VBCABLE_URL "https://vb-audio.com/Cable/"

Name "${NAME}"
OutFile "dist\MikeHopper-Setup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\${NAME}"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${NAME} now"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${NAME}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${NAME} installer"
VIAddVersionKey "LegalCopyright" "${PUBLISHER}"

; ---------------------------------------------------------------------------
; Helpers

; Sets $0 to 1 if VB-CABLE is installed, else 0. Checks the vendor key and the
; Add/Remove Programs entry in both registry views.
Function CheckVbCable
  StrCpy $0 0
  SetRegView 64
  Call CheckVbCableView
  ${If} $0 == 0
    SetRegView 32
    Call CheckVbCableView
  ${EndIf}
  SetRegView lastused
FunctionEnd

Function CheckVbCableView
  ClearErrors
  EnumRegValue $1 HKLM "SOFTWARE\VB-Audio\Cable" 0
  ${IfNot} ${Errors}
    StrCpy $0 1
    Return
  ${EndIf}
  ClearErrors
  ReadRegStr $1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\VB:VBCABLE {87459874-1236-4469}" "DisplayName"
  ${IfNot} ${Errors}
    StrCpy $0 1
  ${EndIf}
FunctionEnd

!macro KillApp
  nsExec::ExecToLog 'taskkill /F /IM ${EXE}'
  Pop $0
  Sleep 500
!macroend

; ---------------------------------------------------------------------------
; Installer

Function .onInit
  Call CheckVbCable
  ${If} $0 == 0
    MessageBox MB_YESNOCANCEL|MB_ICONEXCLAMATION \
      "VB-CABLE Virtual Audio Device was not detected.$\r$\n$\r$\n\
      ${NAME} routes your microphones into a virtual cable, so it will not work without one.$\r$\n$\r$\n\
      Yes: open the VB-CABLE download page and exit this installer.$\r$\n\
      No: install ${NAME} anyway (you can install VB-CABLE later).$\r$\n\
      Cancel: exit." \
      IDNO continue IDCANCEL quit
    ExecShell "open" "${VBCABLE_URL}"
    quit:
    Abort
    continue:
  ${EndIf}
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR"

  ; Replace a running copy (its exe would be locked otherwise).
  !insertmacro KillApp

  File "target\release\${EXE}"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  ; Shortcuts. The Startup one is minimized; the app hides its console anyway.
  CreateShortcut "$SMSTARTUP\${NAME}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0 SW_SHOWMINIMIZED "" "${NAME} - automatic microphone switcher"
  CreateDirectory "$SMPROGRAMS\${NAME}"
  CreateShortcut "$SMPROGRAMS\${NAME}\${NAME}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\${EXE}" 0 SW_SHOWMINIMIZED
  CreateShortcut "$SMPROGRAMS\${NAME}\Choose devices (setup).lnk" "$INSTDIR\${EXE}" "setup" "$INSTDIR\${EXE}" 0
  CreateShortcut "$SMPROGRAMS\${NAME}\Calibrate (monitor).lnk" "$INSTDIR\${EXE}" "--monitor" "$INSTDIR\${EXE}" 0
  CreateShortcut "$SMPROGRAMS\${NAME}\Edit config.lnk" "notepad.exe" '"$INSTDIR\${CONFIG}"'
  CreateShortcut "$SMPROGRAMS\${NAME}\Uninstall.lnk" "$INSTDIR\Uninstall.exe"

  ; Add/Remove Programs entry (per-user).
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayName" "${NAME}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr   HKCU "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINST_KEY}" "URLInfoAbout" "${URL}"
  WriteRegStr   HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr   HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr   HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"

  ; First run: no config yet, so let the app's interactive setup create one.
  ${IfNot} ${FileExists} "$INSTDIR\${CONFIG}"
    ${IfNot} ${Silent}
      DetailPrint "No ${CONFIG} found; launching device setup..."
      ExecWait '"$INSTDIR\${EXE}" setup' $0
      ${If} $0 != 0
        DetailPrint "Setup did not complete (exit code $0)."
        DetailPrint "Run 'Choose devices (setup)' from the Start menu before starting ${NAME}."
      ${EndIf}
    ${EndIf}
  ${EndIf}
SectionEnd

; ---------------------------------------------------------------------------
; Uninstaller

Section "Uninstall"
  !insertmacro KillApp

  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\Uninstall.exe"

  ${If} ${FileExists} "$INSTDIR\${CONFIG}"
    ${If} ${Silent}
      Delete "$INSTDIR\${CONFIG}"
    ${Else}
      MessageBox MB_YESNO|MB_ICONQUESTION "Also delete your configuration (${CONFIG})?" IDNO keepconfig
      Delete "$INSTDIR\${CONFIG}"
      keepconfig:
    ${EndIf}
  ${EndIf}
  RMDir "$INSTDIR"

  Delete "$SMSTARTUP\${NAME}.lnk"
  Delete "$SMPROGRAMS\${NAME}\*.lnk"
  RMDir "$SMPROGRAMS\${NAME}"

  DeleteRegKey HKCU "${UNINST_KEY}"
SectionEnd
