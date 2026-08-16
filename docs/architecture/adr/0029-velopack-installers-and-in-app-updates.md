# Velopack installers and in-app updates

Status: Accepted and implemented

## Context

Artificer had no installer and no way to update itself. A release was a zip or
a tarball of one executable attached to a GitHub release, and every upgrade was
a manual download by someone who happened to look at the releases page. For a
CAD application that ships often and early, that is a slow leak: the version in
use drifts arbitrarily far from the version being tested, and a fix reaches
only the users who go looking for it.

Three smaller faults sat inside the same release path:

- The macOS asset was the bare `artificer-workbench` executable, while the
  README promised an `Artificer.app`. Only `build-standalone.sh`, run by hand,
  ever assembled a bundle.
- `packaging/macos/Info.plist` carried `CFBundleShortVersionString 0.1.1`
  against a 0.2.0 workspace — a hand-maintained number two releases stale.
- Three platform jobs raced to create the same tag, each creating the release
  best-effort and uploading into whatever won.

## Decision

### Velopack packages Windows and Linux; macOS keeps its bundle

[Velopack](https://velopack.io) produces the installer, the update packages,
and the delta packages, and the GitHub releases page is the update feed itself.
There is no server to run and no metadata to keep in step by hand.

Windows ships a `Setup.exe` that installs per-user; Linux ships an AppImage.
Both update themselves in place from the same releases page they were
downloaded from.

macOS is **not** packaged with Velopack. Velopack requires a signed and
notarised bundle there, which requires Apple Developer certificates this
project does not have. Rather than dropping macOS or shipping something that
cannot work, the macOS asset stays the unsigned `.app` zip — now actually a
bundle, built by the same script a developer runs locally, with an icon and a
version written from the crate rather than typed into a plist.

Adding macOS later is purely additive: Velopack channels default to the target
OS name, so `releases.win.json` and `releases.linux.json` never collide, and an
`osx` channel can appear beside them without migrating anyone.

### The pack id is permanent

The pack id is `Artificer`. It names the install directory, the update cache,
and the channel feed. Changing it later would not upgrade existing installs —
it would orphan them, leaving a second copy of the application on disk with no
relationship to the first.

### Nothing installs itself without being asked

Applying an update terminates and relaunches the process, so it can never be a
side effect of something else. The workbench checks the feed once, silently, on
the first frame after launch, and that is the only automatic network request
the updater makes. Everything after that is an explicit click: `Download
update`, then `Restart and install`, which carries the warning that the app
will close.

`VelopackApp::build().run()` is the first statement in `main`, before the
window, before any file is touched. The installer and the updater re-run the
executable with their own arguments to perform install, uninstall, and
post-update steps; anything above that call would run at moments the user never
asked for.

### A build that cannot update itself says so

A copy Velopack did not install — `cargo run`, the macOS bundle, a binary
pulled out of a zip — has no locator to find, so `UpdateManager` refuses to
construct. That is a first-class state (`UpdateStatus::Unmanaged`), not an
error: About reports it plainly and offers the releases page instead of a
button that would fail.

It is also what keeps the test suites offline. A test binary is not an
installation, so no suite can reach the network even by accident, and the
states the network would otherwise produce are set directly.

### The release is a draft until every asset has landed

One job opens the release as a draft, three jobs upload into it, and a final
job publishes it. A partial failure leaves a draft for a maintainer to look at
rather than a public release missing its feed file — which would leave update
checks failing for everyone who installed from it.

The tag and the workspace version are checked against each other before any of
that begins, because Velopack keys every install on the pack version and a
release whose tag disagrees with the binary inside it cannot be reasoned about
afterwards.

## Consequences

- Existing users cannot be migrated automatically. Everyone on a zip or a
  tarball needs one manual download of the installer; after that, never again.
  The plain archives are published alongside the Velopack assets for one
  release cycle so that download is the same shape as the asset they have.
- The Windows installer is unsigned, so SmartScreen warns per release until
  reputation accrues. Code signing is a later, additive change: a certificate
  and `--signParams`, with nothing else moved.
- Packaging now needs the .NET 8 SDK for the `vpk` CLI, on release runners and
  for local packaging. The `vpk` version is pinned to the `velopack` crate
  version the workbench links against; the tool and the library are two halves
  of one format.
- The updater contacts GitHub once per launch. No telemetry is sent, and
  unauthenticated GitHub API requests are rate limited per IP, which one check
  per launch is comfortably inside.
- Icons are generated from one description by `scripts/build-icons.py` rather
  than maintained as three hand-cut files. They are a placeholder mark, not a
  designed identity.
