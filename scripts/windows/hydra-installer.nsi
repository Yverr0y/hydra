; Copyright (C) 2026 Javad Rajabzadeh
; SPDX-License-Identifier: GPL-3.0-or-later
;
; NSIS installer for Hydra Download Manager on Windows.
;
; Compile from THIS directory (paths below are relative to it):
;
;   makensis -DARCH=x64   hydra-installer.nsi
;   makensis -DARCH=arm64 hydra-installer.nsi
;   makensis -DVERSION=0.3.0 -DBUILD_DIR=..\..\target\custom\release hydra-installer.nsi
;
; Expects hydra-gui.exe, hydra-host.exe, and hydra.exe already built in
; BUILD_DIR (default: target\<rust-triple>\release for the chosen ARCH), and
; the browser extensions packed into target\extensions by
; scripts/build-extensions.sh.
; Cross-build from macOS with scripts/build-windows-installer.sh, or
; natively with `cargo build --release -p hya-gui -p hya-host -p hya-cli`.
;
; Installs per-user (no elevation), mirroring scripts/install-native-host.ps1:
;   * Hydra Download Manager  - the GUI app + logo + shortcuts
;   * IPC host                - hydra-host.exe, native-messaging manifests,
;                               and the HKCU registry keys the browsers read
;   * CLI (hydra.exe + the   - with $INSTDIR appended to the user PATH so
;     hya.exe short name)      all binaries resolve in cmd/PowerShell
;   * Browser extensions      - packed .zip/.xpi + unpacked chrome/ and
;                               firefox/, with INSTALL.txt instructions
;
; Silent install (Chocolatey, winget, scripted deployments):
;
;   hydra-<ver>-windows-x64-setup.exe /S [/NODESKTOP] [/D=C:\path]
;
; Every section that is selected by default runs, so a silent install gets
; the same Start-menu AND desktop shortcuts as clicking through the wizard;
; /NODESKTOP drops the desktop shortcut. Chocolatey runs the installer
; elevated, and a per-user install lands in the profile of the account that
; elevated - the shortcuts (and the app) show up for that account.
;
; Windows has no NativeMessagingHosts directory: each browser reads a
; registry value pointing at a manifest file. Chromium browsers key it by
; extension origin, Firefox by add-on id, so two manifests are written. The
; host is only the FALLBACK transport (and the only thing that can start
; Hydra when it is not running) - day to day the extension talks to the app
; over ws://127.0.0.1:6799, which needs no registration at all.

Unicode true
SetCompressor /SOLID lzma

;--------------------------------
; Defines

!define APP_NAME     "Hydra Download Manager"
!define PUBLISHER    "Javad Rajabzadeh"
!define HOMEPAGE     "https://github.com/ja7ad/hydra"
!define HOST_NAME    "com.hydra.host"
; Chromium id is derived from the pinned key in extensions/chrome/manifest.json
; (first 16 bytes of SHA-256 of the decoded key, mapped a-p), so it can never
; drift as long as the key stays pinned. Recompute if the key ever changes:
;   python3 -c "import json,hashlib,base64;k=json.load(open('extensions/chrome/manifest.json'))['key'];h=hashlib.sha256(base64.b64decode(k)).digest()[:16];print(''.join(chr(97+(b>>4))+chr(97+(b&15)) for b in h))"
!define CHROME_EXT_ID  "jpnonmbbkjdpeebdhkjoliklfhkdcomj"
; The Chrome Web Store, Edge Add-ons and Opera add-ons sign with their own keys, so their
; listings get ids of their own (mirrors nmhost::CHROMIUM_EXT_IDS).
!define CHROME_STORE_EXT_ID "oieelfilllghmbnhofajpgpmmilfihmo"
!define EDGE_STORE_EXT_ID   "obemipfpeenmhkdpkobdkeedhdakaoai"
!define OPERA_STORE_EXT_ID  "hcmgiggmiblkfgkndbjkfonlhbmllonb"
!define FIREFOX_EXT_ID "hydra@ja7ad.github.io"

!ifndef VERSION
  ; Falls back to the workspace product version ([workspace.package] in
  ; ../../Cargo.toml) so this never drifts out of sync when no -DVERSION is
  ; passed on the command line. Same grep+cut extraction as
  ; .github/workflows/release.yml; piped through a tempfile (rather than
  ; !searchparse /file) because Cargo.toml's UTF-8 comments (em dashes) trip
  ; NSIS's "Bad text encoding" check when read directly, and NSIS strips
  ; backslashes from !system strings so a sed capture-group regex can't be
  ; used inline.
  !tempfile VERSIONFILE
  !system `grep -m1 "^version = " "../../Cargo.toml" | cut -d'"' -f2 > "${VERSIONFILE}"`
  !define /file VERSION "${VERSIONFILE}"
  !delfile "${VERSIONFILE}"
!endif
; The browser extension carries its own version (extensions/*/manifest.json)
; and bumps only when the extension changes, so it is normally BEHIND VERSION.
; It names the packed .zip/.xpi/.crx that build-extensions.sh produces, and
; INSTALL.txt below has to spell those names -- using VERSION there points the
; reader at files that do not exist. build-windows-installer.sh passes the
; version of the archive it actually packed; the fallback reads the Chromium
; manifest the same way, for a bare makensis run.
!ifndef EXT_VERSION
  !tempfile EXTVERSIONFILE
  !system `grep -m1 '"version"' "../../extensions/chrome/manifest.json" | cut -d'"' -f4 > "${EXTVERSIONFILE}"`
  !define /file EXT_VERSION "${EXTVERSIONFILE}"
  !delfile "${EXTVERSIONFILE}"
!endif

; VIProductVersion accepts only x.x.x.x numerics, so a pre-release VERSION
; (0.3.0-rc1) passes its numeric part separately; display strings keep the
; full version.
!ifndef NUM_VERSION
  !define NUM_VERSION "${VERSION}"
!endif
!ifndef ARCH
  !define ARCH "x64"
!endif
; "amd64" (or anything else non-arm64) maps to the x86_64 target below.
!if "${ARCH}" == "arm64"
  !define RUST_TARGET "aarch64-pc-windows-msvc"
!else
  !define RUST_TARGET "x86_64-pc-windows-msvc"
!endif
!ifndef BUILD_DIR
  !define BUILD_DIR "..\..\target\${RUST_TARGET}\release"
!endif

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Hydra"

Name "${APP_NAME}"
OutFile "..\..\target\hydra-${VERSION}-windows-${ARCH}-setup.exe"

; Per-user install: needs no elevation, and matches the HKCU-only
; native-messaging registration (a per-machine install would need HKLM keys
; for every browser AND still only cover one user's extension profile).
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Hydra"
InstallDirRegKey HKCU "${UNINST_KEY}" "InstallLocation"

VIProductVersion "${NUM_VERSION}.0"
VIAddVersionKey "ProductName"     "${APP_NAME}"
VIAddVersionKey "ProductVersion"  "${VERSION}"
VIAddVersionKey "FileVersion"     "${VERSION}"
VIAddVersionKey "FileDescription" "${APP_NAME} Setup"
VIAddVersionKey "CompanyName"     "${PUBLISHER}"
VIAddVersionKey "LegalCopyright"  "(C) 2026 ${PUBLISHER}. GPL-3.0-or-later."

;--------------------------------
; Includes

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "WinMessages.nsh"
!include "StrFunc.nsh"
!include "Sections.nsh"
${Using:StrFunc} StrRep

; "1" when this account can create files in $INSTDIR; see TestInstDir.
Var WritableDir

;--------------------------------
; UI

!define MUI_ICON   "hydra.ico"
!define MUI_UNICON "hydra.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\hydra-gui.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APP_NAME}"
!define MUI_FINISHPAGE_SHOWREADME "$INSTDIR\extensions\INSTALL.txt"
!define MUI_FINISHPAGE_SHOWREADME_TEXT "How to load the browser extension"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE CheckInstDir
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

;--------------------------------
; Sections

Section "Hydra Download Manager" SEC_GUI
  SectionIn RO  ; the app itself is not optional

  ; A silent install (/S, with /D= naming the directory) never reaches the
  ; Directory page, so its check runs here too. Refusing up front beats
  ; "Error opening file for writing" once per file over a half-copied
  ; directory that the uninstaller - which could not be written either -
  ; will not be able to remove.
  Call TestInstDir
  StrCmp $WritableDir "1" instdir_ok
  SetErrorLevel 2
  DetailPrint "cannot write to $INSTDIR"
  Abort "$INSTDIR cannot be written to by this account. Setup runs without \
administrator rights, so a machine-wide folder such as C:\Program Files needs \
setup to be started as administrator; otherwise choose a folder you own."
instdir_ok:

  ; Replacing a running exe fails with "file in use"; the GUI holds a
  ; single-instance guard anyway, so stop it (and the host) first.
  nsExec::Exec 'taskkill /F /IM hydra-gui.exe'
  nsExec::Exec 'taskkill /F /IM hydra-host.exe'
  Pop $0

  SetOutPath "$INSTDIR"
  File "${BUILD_DIR}\hydra-gui.exe"
  File "${BUILD_DIR}\hydra.exe"
  nsExec::ExecToLog '"$INSTDIR\hydra.exe" plugin sync-official --require-bundled'
  Pop $0
  StrCmp $0 "0" official_plugins_ok
  SetErrorLevel 2
  Abort "Official plugin installation failed. See the installation details."
official_plugins_ok:
  File "hydra.ico"
  File "..\..\LICENSE"

  ; Start-menu shortcut. hydra-gui.exe embeds hydra.ico + VERSIONINFO
  ; (crates/hydra-gui/build.rs), so shortcuts take the exe's own icon.
  CreateDirectory "$SMPROGRAMS\Hydra"
  CreateShortcut "$SMPROGRAMS\Hydra\${APP_NAME}.lnk" "$INSTDIR\hydra-gui.exe"
  CreateShortcut "$SMPROGRAMS\Hydra\Uninstall ${APP_NAME}.lnk" \
    "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "Software\Classes\.hyaplugin" "" "Hydra.PluginPackage"
  WriteRegStr HKCU "Software\Classes\Hydra.PluginPackage" "" "Hydra Plugin Package"
  WriteRegStr HKCU "Software\Classes\Hydra.PluginPackage\DefaultIcon" "" "$INSTDIR\hydra.ico"
  WriteRegStr HKCU "Software\Classes\Hydra.PluginPackage\shell\open\command" "" '$\"$INSTDIR\hydra-gui.exe$\" --install-plugin $\"%1$\"'

  WriteRegStr HKCU "Software\Classes\hydra" "" "URL:Hydra Plugin Install"
  WriteRegStr HKCU "Software\Classes\hydra" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\hydra\shell\open\command" "" '$\"$INSTDIR\hydra-gui.exe$\" $\"%1$\"'

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Add/Remove Programs entry (HKCU to match the per-user install).
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayName"     "${APP_NAME}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayVersion"  "${VERSION}"
  WriteRegStr   HKCU "${UNINST_KEY}" "Publisher"       "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINST_KEY}" "URLInfoAbout"    "${HOMEPAGE}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayIcon"     "$INSTDIR\hydra.ico"
  WriteRegStr   HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr   HKCU "${UNINST_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0
SectionEnd

Section "Browser IPC Host" SEC_HOST
  SetOutPath "$INSTDIR"
  File "${BUILD_DIR}\hydra-host.exe"

  ; JSON needs the path escaped for Windows separators.
  ${StrRep} $1 "$INSTDIR\hydra-host.exe" "\" "\\"

  ; Chromium-family manifest (keyed by extension origin).
  FileOpen $0 "$INSTDIR\${HOST_NAME}.json" w
  FileWrite $0 '{$\r$\n'
  FileWrite $0 '  "name": "${HOST_NAME}",$\r$\n'
  FileWrite $0 '  "description": "Hydra Download Manager native host",$\r$\n'
  FileWrite $0 '  "path": "$1",$\r$\n'
  FileWrite $0 '  "type": "stdio",$\r$\n'
  FileWrite $0 '  "allowed_origins": [$\r$\n'
  FileWrite $0 '    "chrome-extension://${CHROME_EXT_ID}/",$\r$\n'
  FileWrite $0 '    "chrome-extension://${CHROME_STORE_EXT_ID}/",$\r$\n'
  FileWrite $0 '    "chrome-extension://${EDGE_STORE_EXT_ID}/",$\r$\n'
  FileWrite $0 '    "chrome-extension://${OPERA_STORE_EXT_ID}/"$\r$\n'
  FileWrite $0 '  ]$\r$\n'
  FileWrite $0 '}$\r$\n'
  FileClose $0

  ; Firefox manifest (keyed by add-on id).
  FileOpen $0 "$INSTDIR\${HOST_NAME}.firefox.json" w
  FileWrite $0 '{$\r$\n'
  FileWrite $0 '  "name": "${HOST_NAME}",$\r$\n'
  FileWrite $0 '  "description": "Hydra Download Manager native host",$\r$\n'
  FileWrite $0 '  "path": "$1",$\r$\n'
  FileWrite $0 '  "type": "stdio",$\r$\n'
  FileWrite $0 '  "allowed_extensions": ["${FIREFOX_EXT_ID}"]$\r$\n'
  FileWrite $0 '}$\r$\n'
  FileClose $0

  ; Each browser reads the DEFAULT value of its NativeMessagingHosts key.
  WriteRegStr HKCU "Software\Google\Chrome\NativeMessagingHosts\${HOST_NAME}"              "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${HOST_NAME}"             "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\${HOST_NAME}" "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Chromium\NativeMessagingHosts\${HOST_NAME}"                   "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Vivaldi\NativeMessagingHosts\${HOST_NAME}"                    "" "$INSTDIR\${HOST_NAME}.json"
  WriteRegStr HKCU "Software\Mozilla\NativeMessagingHosts\${HOST_NAME}"                    "" "$INSTDIR\${HOST_NAME}.firefox.json"
SectionEnd

Section "Command-Line Alias (hya) + PATH" SEC_CLI
  SetOutPath "$INSTDIR"
  ; The short second name for the CLI: `hydra` is also THC-Hydra, the login
  ; auditor, and three letters types better for a command run as often as a
  ; download. A second copy rather than a link: this installer runs per-user
  ; without elevation, and Windows grants symlinks only to an elevated shell
  ; or with Developer Mode on. hya_updater re-copies it after a self update.
  File /oname=hya.exe "${BUILD_DIR}\hydra.exe"

  ; Append $INSTDIR to the per-user PATH (HKCU\Environment) so `hydra`,
  ; `hydra-gui`, and `hydra-host` resolve in cmd, PowerShell, and any
  ; terminal.
  ;
  ; The read/modify/write runs in PowerShell rather than ReadRegStr +
  ; WriteRegExpandStr here: NSIS runtime strings cap at NSIS_MAX_STRLEN
  ; (1024 characters in official builds), so a longer PATH reads back EMPTY
  ; and this block used to take its "PATH was empty" branch and REPLACE the
  ; whole value with $INSTDIR, wiping every existing entry (the uninstaller
  ; correspondingly wrote an empty PATH). The helper script uses .NET
  ; registry APIs, which have no length limit, keep %VAR% entries
  ; unexpanded, and write back in the value's original REG_EXPAND_SZ kind.
  ; If PowerShell cannot start, the step is skipped: a missing PATH entry
  ; is recoverable, a wiped PATH is not.
  ;
  ; nsExec::Exec (not ExecToStack) pushes a single value, and comparing it
  ; as a string keeps "error"/"timeout" from being mistaken for success.
  ; The environment-change broadcast is done here on success, so the
  ; helper needs no Add-Type (no csc.exe round trip, and it stays usable
  ; under ConstrainedLanguage mode).
  StrCpy $0 "$INSTDIR" "" -1        ; normalize $INSTDIR: drop a trailing
  StrCmp $0 "\" 0 +3                ; backslash so the helper sees the same
  StrCpy $0 "$INSTDIR" -1           ; spelling the uninstaller will strip
  Goto +2
  StrCpy $0 "$INSTDIR"
  InitPluginsDir
  File "/oname=$PLUGINSDIR\set-user-path.ps1" "set-user-path.ps1"
  nsExec::Exec '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$PLUGINSDIR\set-user-path.ps1" -Action Append -Dir "$0"'
  Pop $0
  StrCmp $0 "0" 0 ps_path_skip
  ; Tell running shells/Explorer the environment changed; new terminals
  ; pick it up immediately, already-open ones still need a restart.
  SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
  Goto ps_path_done
ps_path_skip:
  DetailPrint "user PATH not updated (set-user-path.ps1 exit $0)"
ps_path_done:
SectionEnd

Section "Browser Extensions" SEC_EXT
  ; Built by scripts/build-extensions.sh, which build-windows-installer.sh
  ; runs before makensis: the packed .zip/.xpi plus the unpacked directories
  ; those archives were made from. Both shapes ship because an unsigned
  ; extension cannot be installed from a file in either browser family --
  ; Chromium needs "Load unpacked", Firefox takes the .xpi as a TEMPORARY
  ; add-on. The .zip/.xpi are there for a store upload, for a Firefox
  ; Developer Edition/ESR install with signature enforcement off, and so the
  ; files can be copied to another machine as one item each.
  SetOutPath "$INSTDIR\extensions"
  File "..\..\target\extensions\hydra-chrome-*.zip"
  File "..\..\target\extensions\hydra-firefox-*.xpi"
  ; The .crx exists only when the build machine had the extension signing
  ; key (scripts/build-extensions.sh --crx-key), so it is packed
  ; conditionally rather than being a build-breaking requirement.
!if /FileExists "..\..\target\extensions\hydra-chrome-*.crx"
  !define HAVE_CRX
  File "..\..\target\extensions\hydra-chrome-*.crx"
!endif
  File /r "..\..\target\extensions\chrome"
  File /r "..\..\target\extensions\firefox"

  ; The same instructions build-extensions.sh writes as INSTALL.txt on macOS
  ; and Linux, spelled with the real $INSTDIR (which the user may have
  ; changed on the Directory page, so this is generated here rather than
  ; packed in). Keep the two texts in step.
  FileOpen $0 "$INSTDIR\extensions\INSTALL.txt" w
  FileWrite $0 'Hydra browser extension - installing the unsigned build$\r$\n'
  FileWrite $0 '=======================================================$\r$\n$\r$\n'
  FileWrite $0 'These are UNSIGNED builds: they carry no Chrome Web Store or$\r$\n'
  FileWrite $0 'addons.mozilla.org signature, so each browser needs its developer mode$\r$\n'
  FileWrite $0 '(or a temporary install) to accept them. The extension itself is$\r$\n'
  FileWrite $0 'identical to the store build.$\r$\n$\r$\n'
  FileWrite $0 'In this directory:$\r$\n$\r$\n'
  FileWrite $0 '  hydra-chrome-${EXT_VERSION}.zip   packed build for the Chromium family$\r$\n'
!ifdef HAVE_CRX
  FileWrite $0 '  hydra-chrome-${EXT_VERSION}.crx   the same build, signed, for policy deployment$\r$\n'
!endif
  FileWrite $0 '  hydra-firefox-${EXT_VERSION}.xpi  packed build for Firefox$\r$\n'
  FileWrite $0 '  chrome\           the same Chromium build, already unpacked$\r$\n'
  FileWrite $0 '  firefox\          the same Firefox build, already unpacked$\r$\n'
  FileWrite $0 '  INSTALL.txt       this file$\r$\n$\r$\n'
  FileWrite $0 'Chrome, Edge, Opera, Brave, Vivaldi, Arc, Chromium$\r$\n'
  FileWrite $0 '--------------------------------------------------$\r$\n'
  FileWrite $0 'Chromium only installs a .crx that came from the Web Store, so an$\r$\n'
  FileWrite $0 'unsigned build is loaded from the unpacked directory instead.$\r$\n$\r$\n'
  FileWrite $0 '  1. Open the extensions page:$\r$\n'
  FileWrite $0 '         Chrome   chrome://extensions$\r$\n'
  FileWrite $0 '         Edge     edge://extensions$\r$\n'
  FileWrite $0 '         Opera    opera://extensions$\r$\n'
  FileWrite $0 '         Brave    brave://extensions$\r$\n'
  FileWrite $0 '         Vivaldi  vivaldi://extensions$\r$\n'
  FileWrite $0 '  2. Turn on "Developer mode" - top right in Chrome, Brave and Vivaldi;$\r$\n'
  FileWrite $0 '     bottom left in Edge; the sidebar in Opera.$\r$\n'
  FileWrite $0 '  3. Click "Load unpacked" and select:$\r$\n'
  FileWrite $0 '         $INSTDIR\extensions\chrome$\r$\n'
  FileWrite $0 '  4. Leave that folder where it is. The browser re-reads it from disk at$\r$\n'
  FileWrite $0 '     every start, and moving or deleting it uninstalls the extension.$\r$\n$\r$\n'
!ifdef HAVE_CRX
  FileWrite $0 'The signed hydra-chrome-${EXT_VERSION}.crx is for deploying by enterprise$\r$\n'
  FileWrite $0 'policy (ExtensionSettings or ExtensionInstallForcelist, pointing at an$\r$\n'
  FileWrite $0 'update manifest you host). Dragging it onto the extensions page will not$\r$\n'
  FileWrite $0 'work: Chromium refuses any .crx that did not come from the Web Store.$\r$\n$\r$\n'
!endif
  FileWrite $0 'The extension id is ${CHROME_EXT_ID} in every Chromium$\r$\n'
  FileWrite $0 'browser - it is pinned by the manifest key, and it is the id this$\r$\n'
  FileWrite $0 'installer already allow-listed for the native messaging host.$\r$\n$\r$\n'
  FileWrite $0 'Firefox$\r$\n'
  FileWrite $0 '-------$\r$\n'
  FileWrite $0 'Temporary - works in every Firefox, removed at the next restart:$\r$\n$\r$\n'
  FileWrite $0 '  1. Open about:debugging#/runtime/this-firefox$\r$\n'
  FileWrite $0 '  2. Click "Load Temporary Add-on..." and select:$\r$\n'
  FileWrite $0 '         $INSTDIR\extensions\hydra-firefox-${EXT_VERSION}.xpi$\r$\n'
  FileWrite $0 '     (or $INSTDIR\extensions\firefox\manifest.json)$\r$\n$\r$\n'
  FileWrite $0 'Permanent - Developer Edition, Nightly and ESR only:$\r$\n$\r$\n'
  FileWrite $0 '  1. Open about:config and set$\r$\n'
  FileWrite $0 '         xpinstall.signatures.required = false$\r$\n'
  FileWrite $0 '     Release and Beta Firefox ignore this setting and keep refusing an$\r$\n'
  FileWrite $0 '     unsigned add-on; use the temporary install above instead.$\r$\n'
  FileWrite $0 '  2. Open about:addons, click the gear icon, choose$\r$\n'
  FileWrite $0 '     "Install Add-on From File..." and select the .xpi above.$\r$\n$\r$\n'
  FileWrite $0 'After installing$\r$\n'
  FileWrite $0 '----------------$\r$\n'
  FileWrite $0 'Restart the browser once so it picks up the native-messaging$\r$\n'
  FileWrite $0 'registration this installer wrote, then open the Hydra toolbar icon:$\r$\n'
  FileWrite $0 'the status dot is green when the extension has reached the app$\r$\n'
  FileWrite $0 '(WebSocket 127.0.0.1:6799).$\r$\n'
  FileClose $0
SectionEnd

Section "Logo & Branding Assets" SEC_LOGO
  SetOutPath "$INSTDIR\assets"
  File "..\..\docs\logo.png"
SectionEnd

; Selected by default (no /o): a silent /S install takes the default
; selection and never shows the Components page, so an opt-in section would
; be silently skipped - which is exactly how Chocolatey installs ended up
; without a desktop shortcut. Wizard users untick it; scripted installs
; pass /NODESKTOP.
Section "Desktop Shortcut" SEC_DESKTOP
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\hydra-gui.exe"
SectionEnd

;--------------------------------
; Component descriptions

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_GUI}     "The Hydra Download Manager application (required)."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_HOST}    "Native-messaging host bridging browser extensions to Hydra, plus its registry registration for Chrome, Edge, Brave, Chromium, Vivaldi, and Firefox."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_CLI}     "The hydra command-line downloader, with the install directory added to your PATH for cmd, PowerShell, and other terminals."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_EXT}     "Browser extensions for the Chromium family (Chrome, Edge, Opera, Brave, Vivaldi) and Firefox: packed .zip/.xpi plus the unpacked directories a developer-mode install loads, with instructions."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_LOGO}    "Hydra logo image installed alongside the application."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DESKTOP} "Shortcut to Hydra Download Manager on the desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

;--------------------------------
; Callbacks

; Probe $INSTDIR for write access, into $WritableDir.
;
; RequestExecutionLevel is `user`: setup never elevates, so a machine-wide
; target (C:\Program Files, C:\ProgramData) is read-only to it. NSIS does not
; check this anywhere - it copies files one by one and pops "Error opening
; file for writing" for each, and a run that is clicked through with Ignore
; leaves shortcuts and registry entries pointing into a directory holding
; none of the binaries and no uninstall.exe.
;
; Creating a file is the only reliable test: the ACL that matters is the
; one on the directory, and an admin-owned folder is readable and listable
; by everyone. A directory this creates for the probe is taken away again,
; so cancelling on the Directory page leaves nothing behind.
Function TestInstDir
  StrCpy $WritableDir "0"
  StrCpy $0 "0"                    ; "1" when the probe made the directory
  IfFileExists "$INSTDIR\*.*" probe
  CreateDirectory "$INSTDIR"
  StrCpy $0 "1"
probe:
  ClearErrors
  GetTempFileName $1 "$INSTDIR"
  IfErrors clean
  StrCmp $1 "" clean
  Delete $1
  StrCpy $WritableDir "1"
clean:
  StrCmp $0 "1" 0 +2
  RMDir "$INSTDIR"
FunctionEnd

; Directory page: keep the wizard on the page until the chosen folder is one
; this install can actually write to.
Function CheckInstDir
  Call TestInstDir
  StrCmp $WritableDir "1" writable
  MessageBox MB_OK|MB_ICONEXCLAMATION "Hydra installs for your user account and does not ask for administrator rights, so it cannot write to:$\r$\n$\r$\n$INSTDIR$\r$\n$\r$\nChoose a folder you own - the default $LOCALAPPDATA\Programs\Hydra is one - or close setup and start it again with Run as administrator to install into a machine-wide folder."
  Abort
writable:
FunctionEnd

Function .onInit
  ; /NODESKTOP: opt out of the desktop shortcut from the command line
  ; (Chocolatey: --install-arguments="'/NODESKTOP'", appended to /S).
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/NODESKTOP" $1
  IfErrors no_desktop_done      ; switch absent -> keep the default selection
  !insertmacro UnselectSection ${SEC_DESKTOP}
no_desktop_done:
FunctionEnd

Function .onInstSuccess
  ; Tell Explorer the shell items changed so the new Start-menu entry and
  ; desktop icon appear at once. Explorer usually notices new .lnk files on
  ; its own, but a silent install run from a background process (Chocolatey)
  ; can leave the desktop stale until the next refresh.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
FunctionEnd

;--------------------------------
; Uninstaller

Section "Uninstall"
  nsExec::Exec 'taskkill /F /IM hydra-gui.exe'
  nsExec::Exec 'taskkill /F /IM hydra-host.exe'
  Pop $0

  ; Native-messaging registration.
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Chromium\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Vivaldi\NativeMessagingHosts\${HOST_NAME}"
  DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\${HOST_NAME}"

  ; Strip $INSTDIR out of the per-user PATH. Same PowerShell helper as the
  ; installer: with NSIS_MAX_STRLEN at 1024, a longer PATH reads back empty
  ; and writing it out here used to wipe the whole value. nsExec::Exec
  ; pushes a single value, compared as a string so "error"/"timeout" cannot
  ; pass for success; the broadcast runs here on success.
  StrCpy $0 "$INSTDIR" "" -1        ; normalize $INSTDIR: drop a trailing
  StrCmp $0 "\" 0 +3                ; backslash so both directions see the
  StrCpy $0 "$INSTDIR" -1           ; same spelling
  Goto +2
  StrCpy $0 "$INSTDIR"
  InitPluginsDir
  File "/oname=$PLUGINSDIR\set-user-path.ps1" "set-user-path.ps1"
  nsExec::Exec '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$PLUGINSDIR\set-user-path.ps1" -Action Remove -Dir "$0"'
  Pop $0
  StrCmp $0 "0" 0 ps_unpath_skip
  SendMessage ${HWND_BROADCAST} ${WM_WININICHANGE} 0 "STR:Environment" /TIMEOUT=5000
  Goto ps_unpath_done
ps_unpath_skip:
  DetailPrint "user PATH not updated (set-user-path.ps1 exit $0)"
ps_unpath_done:

  Delete "$INSTDIR\hydra-gui.exe"
  Delete "$INSTDIR\hydra.exe"
  Delete "$INSTDIR\hya.exe"
  Delete "$INSTDIR\hydra-host.exe"
  Delete "$INSTDIR\${HOST_NAME}.json"
  Delete "$INSTDIR\${HOST_NAME}.firefox.json"
  Delete "$INSTDIR\hydra.ico"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\extensions"
  RMDir /r "$INSTDIR\assets"
  ; Not /r: leaves user data behind if anything else ever lands in $INSTDIR.
  RMDir "$INSTDIR"

  ; Delete reports nothing to the user, so an uninstall that removed nothing
  ; - a copy installed into a machine-wide folder by an elevated setup, run
  ; from an ordinary shell - otherwise ends on "Completed" with every file
  ; still there. The Add/Remove Programs entry is kept in that case (see
  ; below) so the uninstall can be retried with the rights it needs.
  IfFileExists "$INSTDIR\hydra-gui.exe" 0 removed
  DetailPrint "could not remove $INSTDIR"
  MessageBox MB_OK|MB_ICONEXCLAMATION "Files in $INSTDIR could not be removed - this account may not write there.$\r$\n$\r$\nRun the uninstaller again with Run as administrator to finish removing Hydra." /SD IDOK
removed:

  Delete "$SMPROGRAMS\Hydra\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\Hydra\Uninstall ${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\Hydra"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  ; "Launch on startup" login item (written by the app, autostart.rs), plus
  ; the Startup-folder .cmd that pre-0.2.x versions wrote.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${APP_NAME}"
  Delete "$SMSTARTUP\hydra.cmd"

  ; Only when the files are actually gone: an entry removed over a directory
  ; that is still there takes the only offer of a retry with it.
  IfFileExists "$INSTDIR\hydra-gui.exe" uninstall_registry_kept
  ReadRegStr $0 HKCU "Software\Classes\hydra\shell\open\command" ""
  ${If} $0 == '$\"$INSTDIR\hydra-gui.exe$\" $\"%1$\"'
    DeleteRegKey HKCU "Software\Classes\hydra"
  ${EndIf}
  DeleteRegKey HKCU "Software\Classes\Hydra.PluginPackage"
  ReadRegStr $0 HKCU "Software\Classes\.hyaplugin" ""
  ${If} $0 == "Hydra.PluginPackage"
    DeleteRegValue HKCU "Software\Classes\.hyaplugin" ""
  ${EndIf}
  DeleteRegKey HKCU "${UNINST_KEY}"
  uninstall_registry_kept:
SectionEnd
