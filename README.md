# Packlift

Packlift is a GTK4 and libadwaita app for installing Linux packages and adding Windows games to your desktop menu. It checks a package's origin, architecture, and installation method before making changes, then tracks the programs it manages.

## Supported formats

| Format | Installation method |
| --- | --- |
| `.deb` | APT on Debian and Ubuntu. Other distributions use an Ubuntu 24.04 Toolbx container. |
| `.rpm` | DNF on Fedora and RHEL-based distributions, or Zypper on openSUSE. Other distributions use a Fedora Toolbx container. |
| `.pkg.tar.zst` | Pacman on Arch Linux or in an Arch Toolbx container. |
| `.AppImage` | Stored as an executable in Packlift's data directory. Type 2 AppImages run in extraction mode. |
| `.tar.gz`, `.tar.xz` | Extracted locally and launched with a compatible executable. |
| `.exe`, `.msi`, `.setup` | Run a Windows installer with Wine or Proton. Existing `.exe` games can be added directly to the applications menu. |

## Install Packlift

The [GitHub Actions builds](https://github.com/Luis-Gustavo-Git-Hub/packlift/actions) produce `Packlift-x86_64.flatpak`. Download the ZIP artifact from a completed build, extract it, and run:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub org.gnome.Platform//49
flatpak install --user ./Packlift-x86_64.flatpak
flatpak run io.github.flatapk.Installer
```

Open a package from your file manager with **Open With Packlift**, use **Open package**, or launch Packlift without a file to manage installed programs. In Packlift settings, choose **Set as default for supported files** to open compatible files with a double-click. This updates the file associations for your Linux account. Opening a package still takes you to the review screen before installation or launch.

If an AppImage shortcut does not open a window, use **Open** in Packlift's installed-program list and check **Show details** for its launch output.

## Windows games and programs

On x86-64, Packlift installs Wine WoW64 from Flathub for your account when a host Wine runtime is unavailable. The first Wine or Proton setup requires an internet connection. Choose **Proton (automatic install)** and Packlift uses [UMU Launcher](https://github.com/Open-Wine-Components/umu-launcher) to download UMU-Proton and its Steam Linux Runtime automatically while preparing a game. Proton can run games outside Steam and does not require Steam to be installed. Packlift keeps its shared prefix at `~/.local/share/flatapk/wine/prefix` and UMU's Proton files under `~/.local/share/flatapk/umu` with the default XDG data directory.

For a Windows `.exe`, choose **Install** to either install a program or game through its setup program, or add an already installed game to Packlift and the applications menu. The game shortcut launches the executable from its original folder, where its DLLs and other game files remain available. Packlift can use the Windows icon embedded in the executable. Removing a game shortcut leaves the game files in place.

Choose Wine or Proton in the package's advanced options. Use the gear button in Packlift to set a default for new Windows programs, or open a program's settings to change its runtime and launch environment. UMU-Proton is installed on first use and runs games outside Steam.

Packlift checks for common Windows components and uses Winetricks when it finds a known dependency. Compressed installers and games may need manual configuration. Set per-program variables such as `WINEDLLOVERRIDES` from the program settings.

Close programs using the shared prefix before installing, uninstalling, or repairing. When possible, a failed installation restores the previous prefix. The latest Windows installation can be undone from the maintenance menu. Repairing the shared prefix re-runs saved installers and may require interaction; settings or documents inside the old prefix can be lost. Normal uninstallation uses the program's registered uninstaller when available and preserves the shared prefix.

## Linux containers and permissions

Native packages use the host distribution's package manager and request authorization through polkit. Toolbx and Podman must be installed on the host for container-based packages. Packlift reuses one container per distribution and forwards the graphical session, audio, and home directory to programs running inside it. On Fedora OSTree systems, RPM packages use Toolbx. Set `FLATAPK_UBUNTU_IMAGE`, `FLATAPK_FEDORA_IMAGE`, or `FLATAPK_ARCH_IMAGE` to override the default container images.

In the GitHub Flatpak build, choose **Enable access to your system…** and copy the command shown in the dialog. Run it in your terminal as your user and reopen Packlift. This grants Packlift permission to run host commands with your account's access for Toolbx, native package management, and Wine or Proton. Programs launched through Toolbx or Wine can access your home directory. Install packages only from sources you trust.

## Package integrity

Packlift calculates the SHA-256 digest of the package prepared for installation and checks a neighboring `.sha256` file when present. RPM signatures are checked against keys trusted by the host's `rpmkeys`. For detached `.sig` or `.asc` signatures, place a GPG keyring containing keys you have verified at `~/.local/share/flatapk/trustedkeys.gpg` or the equivalent XDG path. Packlift warns when it cannot verify a package signature. Operation details and `flatapk.log` are stored in Packlift's data directory.

## Development

Packlift's application and installer are written in Rust. To build locally, install Rust 1.85 or later, GTK 4.10 or later, libadwaita 1.4 or later, and the package inspection tools used by the formats you want to work with.

```sh
cargo build --locked --features gui
cargo run --locked --features gui
cargo run --locked --features gui -- /path/to/package.deb
```

All application and CI integration code is written in Rust. `make test` runs the Rust library test target. The Flatpak build uses the GNOME SDK Rust extension and the checked-in Cargo lockfile and crate source list.

To build the x86-64 Flatpak locally, add Flathub and run:

```sh
flatpak-builder --user --install-deps-from=flathub --repo=flatpak-repo --force-clean \
  flatpak-build io.github.flatapk.Installer.json
flatpak build-bundle flatpak-repo Packlift-x86_64.flatpak io.github.flatapk.Installer
```

The Flatpak and the managed Toolbx images are intended for x86-64 hosts. The **Find and install missing libraries in the container** option can resolve missing shared libraries detected in a launcher through the container's configured repositories. **Repair…** retries that process on an existing container installation. Packages that need a different distribution release or an unavailable library require a compatible build.

| Toolbx image | 32-bit package and library support |
| --- | --- |
| Ubuntu | APT enables i386 alongside amd64. Installs i386 DEBs and available i386 libraries. |
| Fedora | DNF resolves i686 RPMs and libraries published for the image release. |
| openSUSE | Zypper uses the distribution's available `-32bit` libraries. Standalone 32-bit RPMs depend on what the image's repositories provide. |
| Arch Linux | Pacman enables the official multilib repository for `lib32-*` packages. Standalone i686 Arch packages require a separate Arch Linux 32 environment. |

Packlift rejects special files and archive links that point outside the program directory.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
