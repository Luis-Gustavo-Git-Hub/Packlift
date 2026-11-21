# Packlift

Packlift is a GTK4 and libadwaita app for installing Linux packages and adding Windows games to your desktop menu. It checks a package's origin, architecture, and installation method before making changes, then tracks the programs it manages.

## Supported formats

| Format | Installation method |
| --- | --- |
| `.deb` | APT on Debian and Ubuntu. Other distributions use an Ubuntu 24.04 Toolbx container. |
| `.rpm` | DNF on Fedora and RHEL-based distributions, or Zypper on openSUSE. Other distributions use a Fedora Toolbx container. |
| `.pkg.tar.zst` | Pacman on Arch Linux or in an Arch Toolbx container. |
| `.AppImage` | Stored as an executable in Packlift's data directory. Type 2 AppImages run in extraction mode. |
| `.tar.gz`, `.tar.xz` | Extracted locally. Packlift asks you to choose when it finds multiple executables. |
| `.exe`, `.msi`, `.setup` | Run a Windows installer with Wine or Proton. Existing `.exe` games can be added directly to the applications menu. |

## Install Packlift

The [GitHub Actions builds](https://github.com/Luis-Gustavo-Git-Hub/packlift/actions) produce `Packlift-x86_64.flatpak`. Download the ZIP artifact from a completed build, extract it, and run:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub org.gnome.Platform//49
flatpak install --user ./Packlift-x86_64.flatpak
flatpak run io.github.flatapk.Installer
```

Open a package from your file manager with **Open With Packlift**, use **Open package**, or launch Packlift without a file to manage installed programs. Set Packlift as the default app for a file format if you want to open that format with a double-click.

If an AppImage shortcut does not open a window, use **Open** in Packlift's installed-program list and check **Show details** for its launch output.

## Windows games and programs

On x86-64, Packlift installs Wine WoW64 from Flathub for your account when a host Wine runtime is unavailable. The initial download requires an internet connection. Packlift keeps its shared prefix at `~/.local/share/flatapk/wine/prefix` with the default XDG data directory.

For a Windows `.exe`, choose **Install** to select between running a Windows setup program and adding an already-extracted game to the applications menu. The game shortcut launches the executable from its original folder, where its DLLs and other game files remain available. Packlift can use the Windows icon embedded in the executable. Removing a game shortcut leaves the game files in place.

Choose Wine or Proton in the package's advanced options. Use the gear button in Packlift to set a default for new Windows programs, or open a program's settings to change its runtime and launch environment. To use Proton, select its executable on the host. Some Proton distributions require Steam and a compatible host runtime.

Packlift checks for common Windows components and uses Winetricks when it finds a known dependency. Compressed installers and games may need manual configuration. Set per-program variables such as `WINEDLLOVERRIDES` from the program settings.

Close programs using the shared prefix before installing, uninstalling, or repairing. When possible, a failed installation restores the previous prefix. The latest Windows installation can be undone from the maintenance menu. Repairing the shared prefix re-runs saved installers and may require interaction; settings or documents inside the old prefix can be lost. Normal uninstallation uses the program's registered uninstaller when available and preserves the shared prefix.

## Linux containers and permissions

Native packages use the host distribution's package manager and request authorization through polkit. Toolbx and Podman must be installed on the host for container-based packages. Packlift reuses one container per distribution and forwards the graphical session, audio, and home directory to programs running inside it. On Fedora OSTree systems, RPM packages use Toolbx. Set `FLATAPK_UBUNTU_IMAGE`, `FLATAPK_FEDORA_IMAGE`, or `FLATAPK_ARCH_IMAGE` to override the default container images.

The Flatpak manifest lets Packlift run host commands through `flatpak-spawn --host`, access files in your home directory, and write application shortcuts. Programs launched through Toolbx or Wine can access your home directory. Install packages only from sources you trust.

## Package integrity

Packlift calculates the SHA-256 digest of the package prepared for installation and checks a neighboring `.sha256` file when present. RPM signatures are checked against keys trusted by the host's `rpmkeys`. For detached `.sig` or `.asc` signatures, place a GPG keyring containing keys you have verified at `~/.local/share/flatapk/trustedkeys.gpg` or the equivalent XDG path. Packlift warns when it cannot verify a package signature. Operation details and `flatapk.log` are stored in Packlift's data directory.

## Development

To run from source, install Python 3.11 or later, PyGObject with GTK 4.10 or later and libadwaita 1.4 or later, `bsdtar`/libarchive, and any tools for the package formats you want to use (`dpkg-deb`, `msiinfo`, `unsquashfs`, and `zstd`). Install gettext to compile translations.

```sh
make locale
python3 -m flatapk
python3 -m flatapk /path/to/package.deb
make test
```

To build the x86-64 Flatpak locally, add Flathub and run:

```sh
flatpak-builder --user --install-deps-from=flathub --repo=flatpak-repo --force-clean \
  flatpak-build io.github.flatapk.Installer.json
flatpak build-bundle flatpak-repo Packlift-x86_64.flatpak io.github.flatapk.Installer
```

The GNOME runtime in this manifest targets x86-64. Running native code on a 32-bit x86 host depends on the availability of GTK, Wine, and the relevant package manager. The current Fedora and Arch Toolbx images require a 64-bit host. On x86-64, i386 packages use APT multiarch support, while i686 RPM packages depend on libraries provided by the distribution. Packlift rejects special files and archive links that point outside the program directory.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
