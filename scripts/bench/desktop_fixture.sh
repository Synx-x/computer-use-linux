#!/bin/bash
# Desktop fixture for act evals: three dialogs, one after another. Each result
# lands in $OUT as JSON, so success never depends on the agent's report.
# Usage: desktop_fixture.sh NONCE OUTFILE
N=${1:?nonce}; OUT=${2:?outfile}
FRUITS=(Apple Banana Cherry Grape Mango Peach)
PICK=${FRUITS[$((RANDOM % 6))]}
SHUF=$(printf '%s\n' "${FRUITS[@]}" | shuf)
CODE="K$((RANDOM % 900 + 100))$N"
got1=$(zenity --list --title "Fixture $N step 1" --text "Pick $PICK, then press OK" --column Fruit $SHUF 2>/dev/null)
zenity --question --title "Fixture $N step 2" --text "Approve request $N?" --ok-label "Approve $N" --cancel-label "Reject" 2>/dev/null; e2=$?
got3=$(zenity --entry --title "Fixture $N step 3" --text "Type the code $CODE, then press OK" 2>/dev/null)
python3 - "$OUT" "$PICK" "$got1" "$e2" "$CODE" "$got3" <<'PY'
import json, sys
out, pick, got1, e2, code, got3 = sys.argv[1:]
json.dump({"d1": got1 == pick, "d2": e2 == "0", "d3": got3 == code,
           "detail": {"pick": pick, "got1": got1, "exit2": e2, "code": code, "got3": got3}}, open(out, "w"))
PY
