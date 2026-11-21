# Packlift: cross-distro installation and accessibility PRD

**Specification complete · 2026-11-21**
Implementation and distribution compatibility await validation.
**Scope:** package installation, container prerequisites, dependency failures, and the GTK interface

## Problem

Installing a package built for a different Linux distribution currently depends on host tools that may not be installed. Packlift asks for Toolbx and Podman only after the user starts the install, then stops with an error. Package-manager failures, missing runtime libraries, and accessibility gaps also do not consistently give the user a clear recovery path.

## Goals

1. Detect and prepare the host container prerequisites before changing the selected package environment.
2. Install a foreign-distro package and its declared dependencies in the matching managed environment, without installing that package into the host OS.
3. Explain and recover from missing dependencies, unsupported package formats, permission denials, network errors, and immutable-host limitations.
4. Make every Packlift flow usable with a keyboard and screen reader, and keep progress and errors understandable without relying on color or animation.
5. Keep the existing native install path for packages that match the host and preserve explicit polkit authorization for host changes.

## Non-goals

- Guarantee compatibility for every Linux package or proprietary application.
- Install host packages without the user's authorization.
- Bypass an immutable distribution's reboot requirement when its package layering mechanism requires a reboot.
- Run untrusted packages with stronger isolation than the host container and Flatpak permissions provide.

## User flows

### Foreign-distro package

1. Packlift identifies the package format, distro family, architecture, declared dependencies, and install plan.
2. Before installation, Packlift checks container engine, Toolbx, network, free disk space, and permissions.
3. If Toolbx or Podman is missing, Packlift explains which host component is missing and offers **Set up container support**. The host package manager runs through polkit, showing the normal system authorization prompt. The user can cancel without modifying the package or host.
4. Packlift verifies the tools after setup, creates or reuses a distro-family container, and installs the package through that container's package manager.
5. Packlift validates exported commands, libraries, icons, and desktop entries before reporting success.
6. If setup cannot proceed (unsupported host, no authorization agent, immutable host requiring restart, or unavailable package), Packlift gives a specific next step and a copyable diagnostic log.

### Dependency or launch failure

1. Installation output records package-manager diagnostics and the resolved dependency transaction.
2. A failed install leaves no registered application or broken menu shortcut; Packlift cleans temporary state where safe.
3. A failed launch keeps the program registered, shows a plain-language summary, and offers **Copy log** and **Open log location**.
4. The error identifies the failed stage and whether the likely cause is a missing host prerequisite, missing package dependency, unsupported architecture, or application incompatibility.

## Functional requirements

### Host prerequisites and container setup

- **FR-01** Check for `toolbox`, `podman`, and an authorization agent before preparing a foreign-distro install.
- **FR-02** When `toolbox` or `podman` is absent, offer one explicit setup action using the detected host package manager and a polkit prompt. Never run a privileged command silently.
- **FR-03** Support a verified host mapping for Fedora/RHEL, Debian/Ubuntu, openSUSE, and Arch. Unknown hosts must stop before mutation and show a useful diagnostic.
- **FR-04** Detect OSTree/immutable hosts. Use their supported layering command only with authorization and clearly state when a restart is required; do not report setup as complete until the tools are callable.
- **FR-05** Recheck tool availability, versions, user namespaces, storage, network, and image access after setup. Provide the exact failing check in the log.
- **FR-06** Reuse a Packlift-owned container per supported package family. Never adopt or delete an unrelated container with a colliding name.
- **FR-07** Record container backend, image, distro release, package manager, and package transaction in the app registry and installation log.

### Package dependency resolution

- **FR-08** Install foreign packages only inside their matching container family: DEB with APT, RPM with DNF or Zypper according to the package family, and Arch packages with Pacman.
- **FR-09** Resolve dependencies from configured repositories before committing the install. Show the package count and approximate download size when the package manager provides them.
- **FR-10** Handle 32-bit dependencies explicitly, including enabling Debian's i386 architecture only inside the managed environment and installing the matching multiarch packages.
- **FR-11** Detect unsupported, unavailable, or conflicting dependencies from the package-manager exit status and logs. Do not convert a failed dependency resolution into a success record.
- **FR-12** Before menu export, validate that the executable exists and that required shared libraries resolve inside the target environment. Record unresolved libraries and suggest the relevant package-manager recovery action.
- **FR-13** If launch fails because of a missing shared library, allow **Repair environment** to retry dependency resolution in the container, with a confirmation before changing installed packages.
- **FR-14** Keep native host package installation separate from foreign-distro installation; require polkit authorization for every host mutation.

### Accessibility and interface

- **A11Y-01** All actions are keyboard reachable with a visible focus indicator and a logical tab order.
- **A11Y-02** Icon-only controls have accessible names; all inputs, status labels, progress indicators, and dialogs expose labels and roles to AT-SPI.
- **A11Y-03** Announce progress stage changes to screen readers. A spinner is never the only indication of activity.
- **A11Y-04** Success, warning, and error states include text and iconography; meaning is never conveyed by color alone.
- **A11Y-05** Respect system text scaling and high-contrast settings. Long names, paths, and errors wrap or scroll without hiding primary actions.
- **A11Y-06** Confirmation, progress, and result windows retain a predictable title, default action, cancel/close action, and keyboard behavior.
- **A11Y-07** Copy-log controls confirm accessibly that the log was copied and remain available when an operation fails.

## Failure handling

| Failure | Required response |
| --- | --- |
| Toolbx/Podman absent | Offer authorized setup; recheck; explain unsupported or restart-required hosts |
| No polkit agent / authorization denied | Cancel cleanly; preserve package and registry; show copyable log |
| Image pull or repository network failure | Show failed image/repository and retry action; no partial shortcut |
| Unsupported host/package architecture | Reject before container creation and name the supported architectures |
| Missing package dependency | Show package-manager diagnostic; do not register the app; offer retry after repository correction |
| Missing runtime library after install | Show unresolved library names and offer repair in the managed environment |
| Interrupted install | Keep a recoverable transaction record; remove only files created by that transaction |
| Icon/desktop export failure | Keep the installed program registered only if it can be opened from Packlift; provide a repair-shortcut action |
| Immutable host needs reboot | Report the exact layered packages and restart requirement; do not claim readiness before reboot |

## Acceptance criteria

1. On a supported mutable Fedora host with neither Toolbx nor Podman, choosing a foreign package offers an authorized setup, installs prerequisites, verifies them, and continues without a terminal command from the user.
2. On supported Debian/Ubuntu, openSUSE, and Arch hosts, setup uses the host's correct package IDs and fails safely when those packages are unavailable.
3. On an immutable host, the app either verifies the tools already present or explains the layering/restart step without leaving a false success state.
4. A foreign DEB, RPM, and Arch package install only inside the matching environment; missing dependencies are visible, logged, and do not produce broken shortcuts.
5. Keyboard-only users can inspect a package, choose a runtime, start/cancel an installation, open/copy logs, launch an app, and remove an entry.
6. Orca can identify every icon-only control and receives each progress-stage update.
7. Existing installed Wine/Proton games remain registered after a repair attempt and get corrected desktop shortcuts when metadata can be recovered.

## Delivery plan

1. **Prerequisite diagnosis and setup:** host detection, package IDs, polkit flow, immutable-host handling, verification, and structured failure codes.
2. **Container and package reliability:** container ownership, supported image matrix, dependency transaction checks, multiarch, runtime-library diagnostics, repair and rollback.
3. **Accessibility pass:** accessible names, live status announcements, keyboard order, scalable layout, and AT-SPI verification.
4. **Compatibility validation:** CI matrix for package metadata and desktop entries plus manual tests on Fedora, Ubuntu, Arch, openSUSE, and an OSTree host.

## Out of scope for automatic repair

Kernel modules, proprietary drivers, unsupported CPU architectures, packages whose dependencies do not exist in enabled repositories, applications requiring a different ABI than the chosen base image, and programs that are themselves incompatible with the container/runtime. In these cases, Packlift must explain the limitation and provide a useful log rather than promise compatibility.

## Installation decisions and recovery

Read distro and boot information on the host when Packlift runs as a Flatpak. Check both tools independently. If both work, continue without requesting administrative privileges. If either is missing, list the host packages and any restart requirement before offering setup. Cancel returns to package details. After a restart, reopen the package, inspect host state again, and continue only if rootless Podman and Toolbx work for the signed-in user.

Use the package metadata, target release and architecture to choose an image. An RPM extension alone cannot distinguish Fedora from openSUSE. If metadata is ambiguous, ask the user to choose among supported targets before installing. Identify the chosen target on the confirmation screen. Never fall back to a native install after a container failure.

Refresh repository metadata inside the target environment and resolve the local package with its package manager. For a missing dependency, show its name and required version when available, the selected distro release, and the repository error. Offer retry after the user fixes repository access, or return to target selection before committing. Keep signature checks enabled. Never automatically add third-party repositories or force conflicting package versions.

Record each install stage before starting it. On retry after an interruption, query the actual installed package and version. If the package transaction succeeded but shortcut export failed, offer to recreate just the shortcut. If the manager holds a transaction lock, explain that the user can retry when it finishes. Preserve other packages and containers during recovery. Distinguish packages with no graphical launcher from apps whose shortcut export failed.

## Release verification

| Scenario | Observable acceptance result |
| --- | --- |
| Clean supported host, both tools missing, then only one missing | Setup lists required host changes, honors cancellation and continues after successful authorization and recheck. |
| Offline host, full storage, broken rootless Podman, or denied authorization | The error identifies the failed stage and exposes a keyboard-accessible copy-log action. Retrying after recovery creates one app record. |
| Cross-distro package with a declared dependency absent from the image | Installing pulls the dependency inside the container. The app opens from the library and the system menu. The host package database has no record of the foreign package. |
| Dependency unavailable or incompatible | The dialog identifies the dependency when the manager reports it. No success result or shortcut appears. |
| App with a known desktop identity and icon | Opening the exported shortcut displays the app icon in the menu and associates its window with that entry on the tested desktop. |
| Interrupted install or export | Reopening Packlift offers recovery from the recorded stage. Repeated recovery creates no duplicate entries. |
| Keyboard and Orca, high contrast and 200% text scale | The user completes setup, install and error recovery. Focus stays visible, every action is reachable, stage changes are announced once, and long paths leave the action buttons reachable. |

Record the Packlift build, host release, image digest, package version, desktop session and result for each run. Claim support for a host and target combination only after its setup, dependency installation, launch and recovery cases pass. Track untested combinations explicitly.
