#!/usr/bin/env bash
# Install only. Starting/restarting a service is a separate, explicit operation.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
cargo build --release --locked
revision=$(git rev-parse HEAD)
if [[ -n $(git status --porcelain) ]]; then
  echo 'Refusing deployment of uncommitted changes. Commit and test first.' >&2
  exit 1
fi
destination="$HOME/.local/lib/mneme/$revision"
install -d -m 700 "$destination" "$HOME/.local/bin" "$HOME/.config/systemd/user"
install -m 700 target/release/mneme "$destination/mneme"
ln -sfn "$destination/mneme" "$HOME/.local/bin/mneme.next"
mv -Tf "$HOME/.local/bin/mneme.next" "$HOME/.local/bin/mneme"
install -m 600 deploy/mneme-collector.service "$HOME/.config/systemd/user/mneme-collector.service"
echo "Installed Mneme $revision. No services were started or restarted."
