# `gtk-transcription`

## Building the project

Make sure you have `flatpak` and `flatpak-builder` installed. Then run the commands below. Please note that these commands are just for demonstration purposes. Normally this would be handled by your IDE, such as GNOME Builder or VS Code with the Flatpak extension.

```sh
flatpak install --user org.gnome.Sdk//43 org.freedesktop.Sdk.Extension.rust-stable//22.08 org.gnome.Platform//43 org.freedesktop.Sdk.Extension.llvm14//22.08
flatpak-builder --user flatpak_app build-aux/dev.mnts.Transcription.Devel.json
```

## Running the project

```sh
flatpak-builder --run flatpak_app build-aux/dev.mnts.Transcription.Devel.json Transcription
```

### Contributing

### Generating `po` and `pot` files.

```sh
meson build build/
cd build
meson compile gtk-transcription-update-po
```
