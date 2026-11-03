<p align="right">
  <a href="README.zh_CN.md">简体中文</a> · <strong>English</strong>
</p>

# VibeMic (feature/vibemic)

VibeMic turns the FoloToy AI Passport (ESP32-C3) into a vibecoding voice remote:
a BLE HID keyboard whose three keys drive the AI tool that has focus on the PC.
Holding OK streams the device microphone over BLE to the companion desktop app
(workspace directory `../desktop`), which recognizes the speech locally and types
the result into the focused window.

## What this branch builds

| Button | Press / release | Click | Double click | Long press |
| --- | --- | --- | --- | --- |
| OK | PTT: hold-to-talk (press = start mic streaming, release = stop → PC recognizes and types); gated on an active PC session. The **second press of a double click** is the desktop app's delete gesture: it does not start PTT (mic stays off) and the screen shows `DELETE` | ignored | delete gesture (second press only) | ignored |
| UP | — | `Enter` (send) | reserved (no slot action) | `Ctrl+Enter` |
| DOWN | — | `Esc` (cancel) | two `Esc` (stop: e.g. DSH interrupts on double Esc) | hold 10 s → **BLE hard reset**: clear all bonds, drop the link, re-advertise (recovery gesture; runs on the device, so it works with the PC app offline. The host must then delete the pairing and pair again) |

The button component reports a double click only when the second press lands inside its
180 ms window after the first release, and a pending single click is then dropped instead
of being emitted. Click and double click are distinct slots, so a fast double tap does not
degrade into one single click. The same 180 ms window is used by `vm_keymap_is_double_press()`
to recognise the OK delete gesture on the device itself.

- BLE HID keyboard (HOGP, boot + report protocol) with Just-Works bonding.
- VibeMic custom GATT service (Base UUID `a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7aXX`)
  carrying the framed control protocol (see `main/vm_protocol.c`).
- Three built-in profiles (Trae toggle / WeChat hold / Win+H dictation); the
  active profile persists in NVS and can be switched from the PC.
- Redesigned single-screen UI (product name, link state, battery, PTT timer).

## Desktop session compatibility

Firmware `0x0006` requires an encrypted `CMD_PC_SESSION` (`0x05`) write on
VM_CTRL after STATUS and EVENT subscriptions are ready. Its one-byte payload is
`1` to renew a 15-second lease or `0` to end the session. The desktop transport
renews every 5 seconds, including during audio. Stale CCCDs alone never activate
PC mode; an expired lease restores direct HID and stops microphone streaming.
VM_PROFILE accepts only PROFILE_CHUNK, and both writable characteristics require encryption.

Update the desktop app before flashing this firmware. The new desktop supports
older firmware; an old desktop paired with this firmware retains standalone HID
but cannot activate desktop key dispatch or voice recording.

## Layout

```
main/
  vibemic_app.c   application entry, main-screen UI (redesigned), state polling
  vm_buttons.c    button events → profile slot lookup → HID macro queue
  vm_hid.c        NimBLE stack: HID/battery/device-info services, advertising,
                  connection management, macro executor (10 ms report throttle)
  vm_link.c       VibeMic custom service: frame decode, command worker, notify
  vm_profile.c    built-in profiles + NVS active-id persistence
  vm_protocol.c   frame codec (pure C, host-tested) — tests/test_vm_protocol.c
components/bsp/   unchanged except bsp_button gains BSP_BTN_RELEASE
```

Baseline demo sources stay in `main/` uncompiled so upstream host tests keep
running; this application does not reuse the demo menu or its visual shell.

## Build & test

```bash
./tools/validate.sh --static    # repo checks + host tests (includes vm_protocol)
./tools/validate.sh --firmware  # ESP-IDF 5.5.3 build + merged image
```

Current status: the firmware and the protocol/control-plane modules above are
implemented. Profile chunk upload, the on-device menu, and dual-host bond slots
are not implemented yet.
