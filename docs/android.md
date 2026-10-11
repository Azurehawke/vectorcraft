# VectorCraft on Android (experimental)

VectorCraft's desktop interface on Android tablets, for use with a mouse, a keyboard and a stylus.
Built from the `android` branch by `.github/workflows/android.yml` for arm64 devices running
Android 11 or later, and published as the `android-latest` prerelease (`vectorcraft.apk`).

## Install

1. Download `vectorcraft.apk` from the `android-latest` prerelease on the tablet and open it
   (allow installs from your browser or Files app; Play Protect: *More details ▸ Install anyway*).
2. Give VectorCraft file access: the first file dialog shows **Allow access…**, or
   **Settings ▸ Apps ▸ ⋮ ▸ Special access ▸ All files access ▸ VectorCraft**.

## How it differs from the desktop app

- **Files:** Open, Save As, Place, Export and the other file dialogs are a file browser drawn
  inside VectorCraft (shared storage, Documents, Download, Pictures, SD cards and USB drives).
- **Mouse:** hover, right-click menus, the middle button and the wheel work as on a desktop
  (vendor/winit). The S Pen hovering over the screen moves the pointer too.
- **S Pen and fingers:** the S Pen draws with pressure; a palm resting on the screen is ignored.
  One finger on the canvas scrolls it; two fingers pinch-zoom, pan, and tap to undo (three: redo).
  Fingers still tap buttons and panels.
- **Keyboard:** with a keyboard attached, text fields don't bring up the on-screen keyboard.
- Preferences, fonts you add, libraries and Data Recovery live in the app's private folder;
  uninstalling removes them. The tablet's own fonts are available.
- **Not on Android:** the system clipboard for artwork (copy and paste work inside VectorCraft),
  printing, Show in Folder, and the control channel.
