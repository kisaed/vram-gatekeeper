; Extra uninstaller steps (spec section 25.4).
;
; Tauri's uninstaller shows a "Delete the application data" checkbox that is
; off by default and sets $DeleteAppDataCheckboxState. Tauri's own handling of
; that box removes only the WebView2 data of the app identifier. This hook adds
; the application's own settings, state, logs, downloaded engines and stored
; secrets, and only when the box is ticked and this is not an update.
;
; Downloaded models are never touched, and the install folder is never removed
; recursively: it shares its folder with state.json, logs and engines.

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    ; Settings (config.json, its backup and schema).
    RmDir /r "$APPDATA\VRAM Gatekeeper"
    ; Learned state, logs and downloaded engines next to the program.
    Delete "$LOCALAPPDATA\VRAM Gatekeeper\state.json*"
    RmDir /r "$LOCALAPPDATA\VRAM Gatekeeper\logs"
    RmDir /r "$LOCALAPPDATA\VRAM Gatekeeper\engines"
    ; Secrets in Credential Manager. nsExec keeps a console window from flashing.
    nsExec::Exec 'cmdkey /delete:"VRAM Gatekeeper/lan_api_key"'
    Pop $0
    nsExec::Exec 'cmdkey /delete:"VRAM Gatekeeper/hf_token"'
    Pop $0
    ; Remove the folder only if nothing else is left in it.
    RmDir "$LOCALAPPDATA\VRAM Gatekeeper"
  ${EndIf}
!macroend
