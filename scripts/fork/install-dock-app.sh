#!/bin/sh
# Installs (or updates) ~/Applications/FigCraft.app, the Dock launcher for scripts/fork/app.sh
# (source: scripts/fork/launcher.swift), and adds it to the Dock once. Re-run after changing the
# launcher. macOS only; needs the Xcode command line tools (swiftc).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
app="$HOME/Applications/FigCraft.app"

mkdir -p "$HOME/Applications"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
swiftc -O -o "$app/Contents/MacOS/FigCraft" "$root/scripts/fork/launcher.swift" -framework AppKit
if [ -f /Applications/FigCraft.app/Contents/Resources/FigCraft.icns ]; then
    cp /Applications/FigCraft.app/Contents/Resources/FigCraft.icns "$app/Contents/Resources/FigCraft.icns"
else
    set_dir=$(mktemp -d)/FigCraft.iconset; mkdir -p "$set_dir"
    for s in 16 32 128 256 512; do sips -z $s $s "$root/assets/app-icon/figcraft-1024.png" --out "$set_dir/icon_${s}x${s}.png" >/dev/null; done
    iconutil -c icns "$set_dir" -o "$app/Contents/Resources/FigCraft.icns"
fi
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>FigCraft</string>
    <key>CFBundleDisplayName</key><string>FigCraft</string>
    <key>CFBundleIdentifier</key><string>com.eliasx45.figcraft.launcher</string>
    <key>CFBundleExecutable</key><string>FigCraft</string>
    <key>CFBundleIconFile</key><string>FigCraft</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>1.0</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>FigCraftRoot</key><string>$root</string>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>FigCraft Document</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>CFBundleTypeIconFile</key><string>FigCraft</string>
            <key>LSItemContentTypes</key><array><string>ai.storyteller.figcraft.document</string></array>
            <key>LSHandlerRank</key><string>Owner</string>
        </dict>
        <dict>
            <key>CFBundleTypeName</key><string>SVG Image</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>LSItemContentTypes</key><array><string>public.svg-image</string></array>
            <key>LSHandlerRank</key><string>Alternate</string>
        </dict>
        <dict>
            <key>CFBundleTypeName</key><string>Image</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSItemContentTypes</key><array><string>public.png</string><string>public.jpeg</string></array>
            <key>LSHandlerRank</key><string>Alternate</string>
        </dict>
    </array>
    <key>UTImportedTypeDeclarations</key>
    <array>
        <dict>
            <key>UTTypeIdentifier</key><string>ai.storyteller.figcraft.document</string>
            <key>UTTypeDescription</key><string>FigCraft Document</string>
            <key>UTTypeConformsTo</key><array><string>public.data</string><string>public.content</string></array>
            <key>UTTypeIconFile</key><string>FigCraft</string>
            <key>UTTypeTagSpecification</key>
            <dict><key>public.filename-extension</key><array><string>figcraft</string></array></dict>
        </dict>
    </array>
</dict>
</plist>
EOF
codesign --force --sign - "$app" >/dev/null 2>&1 || true
touch "$app"

# Register it and make it the app for .figcraft files (Finder double-click, Open With): it opens
# them in the FigCraft built from this checkout. An installed release in /Applications declares the
# same type and would otherwise keep getting them.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app"
default_swift="$(mktemp -d)/default.swift"
cat >"$default_swift" <<'SWIFT'
import AppKit
import UniformTypeIdentifiers
let app = URL(fileURLWithPath: CommandLine.arguments[1])
let type = UTType("ai.storyteller.figcraft.document")!
let done = DispatchSemaphore(value: 0)
var failed = false
NSWorkspace.shared.setDefaultApplication(at: app, toOpen: type) { error in
    failed = error != nil
    done.signal()
}
while done.wait(timeout: .now()) == .timedOut { RunLoop.main.run(until: Date().addingTimeInterval(0.05)) }
exit(failed ? 1 : 0)
SWIFT
swift "$default_swift" "$app" || echo "couldn't make FigCraft.app the default for .figcraft files (Finder ▸ Get Info ▸ Open with ▸ Change All)"

# Pin it to the Dock once.
if ! defaults read com.apple.dock persistent-apps 2>/dev/null | grep -q "Applications/FigCraft.app"; then
    defaults write com.apple.dock persistent-apps -array-add \
        "<dict><key>tile-data</key><dict><key>file-data</key><dict><key>_CFURLString</key><string>file://$app/</string><key>_CFURLStringType</key><integer>15</integer></dict></dict></dict>"
    killall Dock
fi
echo "installed $app"
