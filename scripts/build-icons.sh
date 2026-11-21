#!/bin/sh
# Renders the icon sources in assets/icons into the files the app ships:
# Shrew.icns for the Dock and the template PNGs for the menu bar.
# Needs rsvg-convert (brew install librsvg) and iconutil from macOS.
set -eu

cd "$(dirname "$0")/../assets/icons"

iconset=$(mktemp -d)/Shrew.iconset
mkdir "$iconset"
for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" app-icon.svg -o "$iconset/icon_${size}x${size}.png"
    rsvg-convert -w $((size * 2)) -h $((size * 2)) app-icon.svg -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output Shrew.icns "$iconset"
rm -r "$(dirname "$iconset")"

# The suffix marks the image as a template for AppKit.
rsvg-convert -w 18 -h 18 menu-bar-icon.svg -o menuBarIconTemplate.png
rsvg-convert -w 36 -h 36 menu-bar-icon.svg -o menuBarIconTemplate@2x.png
