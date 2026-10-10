#!/bin/sh
# uwsm-app for an agent desktop. uwsm starts apps in the host session, so the app
# runs directly here instead.
while [ $# -gt 0 ]; do
  case "$1" in --) shift; break ;; -*) shift ;; *) break ;; esac
done
exec "$@"
