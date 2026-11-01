#!/usr/bin/env bash
# Download a successful GitHub build; install without compiling on this machine.
set -euo pipefail
repo=tabenius/mneme
ref=main
run=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ref) ref=${2:?Missing ref}; shift 2 ;;
    --run) run=${2:?Missing run ID}; shift 2 ;;
    *) echo 'Usage: install-github.sh [--ref BRANCH] [--run RUN_ID]' >&2; exit 2 ;;
  esac
done
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) target=x86_64-unknown-linux-musl ;;
  Linux:aarch64 | Linux:arm64) target=aarch64-unknown-linux-musl ;;
  *) echo 'Supported hosts: Linux x86_64 and aarch64' >&2; exit 2 ;;
esac
if [[ -z "$run" ]]; then
  run=$(gh run list --repo "$repo" --workflow check.yml --branch "$ref" --status success \
    --event push --limit 1 --json databaseId --jq '.[0].databaseId // empty')
fi
[[ $run =~ ^[0-9]+$ ]] || { echo "No successful package build found for $ref" >&2; exit 1; }
metadata=$(gh api "repos/$repo/actions/runs/$run" --jq '[.status,.conclusion,.head_sha,.path] | join(" ")')
read -r status conclusion revision workflow <<< "$metadata"
[[ $status == completed && $conclusion == success && $workflow == .github/workflows/check.yml ]] || {
  echo 'Expected a successful Mneme check/package workflow' >&2; exit 1;
}
temp=$(mktemp -d)
trap 'rm -rf -- "$temp"' EXIT
gh run download "$run" --repo "$repo" --name "mneme-$target" --dir "$temp"
(
  cd -- "$temp"
  sha256sum --check "SHA256SUMS-$target"
)
archives=("$temp"/mneme-*"$target".tar.gz)
[[ ${#archives[@]} == 1 && -f ${archives[0]} ]] || { echo 'Expected exactly one binary archive' >&2; exit 1; }
tar -xzf "${archives[0]}" --no-same-owner -C "$temp"
bundle="${archives[0]%.tar.gz}"
read -r bundled_revision < "$bundle/REVISION"
[[ $bundled_revision == "$revision" ]] || { echo 'Artifact/source revision mismatch' >&2; exit 1; }
bash "$bundle/install.sh"
