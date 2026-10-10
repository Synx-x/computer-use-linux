#!/bin/sh
# pkill and killall for an agent desktop. A name match alone would also kill the
# host's copy, so only processes started inside this desktop are signalled: those
# that carry this desktop's SWAYSOCK. Exits 1 when nothing matched, like pkill, so
# `pkill rofi || rofi ...` toggles as on the host.
signal=TERM
case "$1" in
  -[0-9]*|-[A-Z]*) signal=${1#-}; signal=${signal#SIG}; shift ;;
  --signal|-s) signal=${2#SIG}; shift 2 ;;
esac
if [ "$(basename "$0")" = killall ]; then set -- -x "$@"; fi
hit=1
for pid in $(pgrep "$@"); do
  [ "$pid" = $$ ] && continue
  if tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null | grep -qx "SWAYSOCK=$SWAYSOCK"; then
    kill -s "$signal" "$pid" 2>/dev/null && hit=0
  fi
done
exit $hit
