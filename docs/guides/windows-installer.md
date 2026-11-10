# Windows installer

An agent asked to produce a Windows build follows this page and stops at the first matching
section. Testing a build that already exists, or swapping a cross-built host into one, is
[windows testing](windows-testing.md).

The installer is an NSIS executable built on Windows. `apps/desktop/package.json` has
`dist:win:dev` (Codync Dev) and `dist:win:main` (the release identity). Both call
`tools/native-host.mjs`, which compiles `codync-host.exe` only when `process.platform` is
`win32`. On macOS that script builds the Mac host instead, so `npm run dist:win:*` on a Mac
does not produce a Windows app.

Ship the result only to the remote the user named. This checkout's `upstream` is the public
repository. Do not push, tag, or dispatch a workflow there unless the user asks.

## Decide

```sh
node -p process.platform
```

- `win32`: run [On Windows](#on-windows) and stop.
- `darwin` or `linux`: do not run `dist:win:*`, do not install Wine or a MinGW toolchain to
  force a cross-build, and do not create a virtual machine. Use [From the existing Windows
  VM](#from-the-existing-windows-vm) only when that VM is already installed. Otherwise use
  [On the fork's CI](#on-the-forks-ci).

## On Windows

Needs Node.js 24, rustup, and the toolchain in `host/rust-toolchain.toml`. From the repo root:

```sh
cd host && rustup toolchain install && rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc
cd ../apps/desktop && npm ci
```

Codync Dev, the build to use unless the user asked for the release identity:

```sh
npm run dist:win:dev -- --x64
```

On an ARM64 Windows machine, pass `--arm64` instead of `--x64`. The installer is
`apps/desktop/dist/codync-dev-windows-<arch>.exe`.

Release identity, only when the user asked for it:

```sh
npm run dist:win:main -- --x64
```

That writes `apps/desktop/dist/codync-windows-<arch>.exe`.

Install the one that was just built, per user, without a wizard. Stop an older copy first.
Dev and the release app install beside each other.

```powershell
Get-Process 'Codync','Codync Dev','codync-host','codync-hostw','codync-dev-hostw' -ErrorAction SilentlyContinue | Stop-Process -Force
$installer = Resolve-Path .\dist\codync-dev-windows-*.exe   # or codync-windows-*.exe
$p = Start-Process $installer -ArgumentList '/S','/currentuser' -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "installer exit $($p.ExitCode)" }
```

Dev lands in `%LOCALAPPDATA%\Programs\codync-desktop-dev`. The release app lands in
`%LOCALAPPDATA%\Programs\codync-desktop`. Confirm the bundled host answers:

```powershell
& "$env:LOCALAPPDATA\Programs\codync-desktop-dev\resources\codync-host.exe" --version
```

Use `codync-desktop` in that path for a `dist:win:main` build. Done when `--version` prints.
The installer is unsigned, so SmartScreen warns once on a normal launch.

## From the existing Windows VM

The Mac's UTM virtual machine is named `Codync Windows`. Account `codync` / `codync`, shell
`ssh codync-win`. Setup and the rule that large downloads happen on the Mac are in
[windows testing](windows-testing.md#the-vm). If `ssh -o BatchMode=yes -o ConnectTimeout=5
codync-win echo up` fails, stop and say the VM is not running. Do not create one.

When it answers, run [On Windows](#on-windows) there, in this checkout copied onto the VM.
The VM's network is slow: run `npm ci` on the Mac only for the Mac itself, and let the VM
install its own `node_modules`.

## On the fork's CI

`Release desktop` (`.github/workflows/release-desktop.yml`) builds x64 and ARM64 on GitHub's
Windows runners when dispatched with `windows_only`. A run that is not a `v*` tag uploads
artifacts and does not publish a release.

```sh
gh workflow run "Release desktop" --repo "$(gh repo view origin --json nameWithOwner -q .nameWithOwner)" --ref "$(git branch --show-current)" -f windows_only=true
```

Watch that run on `origin`. Download `codync-windows-x64` and `codync-windows-arm64` from it.
Those artifacts are the release identity (`dist:win:main`), including on a dry run. Do not
dispatch this against `upstream`.
