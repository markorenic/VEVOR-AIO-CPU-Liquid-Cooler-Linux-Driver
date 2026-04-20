# VEVOR LCD protocol notes

> **Fully decoded as of 2026-04-18, session 2.**
> This is a *segmented* 1.8" LCD, not a framebuffer.
> Everything the display can show is described below. There is nothing left to
> reverse.

---

## TL;DR

- The display is a **fixed-layout segmented LCD**. Printed artwork on the glass
  provides the labels ("CPU", "GPU", °C, °F, cyan ring, colons, etc.). The
  firmware only drives a handful of 7-segment digit groups.
- The physical display shows exactly three pieces of live data:
  1. **HH:MM** clock
  2. **CPU temperature** (two digits)
  3. **GPU temperature** (two digits)
- The `PC Monitor All V3` OEM software sends a 64-byte HID OUT report roughly
  every 500 ms. **Of those 64 bytes, only 4 bytes are actually rendered.** The
  rest are either pre-computed metadata the firmware ignores, constants, or
  padding.
- Host → device is write-only from a rendering standpoint. The IN reports are
  either unsolicited status pings or simple ACKs — not required for a driver.

### The entire useful protocol

| Byte | Meaning                      | Encoding              |
|------|------------------------------|-----------------------|
| 3    | CPU temperature              | signed/unsigned 8-bit int, whole degrees in whatever unit the user picked |
| 13   | GPU temperature              | same as byte 3 |
| 29   | Hour (of clock displayed)    | 8-bit binary, 0–23 |
| 30   | Minute (of clock displayed)  | 8-bit binary, 0–59 |

That's it. A Linux driver that sets those four bytes (and leaves the rest
either zero or copied from a recorded template) produces a visually identical
result to the OEM software.

---

## Hardware framing (unchanged from Phase 1)

- USB VID:PID `5131:2007`, product string `"FBB"`.
- USB 1.1 full-speed HID device, vendor usage page `0xFF00`.
- **Endpoints**:
  - `0x02` OUT, 64-byte interrupt, 1 ms polling
  - `0x82` IN, 64-byte interrupt, 1 ms polling
- **Reports**: one 64-byte Input, one 64-byte Output, **no Report IDs**, no
  Feature reports.
- **On Linux `hidraw` write()**: prefix with a `0x00` report-id byte, so the
  buffer you `write()` is 65 bytes total. On Windows `HidD_SetOutputReport` /
  `WriteFile`, write 65 bytes where byte 0 is `0x00`.
- **Watchdog**: if the host stops writing for more than ~1 second the LCD
  **blanks / sleeps**. A continuous write loop at ≥ 200 ms cadence keeps it lit.
  (Confirmed in session 2 via `windows_poke.py`: stop the keepalive thread and
  the display goes dark within ~1 s.)

---

## Frame structure — full 64-byte layout

Positions are 0-indexed, as seen in the OUT report **after** stripping the
leading report-id byte.

```
offset  value         role
------  ------------  --------------------------------------------------
  0     0x00          constant sync / header byte; never varies
  1     0x01          constant header byte; never varies
  2     0x02          observed opcode, constant across every capture we
                      took (01-handshake, 02-idle, 03-toggle-celsius,
                      04-dropdown-change, 06-app-relaunch). Switching
                      sensor dropdowns in the app does NOT change it, so
                      this device only has one "mode" from the host side.
  3     CPU temp      *** RENDERED *** 8-bit whole-degree value
  4     ??            unused / padding
  5     °C/°F flag    advisory only; see "Temperature unit" below
  6–12  ??            unused / padding
 13     GPU temp      *** RENDERED *** 8-bit whole-degree value
 14     ??            unused / padding
 15     °C/°F flag    advisory only
 16–25  ??            unused / padding — host app fills with misc sensor
                      data (fan RPM, pump speed, water temp from its
                      dropdowns) that the LCD firmware silently discards
 26     year low      ignored by device
 27     month         ignored
 28     day           ignored
 29     hour          *** RENDERED *** 8-bit binary, 0–23
 30     minute        *** RENDERED *** 8-bit binary, 0–59
 31     second        ignored (display is HH:MM only, no seconds segment)
 32     day-of-week   ignored (no DoW segment on the glass)
 33–62  ??            padding / unused
 63     checksum?     varies — may be a simple 8-bit sum. Never validated
                      by the firmware (we sent arbitrary values via
                      windows_poke.py with no rejection), so driver can
                      set 0x00.
```

### Temperature unit (bytes 5 / 15)

- The OEM app **does the °C→°F conversion PC-side** before putting the number
  in byte 3 / 13. The display does not do any math.
- Bytes 5 and 15 are set to `0x01` when the app is in Fahrenheit mode,
  `0x00` in Celsius — but the LCD has **no °C / °F indicator segment** to
  toggle. Both letters are permanently printed on the glass.
- In session 2 we directly flipped byte 5 between 0 and 1 via
  `windows_poke.py`: **no visible change on the display.** Confirmed purely
  advisory.
- For a Linux driver: just always pick a unit and send that raw integer in
  byte 3 / 13. Bytes 5 and 15 can stay 0.

### Date bytes (26–28) and day-of-week (byte 32)

- The app sends year/month/day/DoW every frame. We flipped each of them
  individually in session 2. **None of them produce any visible change.**
- The glass has no date segments. Ignore them in the driver.

### Seconds byte (31)

- Sent by the app, changes every second in `02-idle.pcapng`, but the clock on
  the LCD is **HH:MM only** (no seconds digits on the glass). Not rendered.

---

## What the device sends back (IN reports)

- In every capture we took, IN reports arrive at the same cadence as OUT
  reports and carry a mostly-static payload that looks like a firmware
  version + running counters.
- We do **not** need to read IN reports to drive the display. A driver that
  only writes works fine (verified in session 2 — `windows_poke.py` never
  reads during normal operation and the display renders correctly).
- If we ever want them: `hidraw` read() returns a 64-byte payload (no
  report-id byte because there are no Report IDs).

---

## Handshake / startup

There is **no host-side handshake**. Specifically:

- `01-handshake.pcapng` captures only the standard OS USB enumeration
  (GET_DESCRIPTOR, SET_IDLE, string descriptor reads). None of that is
  application-level — the Linux kernel's `usbhid` / `hidraw` has already
  done it by the time `/dev/hidraw*` exists.
- `06-app-relaunch.pcapng` shows that the OEM app, on launch, immediately
  starts pushing 64-byte telemetry frames. **No MAGIC / INIT / WAKE
  sequence.** First frame is already a normal telemetry frame.
- `05-app-quit.pcapng` shows traffic simply stopping. No goodbye packet,
  no blanking command. The display sits on the last received frame until
  the watchdog blanks it ~1 s later.

**Driver implication:** open `/dev/hidraw*`, start writing telemetry frames
at ≥ 5 Hz. Done.

---

## What's NOT in this protocol

Things we looked for in captures and/or poked at directly, that **do not
exist** on this model:

- No image upload. No chunked framebuffer transfer. No color data anywhere.
- No brightness control.
- No backlight on/off command. (Only mechanism to turn the screen off is to
  stop sending frames for > ~1 s.)
- No per-digit blink / flash / animation command.
- No "theme" or "mode" switch. The single opcode `0x02` is all there is.
- No firmware version query that we need. (The device sends something that
  *looks* like a version string in IN reports, but the driver doesn't need
  to parse it.)

The marketing claim of "Freestyle Images, GIFs, MP4" on the Amazon listing
(B0F1TMW79P) refers to a **different SKU** in the same VEVOR product line
that has a real IPS panel. This specific 1.8" unit is the cheapest variant
and physically cannot display arbitrary graphics.

---

## Reference frame (known-good baseline)

The sanest way to produce a Linux-side driver is to extract one real OUT
report from `captures/02-idle.pcapng` and use it as the template. On
Ubuntu, with `dissect.py`:

```bash
python3 dissect.py captures/02-idle.pcapng --direction OUT --head 1 --hex
```

Take the 64-byte payload that prints, paste it into the driver as the
constant `BASELINE`, and then every frame you send is just:

```python
buf = bytearray(BASELINE)
buf[3]  = cpu_temp     # whole degrees, whichever unit you prefer
buf[13] = gpu_temp
buf[29] = hour          # 0..23
buf[30] = minute        # 0..59
write("\x00" + bytes(buf))   # the 0x00 prefix is the hidraw report-id byte
```

The windows-side poke tool (`windows_poke.py`) already hard-codes a
`BASELINE` constant derived this way — it's the frame the device is
known to accept without complaint, and all of our experimental evidence
in session 2 came from mutating single bytes of it.

Shape summary (values vary, positions don't):
- byte 0: `0x00` header, constant across captures
- byte 1: `0x01` header, constant
- byte 2: opcode `0x02`, constant
- byte 3: CPU temp — overwrite this
- byte 13: GPU temp — overwrite this
- bytes 26–28: year/month/day (ignored by device)
- byte 29: hour — overwrite this
- byte 30: minute — overwrite this
- byte 31: seconds (ignored)
- byte 32: day-of-week (ignored)
- everything else: padding / unused

---

## Compatibility note

This document describes the VEVOR AIO sold as B0F1TMW79P (1.8" segmented
LCD variant) with VID:PID `5131:2007` and product string `"FBB"`. Other
VEVOR AIOs with the same USB ID but a larger graphical display almost
certainly use a different protocol on top of the same HID transport —
this document **does not** cover those.
