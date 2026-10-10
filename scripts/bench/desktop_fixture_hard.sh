#!/bin/bash
# Harder desktop fixture for act evals: a checklist and a two-field form.
# Usage: desktop_fixture_hard.sh NONCE OUTFILE
N=${1:?nonce}; OUT=${2:?outfile}
ITEMS=(Alpha Bravo Charlie Delta Echo Foxtrot Golf Hotel)
A=${ITEMS[$((RANDOM % 4))]}; B=${ITEMS[$((RANDOM % 4 + 4))]}
ROWS=(); for i in "${ITEMS[@]}"; do ROWS+=(FALSE "$i"); done
got1=$(zenity --list --checklist --title "Fixture $N step 1" --text "Tick $A and $B, then press OK" --column Pick --column Item "${ROWS[@]}" 2>/dev/null)
CITY="Lisbon$N"; NAME="Grace$N"
got2=$(zenity --forms --title "Fixture $N step 2" --text "Enter Name $NAME and City $CITY, then press OK" --add-entry Name --add-entry City 2>/dev/null)
python3 - "$OUT" "$A" "$B" "$got1" "$NAME" "$CITY" "$got2" <<'PY'
import json, sys
out, a, b, got1, name, city, got2 = sys.argv[1:]
json.dump({"h1": sorted(got1.split("|")) == sorted([a, b]), "h2": got2 == f"{name}|{city}",
           "detail": {"want1": [a, b], "got1": got1, "want2": f"{name}|{city}", "got2": got2}}, open(out, "w"))
PY
