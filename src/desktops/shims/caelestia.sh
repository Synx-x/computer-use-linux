#!/bin/sh
# caelestia for an agent desktop. `caelestia shell ...` reaches the desktop's own
# Caelestia shell, never the host's: both share XDG_RUNTIME_DIR, so the instance is
# picked by pid, the qs whose WAYLAND_DISPLAY is this desktop's. With no shell on
# the desktop the call does nothing. Other subcommands run the real CLI.
if [ "$1" = shell ]; then
  shift
  [ $# -eq 0 ] && exit 0
  for pid in $(pgrep -f 'qs -c caelestia'); do
    if tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null | grep -qx "WAYLAND_DISPLAY=$WAYLAND_DISPLAY"; then
      exec qs ipc --pid "$pid" call "$@"
    fi
  done
  exit 1
fi
exec /usr/bin/caelestia "$@"
