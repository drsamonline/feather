# macOS

Feather is not bundled for macOS yet, and this file says so rather than
pretending otherwise.

A `.app` needs three things this repository does not produce yet:

1. **A bundle** — `feather-viewer` inside `Feather.app/Contents/MacOS/`, with an
   `Info.plist`. `cargo-bundle` does this in one command and is the intended
   route; it is a build dependency, not a runtime one.
2. **Document types** — `CFBundleDocumentTypes` in the `Info.plist`, one entry
   per format family with `CFBundleTypeRole = Viewer`. That is what puts Feather
   in Finder's "Open With" menu.
3. **Notarisation** — without it, Gatekeeper refuses the first launch with a
   warning most people will not read past. That needs an Apple Developer ID.

The `Info.plist` fragment the bundle needs:

```xml
<key>CFBundleDocumentTypes</key>
<array>
  <dict>
    <key>CFBundleTypeName</key><string>Document</string>
    <key>CFBundleTypeRole</key><string>Viewer</string>
    <key>LSHandlerRank</key><string>Alternate</string>
    <key>CFBundleTypeExtensions</key>
    <array>
      <string>pdf</string><string>epub</string><string>cbz</string><string>cbr</string>
      <string>png</string><string>jpg</string><string>jpeg</string><string>gif</string>
      <string>webp</string><string>bmp</string><string>tif</string><string>tiff</string>
    </array>
  </dict>
</array>
```

`LSHandlerRank` is `Alternate` on purpose: Feather offers itself as a viewer
without claiming to own the format.
