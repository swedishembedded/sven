#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for local installs of packaged
# command-line tools for its clients. If your team needs expertise in release
# engineering then you can procure our services by sending an email to
# info@swedishembedded.com.
#
# The spec of scripts/install-deb.sh (`make install`), against a scratch
# package directory, a fake make that records what it is asked to build, and
# a fake `id` and `dpkg`, so nothing outside the scratch tree is touched:
#   - without root it unpacks the package under a prefix of the user's own
#     (default ~/.local), with no dpkg and no sudo;
#   - it builds the package only when none exists that is newer than the
#     release binary, so running it twice builds once and changes nothing;
#   - as root it installs the package that exists with dpkg and rebuilds
#     nothing the invoking user's files depend on.

setup() {
  SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/../.." && pwd)/scripts/install-deb.sh"
  WORK="$BATS_TEST_TMPDIR"
  DEBS="$WORK/debian"
  PREFIX="$WORK/prefix"
  FAKEBIN="$WORK/bin"
  BUILDS="$WORK/builds"
  mkdir -p "$DEBS" "$FAKEBIN"
  : >"$BUILDS"
  # A fake make: `build/release` makes the binary once (cargo leaves a current one alone), `package/deb/release`
  # makes the package, and each call is recorded.
  cat >"$WORK/make" <<FAKE
#!/bin/sh
WORK="$WORK"
echo "\$*" >>"$BUILDS"
case "\$1" in
  build/release) [ -e "$WORK/release/sven" ] || { mkdir -p "$WORK/release" && echo bin >"$WORK/release/sven"; } ;;
  package/deb/release) sleep 1; "$WORK/mkdeb" ;;
esac
FAKE
  chmod +x "$WORK/make"
  # A package holding /usr/bin/sven and a completion, like build-deb.sh's.
  cat >"$WORK/mkdeb" <<FAKE
#!/bin/sh
root="$WORK/pkgroot"
rm -rf "\$root"
mkdir -p "\$root/DEBIAN" "\$root/usr/bin" "\$root/usr/share/bash-completion/completions"
printf 'Package: sven\nVersion: 1.2.3\nArchitecture: all\nMaintainer: t <t@t>\nDescription: t\n' >"\$root/DEBIAN/control"
printf '#!/bin/sh\necho sven 1.2.3\n' >"\$root/usr/bin/sven"
chmod 755 "\$root/usr/bin/sven"
echo complete >"\$root/usr/share/bash-completion/completions/sven"
dpkg-deb --build --root-owner-group "\$root" "$DEBS/sven_1.2.3_all.deb" >/dev/null
FAKE
  chmod +x "$WORK/mkdeb"
  export PATH="$FAKEBIN:$PATH"
  fake_id 1000
}

fake_id() {
  printf '#!/bin/sh\necho %s\n' "$1" >"$FAKEBIN/id"
  chmod +x "$FAKEBIN/id"
}

run_install() {
  run bash "$SCRIPT" --deb-dir "$DEBS" --version 1.2.3 --binary "$WORK/release/sven" \
    --make "$WORK/make" --prefix "$PREFIX" "$@"
}

builds() { grep -c "^$1\$" "$BUILDS" || true; }

@test "builds the package, then installs it under the prefix without root" {
  run_install
  [ "$status" -eq 0 ]
  [ "$(builds package/deb/release)" -eq 1 ]
  [ -x "$PREFIX/bin/sven" ]
  [ "$("$PREFIX/bin/sven")" = "sven 1.2.3" ]
  [ -f "$PREFIX/share/bash-completion/completions/sven" ]
}

@test "a second run builds nothing and leaves the install as it was" {
  run_install
  before="$(stat -c %Y "$DEBS/sven_1.2.3_all.deb")"
  run_install
  [ "$status" -eq 0 ]
  [ "$(builds package/deb/release)" -eq 1 ]
  [ "$(stat -c %Y "$DEBS/sven_1.2.3_all.deb")" = "$before" ]
  [ "$("$PREFIX/bin/sven")" = "sven 1.2.3" ]
}

@test "a package older than the release binary is rebuilt" {
  run_install
  touch "$WORK/release/sven"
  run_install
  [ "$(builds package/deb/release)" -eq 2 ]
}

@test "warns when the prefix bin directory is not on PATH" {
  run_install
  [[ "$output" == *"not on your PATH"* ]]
}

@test "as root it installs the existing package with dpkg and builds nothing" {
  run_install
  : >"$BUILDS"
  fake_id 0
  printf '#!/bin/sh\necho "dpkg $*" >"%s/dpkg.log"\n' "$WORK" >"$FAKEBIN/dpkg"
  chmod +x "$FAKEBIN/dpkg"
  rm -rf "$PREFIX"
  run_install
  [ "$status" -eq 0 ]
  [ ! -s "$BUILDS" ]
  [[ "$(cat "$WORK/dpkg.log")" == "dpkg -i $DEBS/sven_1.2.3_all.deb" ]]
  [ ! -e "$PREFIX" ]
}

@test "as root with no package it fails and says to build it as a user" {
  fake_id 0
  run_install
  [ "$status" -ne 0 ]
  [[ "$output" == *"make install"* ]]
  [ ! -s "$BUILDS" ]
}
