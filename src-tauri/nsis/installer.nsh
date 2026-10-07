; NSIS installer hooks for LAN-Chat (Tauri 2).
;
; Peer discovery uses mDNS (UDP 5353) and direct TCP connections, so the
; Windows firewall must allow inbound traffic for the installed executable.
; Without these rules, other devices on the LAN cannot see this machine.
;
; The hooks are best-effort: a netsh failure never aborts the install or
; uninstall (nsExec pushes the exit code; we pop it and keep going).

!macro NSIS_HOOK_POSTINSTALL
  ; Allow inbound connections to the app binary (TCP chat + file transfer).
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LAN-Chat" dir=in action=allow program="$INSTDIR\lan-chat.exe" enable=yes profile=any'
  Pop $0

  ; Allow inbound mDNS traffic so peer announcements/queries on UDP 5353 arrive.
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LAN-Chat mDNS" dir=in action=allow protocol=UDP localport=5353 profile=any'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Remove the firewall rules added at install time (matched by name only).
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LAN-Chat"'
  Pop $0

  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LAN-Chat mDNS"'
  Pop $0
!macroend
