# Unsaved work is never lost silently, and files are chosen in the desktop's own dialog

Status: Accepted and implemented

## Context

Both desktop applications could throw away a person's work without a word.
Script Studio tracked whether the editor differed from the file (`is_dirty`)
and put a dot in the title, but File ▸ New, an example, a dropped file and
the window's close button all replaced or discarded the text without looking
at the flag. The workbench had no notion of "unsaved" at all: a document with
an hour of features in it closed, was replaced by Open, and lost its tab
exactly as a blank one did.

Both applications also asked for every file as a typed path in a text box.
That is a reasonable last resort and a poor first offer: the person has to
know the path, type it without error, and cannot browse.

## Decision

### Dirty is a comparison, not a flag that has to be remembered

Script Studio's document is its text: it is dirty when the text differs from
the text last read from or written to disk. The workbench's document is its
parametric history: it is dirty when `ModelDocument::revision()` differs from
the revision recorded at the last save or load. Neither app sets a "modified"
flag on every edit path, because a flag that must be set everywhere is a flag
that is missed somewhere; a comparison cannot be missed.

The workbench takes the saved revision *after* a load completes, not before:
replaying a loaded document rebuilds it, and a rebuild moves the revision on.
A blank document and a freshly built fixture count as saved; there is
nothing in them worth a prompt.

### One prompt, three answers, before anything that would lose work

Whatever would replace or discard a dirty document — closing the window,
closing a tab, opening another document, loading an example, a dropped file,
File ▸ New — first shows the same modal: *Save changes to X?* with **Save**,
**Don't save** and **Cancel**. Escape and a click outside are Cancel. The
action the person was taking is held and carried out only once they have
answered; after **Save** it goes ahead only if the save actually landed, so a
failed or cancelled save never turns into a discard.

The window's close button is intercepted the way egui intends: the frame that
sees `close_requested()` on a dirty document sends
`ViewportCommand::CancelClose` and raises the prompt; **Save** or **Don't
save** then sends `ViewportCommand::Close` with the decision recorded, so the
second close request goes through.

In the workbench, opening over a dirty document goes through the prompt and
*then* through the universal confirmation gate of ADR 0007, like every other
operation on the model. The two questions are different — "may these changes
be lost?" and "do this to the model?" — and neither replaces the other. The
document keeps its own path until the load has succeeded, so cancelling the
confirmation leaves a document pointing where it was, not at a file it never
opened.

### The dirty state is visible

A dirty document wears a dot: on the workbench's tab and beside its header
title, and in Script Studio's title. The dot is display, not identity — a
tab's accessible name does not change — so tests and screen readers keep
finding the document by its name.

### The desktop's file dialog first, a typed path always

Open, Save as, and every export ask through the desktop's own file dialog
(`rfd`: the XDG desktop portal on Linux, so no GTK is linked; the platform
dialogs on Windows and macOS), called synchronously on the UI thread when the
menu item or shortcut fires. Each dialog filters for its file kind and opens
beside the document, else where the last dialog ended, else at home; the
application data directory an unsaved workbench document autosaves into is
never offered as a place to browse.

The typed-path prompt stays, reachable as a secondary "by path" entry. Some
Linux desktops have no portal to offer a dialog, a path is sometimes quicker
to type than to browse to, and a headless test cannot drive a native dialog.
Every paused (test) constructor turns native dialogs off, so a test drives
the typed prompt and never blocks on a dialog nobody can see.

### Shortcuts

Ctrl+O opens, Ctrl+S saves, Ctrl+Shift+S saves as, in both applications.
In the workbench, Save on a document that has never been given a home asks
where; on one that has, it saves there without asking.

## Consequences

- No path in either application discards unsaved work without the prompt.
  New entry points must go through the guard (`guard_unsaved` in Script
  Studio; `request_open_document` and `WorkbenchShell::request_close` in the
  workbench) rather than replacing the document directly.
- The workbench's revision-based comparison means an undo back to the saved
  state still reads as dirty, because undo moves the revision forward. That
  is the honest answer for a history-based document and the same one most
  editors give.
- Tests: `apps/script-studio/tests/studio_ui.rs` closes a dirty script and
  checks the prompt appears and `CancelClose` is sent;
  `apps/workbench/tests/document_tabs_ui.rs` checks the dot appears on the
  first edit and clears on save, and that closing a tab, closing the window
  and opening by path over a dirty document all ask first.
