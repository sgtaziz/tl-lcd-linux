# TL LCD Linux

`tl-lcd-linux` is a Rust service that replaces the Windows-only L-Connect runtime for Lian Li TL Wireless LCD fans. It handles the full USB+wireless handshake, DES header generation, and continuous streaming of videos or images to the LCD panels on Linux.

## Features

- Automatic detection of the wireless TX/RX dongles and individual LCD controllers
- DES-CBC encryption of LCD headers (`slv3tuzx` key) exactly as the original software
- Continuous wireless polling and per-frame TX prep bursts to keep the dongle in the correct mode
- Smooth video playback (mp4 decoded via ffmpeg), GIF animations, or static image/color frames
- Sensor dashboards with configurable labels, units, colors, and command-driven data sources
- Hot-plug support and live config reloads without restarting the service
- Automatic recovery from USB timeouts (software reset of the TX dongle)

## Requirements

- Rust 1.75 or newer (`rustup install stable`)
- `ffmpeg` and `ffprobe` in `PATH` (used for video decoding)
- `libusb` development headers (`libusb-1.0-0-dev` on Debian/Ubuntu)
- The Lian Li TL wireless kit connected over USB (devices `0416:8040`, `0416:8041`, `1cbe:0006`)

## Building

```bash
cd tl-lcd-linux
cargo build --release
```

The compiled binary will be at `target/release/tl-lcd-linux`.

## Configuration

At startup the service reads `config.json` (path can be overridden with `--config`). Example:

```json
{
  "default_fps": 30,
  "lcds": [
    { "serial": "d042fcb2061e0566", "type": "video", "path": "/path/to/video/portal.mp4", "orientation": 0 },
    { "serial": "29c3a0b00a1e0566", "type": "color", "rgb": [0, 255, 128], "orientation": 90 },
    { "serial": "c72a34b39a1e0976", "type": "gif", "path": "/path/to/gif/loop.gif", "orientation": 180 }
  ]
}
```

**Note:** The key `lcds` replaces the old `devices` key for clarity. The old `devices` key is still supported for backward compatibility.

**Device Identification Fields:**

- **`serial`** – _(recommended)_ Device serial number for stable identification. Serial numbers don't change when you replug devices. See [Finding your LCD serial numbers](#finding-your-lcd-serial-numbers) below.
- **`index`** – _(legacy)_ Zero-based LCD index (0..n-1). Less reliable as USB order can change after replug. Still supported for backwards compatibility.

**Note:** Each device must have either `serial` or `index` specified. Using `serial` is strongly recommended for reliable device targeting.

### Finding your LCD serial numbers

Each LCD controller (`1cbe:0006`) has a unique USB serial number burned into its firmware. This is the value you put in the `"serial"` field in your config.

```bash
lsusb -v -d 1cbe:0006 2>/dev/null | grep iSerial
```

This prints one line per connected LCD device, e.g.:

```
  iSerial                 3 d042fcb2061e0566
  iSerial                 3 29c3a0b00a1e0566
  iSerial                 3 c72a34b39a1e0976
```

The hex string at the end of each line is the serial you need. If you have multiple LCDs, you can figure out which serial corresponds to which physical fan by unplugging one at a time and re-running the command.

> **Tip:** Serials are stable across reboots and replugs — once you've identified them, they won't change.

**Other Fields:**

- `type` – `"video"`, `"image"`, `"gif"`, `"color"`, or `"sensor"`.
- `path` – file path for videos/images (relative paths are resolved against the config directory).
- `fps` – optional FPS override for videos (falls back to `default_fps`).
- `rgb` – solid color `[R,G,B]` for `"color"` entries.
- `orientation` – optional rotation in degrees (clockwise). Values are normalized to the nearest 90°; use 0/90/180/270 to match fan mounting.
- `sensor` – required for `"sensor"` entries; provides label/unit, data source, colors, gauge ranges, and update cadence.

**Sensor configuration example**

```json
{
  "serial": "abc123def4567890",
  "type": "sensor",
  "orientation": 270,
  "sensor": {
    "label": "CPU",
    "unit": "%",
    "source": { "type": "command", "cmd": "sensors | awk '/Package/ {print $4}' | tr -d '+°C'" },
    "text_color": [255, 255, 255],
    "background_color": [0, 0, 0],
    "gauge_background_color": [60, 60, 60],
    "gauge_ranges": [
      { "max": 50, "color": [0, 200, 0] },
      { "max": 80, "color": [220, 140, 0] },
      { "color": [220, 0, 0] }
    ],
    "update_interval_ms": 1000,
    "gauge_start_angle": 90,
    "gauge_sweep_angle": 330,
    "gauge_outer_radius": 180,
    "gauge_thickness": 40,
    "bar_corner_radius": 5.0,
    "value_font_size": 72.0,
    "unit_font_size": 32.0,
    "label_font_size": 28.0,
    "font_path": "fonts/Roboto-Bold.ttf",
    "decimal_places": 1,
    "value_offset": 0,
    "unit_offset": 60,
    "label_offset": -60
  }
}
```

Sensor fields:
- `source` – may be `"constant"` with a fixed percentage or `"command"`, evaluated each refresh (stdout must begin with a numeric value)
- `gauge_ranges` – color thresholds; defaults to green/orange/red ramp if omitted; values are clamped to `0..100`
- `bar_corner_radius` – roundness applied to the extremities (start/end caps) of the filled gauge portion in pixels (default: `0.0` for sharp corners)
- `value_font_size`, `unit_font_size`, `label_font_size` – font sizes in points (defaults: `72.0`, `32.0`, `28.0`)
- `value_offset`, `unit_offset`, `label_offset` – vertical offset in pixels from center for each text element (defaults: `0`, `60`, `-60`)
- `font_path` – optional path to TTF font file for custom rendering; uses built-in bitmap font if omitted
- `decimal_places` – number of decimal places to display (0-10, default: `0`)

### Fan Speed Control

The service can control fan speeds via the wireless dongle. Fan configuration supports per-fan control with either constant speeds or temperature-based curves.

```json
{
  "default_fps": 30,
  "lcds": [...],
  "fan_curves": [
    {
      "name": "cpu",
      "temp_command": "sensors | awk '/Package/ {print $4}' | tr -d '+°C'",
      "curve": [
        [30, 20],
        [50, 40],
        [70, 70],
        [85, 100]
      ]
    }
  ],
  "fans": {
    "speeds": [128, "cpu", "cpu", 0],
    "update_interval_ms": 1000
  }
}
```

**Fan Configuration:**

- **`fan_curves`** – Array of named temperature-based curves (optional)
  - `name` – Unique identifier for the curve
  - `temp_command` – Shell command that outputs temperature in Celsius
  - `curve` – Array of `[temperature, speed_percent]` points for interpolation

- **`fans`** – Fan speed configuration (there is only one wireless dongle, device_index is always 0)
  - `speeds` – Array of 4 fan configurations (one per fan):
    - Number (0-255): Constant PWM value (0 = 0%, 128 = 50%, 255 = 100%)
    - String: Reference to a named curve in `fan_curves`
  - `update_interval_ms` – Update interval in milliseconds (default: 1000)
  - **IMPORTANT**: Unused fan slots MUST be set to `0` (e.g., if you have 3 fans, 4th value = `0`)

**Examples:**

**All fans at constant speed:**
```json
{
  "fans": {
    "speeds": [128, 128, 128, 0]
  }
}
```

**Mixed: Fan 1 constant, Fans 2-3 temperature-controlled:**
```json
{
  "fan_curves": [
    {
      "name": "cpu",
      "temp_command": "sensors | awk '/Tctl/ {print $2}' | tr -d '+°C'",
      "curve": [[30, 20], [50, 40], [70, 70], [85, 100]]
    }
  ],
  "fans": {
    "speeds": [255, "cpu", "cpu", 0]
  }
}
```

**Multiple curves for different fans:**
```json
{
  "fan_curves": [
    {
      "name": "cpu",
      "temp_command": "sensors | awk '/Package/ {print $4}' | tr -d '+°C'",
      "curve": [[30, 30], [60, 60], [80, 100]]
    },
    {
      "name": "gpu",
      "temp_command": "nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader",
      "curve": [[40, 30], [70, 70], [85, 100]]
    }
  ],
  "fans": {
    "speeds": ["cpu", "gpu", "cpu", 0]
  }
}
```

The service watches the config file for changes; editing it will hot-reload both LCD and fan configurations.

**Technical Note:** Fan control uses the wireless RF protocol with MAC addresses and channel automatically discovered from the hardware at startup. This ensures compatibility across different hardware setups and L-Connect 3 configurations without requiring manual configuration.

**Device Groups:** The L-Wireless controller supports up to 10 groups of devices. Each group can contain up to 4 fans (with a maximum of 2 being LCD fans), one Strimer Wireless cable, or one wireless AIO. When configuring fans, you can either:
- Use a flat array `[speed, speed, speed, speed]` for a single group (backward compatible)
- Use an array of arrays `[[speed, speed, speed, speed], [speed, speed, 0, 0]]` for multiple groups

Each group is discovered automatically with its unique MAC address and controlled independently.

## Running

```bash
sudo ./target/release/tl-lcd-linux --config /path/to/config.json --log-level info
```

Root privileges are recommended so the process can detach kernel drivers and access the USB interfaces. Log verbosity is selectable via `--log-level` (`error`, `warn`, `info`, `debug`, `trace`). Device attach/detach, automatic wireless resets, and (at `debug`) frame cadence are emitted through the standard logging output.

## Extending

- `src/config.rs` – JSON config parsing/validation.
- `src/media/` – modular media handling:
  - `mod.rs` – MediaAsset enum and preparation orchestration
  - `common.rs` – shared utilities, error types, JPEG encoding
  - `image.rs` – static image and color frame rendering
  - `video.rs` – video (ffmpeg) and GIF processing
  - `sensor.rs` – dynamic sensor gauges with TTF font rendering
- `src/hardware.rs` – low-level USB + DES header logic (IDs, encrypted headers, wireless polling).
- `src/service.rs` – long-running controller (hot-plug, streaming loop, recovery).

To add new content types (e.g. live telemetry), implement a new variant in `MediaAsset` (in `src/media/mod.rs`), create a new module in `src/media/` for the rendering logic, and the service automatically handles scheduling and playback.
