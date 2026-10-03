# Plan: Android viewer — keyboard, cursor visibility, zoom

The Android viewer (`android/app/.../SessionActivity.kt`, Rust in
`crates/tidedesk-android`) shows the remote screen and sends touch as mouse.
It cannot send keystrokes, has no visible pointer, and cannot zoom. These
three are the bare minimum to make it usable for real work.

## What already exists

- Touch -> mouse: tap = left click, long press = right click, one-finger drag
  = left-drag, two fingers = scroll. In `SessionActivity.touch()`.
- JNI input bridge: `Native.pointer/button/wheel`, backed by
  `Viewer::pointer/button/wheel` in `session.rs`, sending `InputEvent` over the
  existing protocol.
- **The wire protocol already carries keys**: `InputEvent::Key { scancode: u16 }`,
  PC/AT set-1 scancodes, `0xE0` in the high byte for extended keys. The host
  already applies them. The desktop viewer builds these in
  `tidedesk-view/src/keys.rs` (`scancode()` and the `typed()` fallback).

So keyboard is not a protocol change; it is wiring the Android side to a message
type that already works.

## Feature 1: keyboard (the core, do first)

### Rust (small)

- Add `Viewer::key(&self, scancode: u16, pressed: bool)` in
  `crates/tidedesk-android/src/session.rs`, sending
  `InputEvent::Key { scancode, pressed }` the same way `button()` sends
  `MouseButton`.
- Add the JNI export `Java_app_tidedesk_viewer_Native_key(handle, scancode, pressed)`
  in `jni.rs`, copying the `button` function.
- Add `external fun key(handle: Long, scancode: Int, pressed: Boolean)` to
  `Native.kt`.

### Scancode mapping (the real work)

The desktop maps from winit key codes; Android gives `KeyEvent` keycodes and
typed text, so Android needs its own small map to PC set-1 scancodes. Keep it
to what a keyboard actually sends:

- Letters a-z, digits 0-9, and the main punctuation (US layout).
- Enter, Backspace, Tab, Space, Escape.
- The modifiers: Shift, Ctrl, Alt. Track held state so combos (Ctrl+C) send
  modifier-down, key, key-up, modifier-up.
- Arrows, Home/End, PageUp/Down, Delete, Insert (the `0xE0` extended set).
- Function keys F1-F12 if easy; otherwise a later addition.

Two input sources on Android, handle both:

1. **Hardware / Bluetooth keyboards:** override `onKeyDown`/`onKeyUp` on a
   focusable view, map `event.keyCode` to a scancode, send down/up directly.
   This is the clean path and worth doing first.
2. **The on-screen (soft) keyboard:** Android delivers most soft-key input as
   committed text through an `InputConnection`, not as key events. Provide a
   `BaseInputConnection` on a focusable view and, for each committed character,
   send a press+release for its scancode (shifting when the character needs it),
   mirroring the desktop's `typed()` fallback. Non-US characters that have no
   set-1 scancode are dropped in this first version, as the desktop already does.

### UI

- A way to summon the soft keyboard: a small toolbar toggle, or a three-finger
  tap, or a menu. A toolbar (see Feature 3's shared bar) is cleanest.
- A hidden focusable `View` that owns the `InputConnection` and key callbacks,
  so the `SurfaceView` keeps drawing while the view takes input focus.

### Tests

- Rust: a unit test that `Viewer::key` emits `InputEvent::Key` with the right
  scancode and pressed flag (mirror the existing input tests in `protocol.rs`).
- Kotlin: a pure JVM test of the keycode/character -> scancode map for a table
  of cases (letters, a Ctrl+C combo, an arrow key, a shifted symbol).

## Feature 2: cursor you can see ("see mouse")

On a phone, absolute touch means you click where you tap, with no pointer to
aim first. Add an optional **trackpad mode**:

- A local cursor overlay (a small drawable) at a tracked position.
- One finger drags the cursor relatively (not absolute), with a sensitivity
  factor; a tap taps at the cursor, long-press right-clicks there, two fingers
  still scroll.
- Absolute mode (today's behaviour) stays the default; trackpad mode is a
  toggle on the toolbar. This keeps both styles.

This is viewer-side only; it reuses `Native.pointer/button` with the cursor's
position mapped through the same 0..1 coordinates as `move()`.

## Feature 3: zoom in / out

Pure viewer-side, no protocol change:

- Add a `ScaleGestureDetector`; pinch scales the `SurfaceView` (scaleX/scaleY)
  about the gesture focus, clamped (for example 1x to 4x).
- When zoomed in, one finger pans instead of dragging the mouse; dragging the
  mouse stays available in trackpad mode or with a modifier, to avoid a gesture
  clash. Decide the exact gesture split during implementation and document it.
- `move()` must invert the current scale and pan so a touch still lands on the
  right remote point. This is the one correctness-sensitive part: add a helper
  that converts a screen point to a surface point, and unit-test the maths.

A shared, auto-hiding **toolbar** across the top holds: keyboard toggle,
trackpad/absolute toggle, zoom reset, disconnect. It gives Features 1-3 a home
and keeps the gesture surface uncluttered.

## Order of work

| Step | Scope | Output |
|------|-------|--------|
| 1 | Rust `key()` + JNI + `Native.kt` | Android can send a scancode |
| 2 | Kotlin keycode/char -> scancode map + tests | correct scancodes |
| 3 | Hardware-keyboard key events | Bluetooth keyboard works |
| 4 | Soft keyboard via InputConnection + toolbar toggle | on-screen typing works |
| 5 | Pinch-zoom + pan + coordinate maths + test | zoom works, clicks land right |
| 6 | Trackpad cursor mode | a visible pointer to aim |

Steps 1-4 deliver the keyboard, which is the priority. Step 5 (zoom) is
independent and can be done in parallel. Step 6 (cursor) is the softest of the
three and can come last or be dropped from the minimum.

## Notes

- This is viewer-only; the host and the protocol are unchanged except that the
  host now receives keys from Android, which it already understands.
- Battery: input is event-driven, nothing added polls, matching the existing
  design.
- Keep each step building and testable on its own: `cargo test` for the Rust,
  a JVM unit test for the map, and a manual check against a real host for the
  gestures.
