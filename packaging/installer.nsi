; rustshot NSIS installer (per-user, no admin required).
; Build: makensis packaging\installer.nsi   (optionally /DPRODUCT_VERSION=x.y.z)
; Output: dist\rustshot-<version>-setup.exe
; In-app updates run it as "setup.exe /S /RELAUNCH" (see .onInit and
; .onInstSuccess); interactive installs are unaffected by that path.
!ifndef PRODUCT_VERSION
  !define PRODUCT_VERSION "0.1.1"
!endif
!define PRODUCT_NAME "rustshot"
!define PRODUCT_EXE "rustshot.exe"

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "Sections.nsh"

Name "${PRODUCT_NAME} ${PRODUCT_VERSION}"
Icon "icons\rustshot.ico"
UninstallIcon "icons\rustshot.ico"
BrandingText "${PRODUCT_NAME} ${PRODUCT_VERSION}"
OutFile "..\dist\${PRODUCT_NAME}-${PRODUCT_VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\${PRODUCT_NAME}"
InstallDirRegKey HKCU "Software\${PRODUCT_NAME}" "InstallDir"
RequestExecutionLevel user

Page directory
Page components
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "${PRODUCT_NAME} (required)" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  ${If} ${Silent}
    ; Silent update: if the old daemon is still running after the wait in
    ; .onInit, move its exe aside (allowed while it runs) so File succeeds.
    ; The new daemon deletes rustshot.exe.old when it starts.
    Delete "$INSTDIR\${PRODUCT_EXE}.old"
    FindWindow $0 "rustshot_tray"
    ${If} $0 <> 0
      Rename "$INSTDIR\${PRODUCT_EXE}" "$INSTDIR\${PRODUCT_EXE}.old"
    ${EndIf}
  ${EndIf}
  File "..\target\release\${PRODUCT_EXE}"
  File "..\LICENSE"
  File "..\THIRD_PARTY_NOTICES.md"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\${PRODUCT_NAME}" "InstallDir" "$INSTDIR"
  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortcut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk" "$INSTDIR\${PRODUCT_EXE}"
  CreateShortcut "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall.lnk" "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
      "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
      "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
      "Publisher" "${PRODUCT_NAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
      "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}" \
      "DisplayIcon" "$INSTDIR\${PRODUCT_EXE}"
SectionEnd

Section "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${PRODUCT_NAME}.lnk" "$INSTDIR\${PRODUCT_EXE}"
SectionEnd

Section "Start daemon with Windows" SecStartup
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCT_NAME}" \
      '"$INSTDIR\${PRODUCT_EXE}" daemon'
SectionEnd

; Silent installs only; interactive installs are unchanged.
Function .onInit
  ${If} ${Silent}
    ; An update keeps the user's choices: over an existing install the
    ; optional sections run again only if their shortcut / Run value exists.
    ReadRegStr $0 HKCU "Software\${PRODUCT_NAME}" "InstallDir"
    ${If} $0 != ""
      ${IfNot} ${FileExists} "$DESKTOP\${PRODUCT_NAME}.lnk"
        !insertmacro UnselectSection ${SecDesktop}
      ${EndIf}
      ClearErrors
      ReadRegStr $1 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCT_NAME}"
      ${If} ${Errors}
        !insertmacro UnselectSection ${SecStartup}
      ${EndIf}
    ${EndIf}
    ; Wait up to 10 s for the running daemon (its hidden "rustshot_tray"
    ; window) to exit.
    StrCpy $2 0
    ${Do}
      FindWindow $0 "rustshot_tray"
      ${If} $0 = 0
        ${Break}
      ${EndIf}
      Sleep 100
      IntOp $2 $2 + 1
    ${LoopUntil} $2 >= 100
  ${EndIf}
FunctionEnd

; "/S /RELAUNCH": start the new daemon after a successful silent install.
Function .onInstSuccess
  ${If} ${Silent}
    ${GetParameters} $0
    ClearErrors
    ${GetOptions} $0 "/RELAUNCH" $1
    ${IfNot} ${Errors}
      Exec '"$INSTDIR\${PRODUCT_EXE}" daemon'
    ${EndIf}
  ${EndIf}
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\${PRODUCT_EXE}"
  Delete "$INSTDIR\${PRODUCT_EXE}.old"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  ; In-app update downloads (files only; see update_install::update_dir).
  Delete "$LOCALAPPDATA\${PRODUCT_NAME}\update\*"
  RMDir "$LOCALAPPDATA\${PRODUCT_NAME}\update"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_NAME}.lnk"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall.lnk"
  RMDir "$SMPROGRAMS\${PRODUCT_NAME}"
  Delete "$DESKTOP\${PRODUCT_NAME}.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCT_NAME}"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
  DeleteRegKey HKCU "Software\${PRODUCT_NAME}"
SectionEnd
