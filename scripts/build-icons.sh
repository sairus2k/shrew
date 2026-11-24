#!/bin/sh
# Renders the icon sources in assets/icons into the files the app ships:
# Shrew.icns for the Dock, Shrew.ico for the Windows binary and the template
# PNGs for the menu bar.
# Needs rsvg-convert (brew install librsvg), magick (brew install imagemagick)
# and iconutil from macOS.
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

ico=$(mktemp -d)
for size in 16 24 32 48 64 256; do
    rsvg-convert -w "$size" -h "$size" app-icon.svg -o "$ico/$size.png"
done
magick "$ico/16.png" "$ico/24.png" "$ico/32.png" "$ico/48.png" "$ico/64.png" "$ico/256.png" Shrew.ico
rm -r "$ico"

# The suffix marks the image as a template for AppKit.
rsvg-convert -w 18 -h 18 menu-bar-icon.svg -o menuBarIconTemplate.png
rsvg-convert -w 36 -h 36 menu-bar-icon.svg -o menuBarIconTemplate@2x.png
