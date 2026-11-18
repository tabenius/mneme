#!/usr/bin/env bash
# Package a previously compiled binary. No build, network, service or index access.
set -euo pipefail
if [[ $# != 3 ]]; then
  echo 'Usage: package-release.sh BINARY RUST_TARGET OUTPUT_DIR' >&2
  exit 2
fi
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
binary=$(realpath -- "$1")
target=$2
case "$target" in
  x86_64-unknown-linux-musl) architecture=amd64 ;;
  aarch64-unknown-linux-musl) architecture=arm64 ;;
  *) echo "Unsupported release target: $target" >&2; exit 2 ;;
esac
[[ -x "$binary" ]] || { echo 'Binary must exist and be executable' >&2; exit 2; }
mkdir -p -- "$3"
out=$(realpath -- "$3")
revision=$(git -C "$root" rev-parse HEAD)
epoch=$(git -C "$root" show -s --format=%ct HEAD)
base_version=$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' "$root/Cargo.toml")
version="$base_version+git.${revision:0:12}"
if [[ ${GITHUB_REF_TYPE:-} == tag ]]; then
  [[ ${GITHUB_REF_NAME:-} == "v$base_version" ]] || { echo 'Tag must match Cargo version' >&2; exit 2; }
  version=$base_version
fi
name="mneme-$version-$target"
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
bundle="$stage/$name"
install -d "$bundle/bin" "$bundle/deploy" "$bundle/docs"
install -m 755 "$binary" "$bundle/bin/mneme"
install -m 644 "$root"/deploy/mneme-* "$bundle/deploy/"
install -m 755 "$root/scripts/install-binary.sh" "$bundle/install.sh"
install -m 644 "$root/LICENSE" "$bundle/LICENSE"
install -m 644 "$root/docs/DEPLOYMENT.md" "$bundle/docs/DEPLOYMENT.md"
printf '%s\n' "$revision" > "$bundle/REVISION"
printf '%s\n' "$base_version" > "$bundle/VERSION"
printf '%s\n' "$target" > "$bundle/TARGET"
tar --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner \
  -C "$stage" -cf - "$name" | gzip -n > "$out/$name.tar.gz"

deb="$stage/debian"
install -d "$deb/DEBIAN" "$deb/usr/bin" "$deb/usr/lib/systemd/user" "$deb/usr/share/doc/mneme"
install -m 755 "$binary" "$deb/usr/bin/mneme"
for unit in "$root"/deploy/mneme-*; do
  sed 's|ExecStart=%h/.local/bin/mneme|ExecStart=/usr/bin/mneme|' "$unit" > "$deb/usr/lib/systemd/user/$(basename -- "$unit")"
done
install -m 644 "$root/LICENSE" "$deb/usr/share/doc/mneme/copyright"
install -m 644 "$root/docs/DEPLOYMENT.md" "$deb/usr/share/doc/mneme/DEPLOYMENT.md"
install -m 644 "$bundle/REVISION" "$deb/usr/share/doc/mneme/REVISION"
cat > "$deb/DEBIAN/control" <<EOF
Package: mneme
Version: $version
Section: utils
Priority: optional
Architecture: $architecture
Maintainer: Mneme maintainers <noreply@github.com>
Homepage: https://github.com/tabenius/mneme
Recommends: python3
Suggests: python3-numpy
Description: Evidence-oriented local session history index and MCP server
 Static Linux binary, optional local Python semantic helpers, and user service
 templates. Installing this package does not start or restart any service.
EOF
SOURCE_DATE_EPOCH=$epoch dpkg-deb --root-owner-group --build "$deb" "$out/mneme_${version}_${architecture}.deb"
(
  cd -- "$out"
  sha256sum "$name.tar.gz" "mneme_${version}_${architecture}.deb" > "SHA256SUMS-$target"
)
echo "Packaged $version ($architecture) from $revision"
