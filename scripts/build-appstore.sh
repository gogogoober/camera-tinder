#!/usr/bin/env bash
# Build Darkroom for the Mac App Store: a sandboxed, signed .app wrapped in a signed .pkg ready to upload.
# See docs/app-store.md for the one-time setup (certificates, App ID, provisioning profile).
#
#   APPLE_TEAM_ID=ABCDE12345 scripts/build-appstore.sh
#
# Environment:
#   APPLE_TEAM_ID         required (unless LOCAL_CHECK=1); your 10-character team id
#   APPLE_SIGNING_IDENTITY   app signing cert, default "Apple Distribution"
#   INSTALLER_IDENTITY       pkg signing cert, default "3rd Party Mac Developer Installer"
#   BUILD_NUMBER          CFBundleVersion; must go up with every upload. Default: yymmdd.HHMM
#   TARGET                default universal-apple-darwin (Intel + Apple Silicon); or aarch64-apple-darwin
#   LOCAL_CHECK=1         no certificates needed: ad-hoc signed, no provisioning profile, no .pkg.
#                         Only for checking the bundle (entitlements, Info.plist, privacy manifest).
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

LOCAL_CHECK="${LOCAL_CHECK:-0}"
TARGET="${TARGET:-universal-apple-darwin}"
BUILD_NUMBER="${BUILD_NUMBER:-$(date +%y%m%d.%H%M)}"
APPLE_SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-Apple Distribution}"
INSTALLER_IDENTITY="${INSTALLER_IDENTITY:-3rd Party Mac Developer Installer}"
STORE=src-tauri/appstore
BUILD="$STORE/.build"
IDENTIFIER=$(node -p "require('./src-tauri/tauri.conf.json').identifier")
PRODUCT=$(node -p "require('./src-tauri/tauri.conf.json').productName")

die() { echo "error: $*" >&2; exit 1; }

# Rust targets for the architectures being built.
case "$TARGET" in
  universal-apple-darwin) needed="aarch64-apple-darwin x86_64-apple-darwin" ;;
  *) needed="$TARGET" ;;
esac
for t in $needed; do
  rustup target list --installed | grep -qx "$t" || die "missing Rust target $t: rustup target add $t"
done

# Entitlements: the committed set + the two ids App Store builds need (Xcode normally adds them).
rm -rf "$BUILD" && mkdir -p "$BUILD"
cp "$STORE/entitlements.plist" "$BUILD/entitlements.plist"
overrides="{\"bundle\":{\"macOS\":{\"bundleVersion\":\"$BUILD_NUMBER\""
if [ "$LOCAL_CHECK" = 1 ]; then
  export APPLE_SIGNING_IDENTITY="-"   # ad-hoc
  overrides="$overrides,\"files\":{\"embedded.provisionprofile\":null}"
else
  [ -n "${APPLE_TEAM_ID:-}" ] || die "set APPLE_TEAM_ID (developer.apple.com → Membership)"
  [ -f "$STORE/embedded.provisionprofile" ] || die "put your Mac App Store provisioning profile at $STORE/embedded.provisionprofile"
  plutil -insert com.apple.application-identifier -string "$APPLE_TEAM_ID.$IDENTIFIER" "$BUILD/entitlements.plist"
  plutil -insert com.apple.developer.team-identifier -string "$APPLE_TEAM_ID" "$BUILD/entitlements.plist"
  export APPLE_SIGNING_IDENTITY
fi
overrides="$overrides}}}"

# App Store builds aren't notarized (Apple signs them); Tauri notarizes on its own when these are set.
unset APPLE_ID APPLE_PASSWORD APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH

echo "Building $PRODUCT ($IDENTIFIER) build $BUILD_NUMBER for $TARGET…"
npx tauri build --target "$TARGET" --config src-tauri/tauri.appstore.conf.json --config "$overrides"

APP="target/$TARGET/release/bundle/macos/$PRODUCT.app"
[ -d "$APP" ] || die "no app bundle at $APP"
codesign --verify --strict --verbose=1 "$APP"
echo "Entitlements:"; codesign -d --entitlements - --xml "$APP" | plutil -p -

if [ "$LOCAL_CHECK" = 1 ]; then
  echo "Local check done: $APP (ad-hoc signed; not uploadable)."
  exit 0
fi

PKG="target/$TARGET/release/bundle/$PRODUCT-$BUILD_NUMBER.pkg"
productbuild --sign "$INSTALLER_IDENTITY" --component "$APP" /Applications "$PKG"
echo
echo "Ready to upload: $PKG"
echo "Upload with the Transporter app, or:"
echo "  xcrun altool --upload-app --type macos --file \"$PKG\" --apiKey <key id> --apiIssuer <issuer id>"
