# Navigation cube corners and edges, and 3D-mouse input

Status: Accepted and implemented

## Context

The view cube offered six faces, four turn arrows, and two roll buttons. An
isometric view — the view most CAD work is done in — was reachable only by
the ISO button, which resets to one fixed isometric, or by dragging the
cube. Every mainstream package lets the cube's corners and edges be clicked
for the other seven isometrics and the twelve half-turned views.

Separately, people who model all day tend to own a 3Dconnexion SpaceMouse,
and the workbench could not hear it. The vendor's driver stack is
proprietary and per-platform; the device itself is plain USB HID.

## Decision

### The cube's corners and edges are hit regions

`ViewState::set_view_along(direction, up_hint)` orients the camera to look
from any direction, with screen-up taken from the hint projected
perpendicular to it — world +Z, so the horizon stays level — and framing
untouched. Each corner and edge of the cube is a ten-pixel click target at
its projected vertex or midpoint, offered only while a face it belongs to
is visible, and registered after the faces so that egui's topmost-wins hit
test gives it the click rather than the face behind it. The marks are drawn
only while the pointer is over the cube, so the cube keeps its look. Names
are the face labels the cube already wears, joined front-to-back,
top-to-bottom, left-to-right: `View cube corner front-top-right`, `View
cube edge front-top`. Dragging the cube to orbit is unchanged.

A click does not cut to the new view. Every cube command — a face, an
edge, a corner, a roll arrow, the ISO button — names a destination camera,
and the workbench flies there through the same `CameraTransition` that
carries it to a face it is asked to look at squarely: shortest-path
orientation, the same quintic ease, framing untouched. A second click
while a flight is under way retargets it from wherever the camera is; a
drag on the cube takes over at once. A flight that a sketch is waiting on
is never steered, because its landing opens the sketch on the face it was
aimed at. With the flights switched off (the accessibility setting and the
test default) every command is instant, as before.

### The SpaceMouse is read as raw HID, in its own crate

`artificer-spacemouse` enumerates HID devices with the `hidapi` crate's
pure-Rust backends (`linux-native-basic-udev`, `windows-native`; IOKit on
macOS), so no C source and no vendor driver enter the build. A device is
recognised by a known 3Dconnexion or Logitech product id, or failing that
by declaring itself a Generic Desktop multi-axis controller. A reader
thread parses the three report shapes the pucks use (six-byte translation
and rotation under report ids 1 and 2, twelve-byte combined under id 1,
button bitmask under id 3), normalises ±350 to ±1 with a 3 % dead zone, and
folds reports into a `Motion` the application takes once a frame. Buttons
are edge events. The reader wakes the UI through a callback that requests a
repaint, so nothing polls, and it looks for a device every two seconds
when there is none or after a read error, so hot-plugging just works.

`Motion` is in 3Dconnexion's own SDK frame — x right, y up, z toward the
viewer, right-hand rotations — and the parser converts from the puck's
device frame (x right, y toward the user, z down). All of that is pure
functions with unit tests; no test needs a device.

### The camera mapping is the camera's

`ViewState::apply_six_dof(motion, seconds, settings)` in `ui-core` is the
whole mapping: rotation about up orbits the yaw, about right orbits the
pitch, about the viewing axis rolls only if enabled (off by default);
translation across the screen pans by a share of the framed radius, so a
push covers the same part of the window at any zoom; translation toward
the viewer zooms exponentially. Object mode (the default) moves the model
with the cap; camera mode reverses every axis. Per-axis inversion and one
overall sensitivity are settings. Frame time is capped as the turntable's
is, so a stall cannot throw the view.

The workbench opens the device once per process and shares the reader
between documents; only the document in front takes motion. Button 1
resets and fits the view; button 2 flips between the current view and the
one it was pressed in last. The puck is ignored while a sketch canvas is
up or the camera is mid-flight to a face. Script Studio applies the same
mapping with the default settings and treats either button as a fit.

### Absence is a state, not an error

No device, a device the process may not open, and a device unplugged
mid-session are all shown in About Artificer beside the sensitivity slider.
On Linux the hidraw node is root-only by default; the rule below grants it
to the logged-in user, after which the reader picks the device up on its
next look:

```
# /etc/udev/rules.d/70-spacemouse.rules
KERNEL=="hidraw*", ATTRS{idVendor}=="256f", MODE="0660", TAG+="uaccess"
KERNEL=="hidraw*", ATTRS{idVendor}=="046d", ATTRS{idProduct}=="c62?", MODE="0660", TAG+="uaccess"
```

Then `sudo udevadm control --reload-rules && sudo udevadm trigger`, and
replug the device. The same rule ships in the tree as
`packaging/linux/70-spacemouse.rules`, ready to copy.

## Consequences

- The workbench and Script Studio depend on `hidapi`; on Linux that is the
  pure-Rust hidraw path with `basic-udev`, and no `libudev` headers are
  needed to build.
- The device-frame sign conventions are taken from the pucks' published
  reports rather than measured on every model; the per-axis inversion
  settings exist for the one that disagrees.
- 3DxWare, if installed, may hold the device; the reader then reports it
  as found but not openable, which the About card shows.
