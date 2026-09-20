#!/usr/bin/env bash
#
# Build `dist/rustmd.app` — a real macOS application bundle.
#
# Why a bundle at all: a bare binary cannot be launched from Finder, cannot be
# found by Spotlight, and — the part that matters here — cannot be registered as
# something that opens `.md` files. macOS only offers applications it knows
# about in the "打开方式" menu, and it only knows about bundles.
#
# Once built, `dist/rustmd.app`:
#   * launches by double-click, with a Dock icon and a proper app menu;
#   * appears under "打开方式" for `.md`, `.markdown`, `.mdown`, `.mkd`;
#   * receives the double-clicked file through an Apple Event, which
#     `src/macos.rs` turns back into a path.
#
# It is declared with `LSHandlerRank = Alternate`, so installing it does not
# steal the default handler for Markdown from whatever the user already uses.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/dist/rustmd.app"
BIN="$ROOT/target/release/rustmd"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "make-app.sh 只适用于 macOS（当前系统：$(uname -s)）" >&2
  exit 1
fi

VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
VERSION="${VERSION:-0.1.0}"
IDENTIFIER="dev.rustmd.app"

echo "==> 编译 release"
( cd "$ROOT" && cargo build --release )
if [[ ! -x "$BIN" ]]; then
  echo "找不到可执行文件 $BIN" >&2
  exit 1
fi

echo "==> 组装 ${APP#$ROOT/}"
# Deliberately no `rm -rf "$APP"` here. Deletions under a sandbox are diverted to
# ~/.Trash, and every trashed copy of a bundle stays registered with Launch
# Services under the same bundle identifier — after a few builds Finder starts
# offering a handful of identically-named apps whose paths no longer exist.
# Overwriting in place leaves one registration and one bundle, which is what we
# want anyway. (Cost: a file removed from this script's output is not cleaned
# up. There are three of them, so it has not been worth a loop.)
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 755 "$BIN" "$APP/Contents/MacOS/rustmd"

# Icon. `assets/rustmd.icns` is committed, so a normal build needs nothing extra;
# regenerating it needs Python with cairosvg, which is not something a Rust
# build should insist on — hence the attempt-then-warn.
ICNS="$ROOT/assets/rustmd.icns"
if [[ ! -f "$ICNS" ]]; then
  echo "==> 图标缺失，尝试生成 ${ICNS#$ROOT/}"
  if ! "${RUSTMD_PYTHON:-python3}" "$ROOT/scripts/make-icon.py"; then
    echo "    警告：图标生成失败，将打包成无图标的 app" >&2
    echo "    可以手动跑：scripts/make-icon.py" >&2
  fi
fi
if [[ -f "$ICNS" ]]; then
  cp "$ICNS" "$APP/Contents/Resources/rustmd.icns"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>rustmd</string>
    <key>CFBundleDisplayName</key>
    <string>rustmd</string>
    <key>CFBundleExecutable</key>
    <string>rustmd</string>
    <key>CFBundleIdentifier</key>
    <string>${IDENTIFIER}</string>
    <key>CFBundleIconFile</key>
    <string>rustmd</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key>
            <string>Markdown Document</string>
            <key>CFBundleTypeRole</key>
            <string>Editor</string>
            <key>LSHandlerRank</key>
            <string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>net.daringfireball.markdown</string>
                <string>public.plain-text</string>
            </array>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>md</string>
                <string>markdown</string>
                <string>mdown</string>
                <string>mkd</string>
                <string>mdwn</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
PLIST

# Refresh Launch Services so Finder picks the bundle up immediately.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
  -f "$APP" >/dev/null 2>&1 || true

echo "==> 完成：$APP"
echo "    启动：open -a '$APP'"
echo "    打开：open -a '$APP' 某个文件.md"
