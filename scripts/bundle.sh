#!/usr/bin/env bash
# Build coypa.app — a proper macOS bundle.
#
# Bundling matters beyond tidiness: macOS ties Accessibility permission to an
# app's code-signing identity. Running the bare binary attaches the grant to
# your *terminal*; a signed bundle gets its own stable identity, so the grant
# sticks to coypa and survives rebuilds.
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="coypa"
BUNDLE="dist/${APP_NAME}.app"
CONTENTS="${BUNDLE}/Contents"

echo "==> building release binary"
cargo build --release

echo "==> assembling ${BUNDLE}"
rm -rf "${BUNDLE}"
mkdir -p "${CONTENTS}/MacOS" "${CONTENTS}/Resources"

cp "target/release/${APP_NAME}" "${CONTENTS}/MacOS/${APP_NAME}"
cp packaging/Info.plist "${CONTENTS}/Info.plist"
# Inter is embedded in the binary; ship its license to satisfy the SIL OFL.
cp assets/OFL-Inter.txt "${CONTENTS}/Resources/OFL-Inter.txt"

if [[ ! -f assets/AppIcon.icns && -f assets/icon.png ]]; then
  echo "==> generating icon"
  ./scripts/make-icon.sh >/dev/null
fi
if [[ -f assets/AppIcon.icns ]]; then
  cp assets/AppIcon.icns "${CONTENTS}/Resources/AppIcon.icns"
else
  echo "==> note: no icon (add assets/icon.png)"
fi

# SDL3 is linked dynamically from Homebrew. Vendor it into the bundle so the
# app runs on machines without Homebrew.
SDL_LIB="$(otool -L "${CONTENTS}/MacOS/${APP_NAME}" | awk '/libSDL3/ {print $1; exit}')"
if [[ -n "${SDL_LIB}" && "${SDL_LIB}" != @rpath/* && -f "${SDL_LIB}" ]]; then
  echo "==> vendoring $(basename "${SDL_LIB}")"
  mkdir -p "${CONTENTS}/Frameworks"
  cp "${SDL_LIB}" "${CONTENTS}/Frameworks/"
  chmod u+w "${CONTENTS}/Frameworks/$(basename "${SDL_LIB}")"
  install_name_tool -change "${SDL_LIB}" \
    "@executable_path/../Frameworks/$(basename "${SDL_LIB}")" \
    "${CONTENTS}/MacOS/${APP_NAME}"
else
  echo "==> note: SDL3 not vendored (${SDL_LIB:-not found}); app will need it installed"
fi

echo "==> signing (ad-hoc)"
codesign --force --deep --sign - "${BUNDLE}"

echo
echo "built ${BUNDLE}"
echo
echo "next:"
echo "  open dist/            # drag coypa.app to /Applications"
echo "  then grant Accessibility:"
echo "  System Settings > Privacy & Security > Accessibility > +  (add coypa.app)"
