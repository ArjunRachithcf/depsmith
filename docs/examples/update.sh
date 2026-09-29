#!/bin/sh
# Install depsmith and its native tools before calling this script.
# Usage: update.sh PROJECT_DIR REPORT_DIR [check|apply]
set -eu
project_dir=$(cd "${1:?project directory required}" && pwd -P)
mkdir -p "${2:?report directory required}"
report_dir=$(cd "$2" && pwd -P)
case "$report_dir/" in
  "$project_dir/"*) echo "Reports must be outside the project" >&2; exit 2 ;;
esac
mode=${3:-check}
case "$mode" in
  check) operation=check ;;
  apply) operation=update ;;
  *) echo "Expected check or apply" >&2; exit 2 ;;
esac
# --all makes selection explicit; replace with repeated --target flags if needed.
# Resolve and apply in one process. JSON is a report, not an executable proposal.
if [ "$mode" = apply ]; then
  depsmith "$operation" --root "$project_dir" --all --apply --yes --non-interactive --json > "$report_dir/depsmith.json"
else
  depsmith "$operation" --root "$project_dir" --all --non-interactive --json > "$report_dir/depsmith.json"
fi
