#!/usr/bin/env bash
# Run from an extracted release bundle. Installs for the current user, never builds.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
read -r revision < "$root/REVISION"
read -r target < "$root/TARGET"
[[ $revision =~ ^[a-f0-9]{40}$ ]] || { echo 'Invalid bundle revision' >&2; exit 2; }
case "$(uname -s):$(uname -m):$target" in
  Linux:x86_64:x86_64-unknown-linux-musl | Linux:aarch64:aarch64-unknown-linux-musl | Linux:arm64:aarch64-unknown-linux-musl) ;;
  *) echo "Bundle target $target does not match this host" >&2; exit 2 ;;
esac
"$root/bin/mneme" --version
destination="$HOME/.local/lib/mneme/$revision"
install -d -m 700 "$destination" "$HOME/.local/bin" "$HOME/.config/systemd/user"
install -m 700 "$root/bin/mneme" "$destination/mneme"
install -m 600 "$root/REVISION" "$root/TARGET" "$root/VERSION" "$destination/"
install -m 600 "$root"/deploy/mneme-* "$HOME/.config/systemd/user/"
link="$HOME/.local/bin/.mneme-install-$$"
trap 'rm -f -- "$link"' EXIT
ln -s "$destination/mneme" "$link"
mv -Tf "$link" "$HOME/.local/bin/mneme"
echo "Installed Mneme $revision at $HOME/.local/bin/mneme"
echo 'No services were started or restarted. Add ~/.local/bin to PATH if needed.'
