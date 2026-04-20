# vevor-lcd Linux Driver

[![ci](https://github.com/markorenic/VEVOR-AIO-CPU-Liquid-Cooler-Linux-Driver/actions/workflows/ci.yml/badge.svg)](https://github.com/markorenic/VEVOR-AIO-CPU-Liquid-Cooler-Linux-Driver/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![rust](https://img.shields.io/badge/rust-1.85+-DEA584?logo=rust&logoColor=white)](https://doc.rust-lang.org/stable/edition-guide/rust-2024/index.html)

Tiny Linux driver for the **VEVOR AIO CPU cooler** [B0F1TMW79P](https://www.amazon.com/dp/B0F1TMW79P).

If your VEVOR AIO has a full *graphical* LCD instead (Freestyle images,
GIFs, video) it uses a different protocol on top of the same HID transport
and this driver won't produce anything useful.

## Where it runs

**Works as documented:** Linux with **systemd** and **udev** (Debian, Ubuntu,
and similar). `setup.sh` adds you to **`plugdev`** (common on those
distros). **Hardware:** USB `5131:2007`, product **`FBB`**, segmented LCD
only.

**Works with small tweaks:** Fedora, Arch, openSUSE, etc. — install the same
udev rule; if `plugdev` does not exist on your distro, you can still get
access via the rule’s **`uaccess`** tag after replug. Use the systemd user
unit as shown, or run the binary under your own service manager.

**Not supported here:** Windows, macOS, *BSD. Init systems without systemd
(no `systemd --user`): run `vevor-lcd` manually or wrap it yourself.

## What you get

- `~380 KB` stripped Rust binary, `~2 MB` RSS, effectively 0% CPU
- No runtime dependencies beyond libc
- Auto-detects CPU temp (`k10temp` / `coretemp` / `zenpower` via
  `/sys/class/hwmon`) and GPU temp (`nvidia-smi`, `amdgpu` hwmon fallback)
- systemd user-service unit with `Restart=on-failure` for unplug/replug
- Full protocol documented in [`PROTOCOL.md`](PROTOCOL.md) so someone with
  an adjacent SKU can port or extend

## Install

```bash
git clone https://github.com/markorenic/vevor-lcd.git
cd vevor-lcd

# 1. udev rule + plugdev membership (one-time, needs sudo)
sudo bash setup.sh

# 2. build the driver
(cd rust && cargo build --release)
install -Dm755 rust/target/release/vevor-lcd ~/.local/bin/vevor-lcd

# 3. install + enable the systemd user service
mkdir -p ~/.config/systemd/user
cp vevor-lcd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now vevor-lcd.service
loginctl enable-linger "$USER"          # keep running across logouts

# 4. (optional) sanity check
bash verify.sh
```

## Usage

```
vevor-lcd [--device PATH] [--interval MS]

  --device PATH   hidraw node to write (default: /dev/vevor_lcd)
  --interval MS   ms between frames (default: 200; must be 1..999 — the
                  device blanks itself after ~1 s of silence)
```

Everything else is auto-detected. Temperatures are rendered in °C.

## How it works

The firmware reads exactly four bytes out of the 64-byte HID OUT report:

| Byte | Meaning            |
|------|--------------------|
| 3    | CPU temperature    |
| 13   | GPU temperature    |
| 29   | Hour (0-23)        |
| 30   | Minute (0-59)      |

Everything else is header bytes and padding. No handshake, no opcodes.
Write at >1 Hz or the device blanks itself. Full analysis in
[`PROTOCOL.md`](PROTOCOL.md).

Two threads, no async runtime:

- **Main thread** writes a 65-byte HID report at 5 Hz, patching
  bytes 3/13/29/30 from atomics + a `libc::localtime_r` call.
- **Reader thread** re-reads CPU from sysfs and GPU from `nvidia-smi` once
  per second and stores into `AtomicU8`s.

## License

[MIT](LICENSE)
