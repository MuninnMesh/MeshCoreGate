#!/usr/bin/env python3
"""Muninn Gate host-side CLI.

Install the tool environment:

    uv sync

Validate and upload a config when the firmware provisioning window opens:

    uv run python tools/cli.py config.json

Generate/persist MeshCore gateway keys, set WiFi, and upload:

    uv run python tools/cli.py config.json --keys --wifi "wifi-name" "wifi-password"

Use an explicit port only when more than one USB serial device is attached:

    uv run python tools/cli.py config.json /dev/cu.usbmodemXXXX

Generate a config without uploading:

    uv run python tools/cli.py config.json --keys -o config.generated.json
"""

from __future__ import annotations

import argparse
import errno
import glob
import hashlib
import json
import os
import select
import subprocess
import sys
import termios
import time
import tty
from pathlib import Path
from typing import Any

BAUD_RATE = termios.B115200
CHUNK_SIZE = 32
CHUNK_DELAY_SECS = 0.02
READY_TIMEOUT_SECS = 60.0
RESPONSE_TIMEOUT_SECS = 15.0
WRITE_TIMEOUT_SECS = 5.0
AUTO_PORT_WAIT_SECS = 20.0
READY_MARKER = r'{"type":"ready","request":"set_config"}'
SERIAL_PORT_PATTERNS = (
    "/dev/cu.usbmodem*",
    "/dev/cu.usbserial*",
    "/dev/cu.wchusbserial*",
    "/dev/cu.SLAB_USBtoUART*",
    "/dev/ttyACM*",
    "/dev/ttyUSB*",
)
DISCONNECT_ERRNOS = {
    errno.EBADF,
    errno.EIO,
    errno.ENODEV,
    errno.ENOENT,
    errno.ENXIO,
}
DISPLAY_VALUES = {
    "temperature",
    "humidity",
    "soc",
    "battery_voltage",
    "pressure",
    "luminosity",
    "rssi",
    "latency",
}
RADIO_BANDWIDTH_HZ = {7810, 10420, 15630, 20830, 31250, 41670, 62500, 125000, 250000, 500000}
RADIO_RAMP_US = {10, 20, 40, 80, 200, 800, 1700, 3400}
MIN_UTC_OFFSET_MINUTES = -12 * 60
MAX_UTC_OFFSET_MINUTES = 14 * 60


def main() -> int:
    """Run the CLI."""
    args = build_parser().parse_args()
    normalize_args(args)
    data = prepare_config(args)
    write_output(args, data)
    if args.port is None and not args.output:
        args.port = discover_serial_port(AUTO_PORT_WAIT_SECS)
    if args.port:
        upload_config(args.port, data)
    elif not args.output:
        sys.stdout.buffer.write(data)
    return 0


def build_parser() -> argparse.ArgumentParser:
    """Create the command-line parser."""
    parser = argparse.ArgumentParser(
        description="Prepare and upload Muninn Gate configuration."
    )
    parser.add_argument(
        "template",
        nargs="?",
        default="config.json",
        help="JSON/JSONC config template",
    )
    parser.add_argument("port", nargs="?", help="USB serial device")
    parser.add_argument("--name", help="gateway name")
    parser.add_argument(
        "--wifi",
        nargs=2,
        metavar=("SSID", "PASSWORD"),
        help="set WiFi credentials",
    )
    parser.add_argument(
        "--keys",
        action="store_true",
        help="generate gateway MeshCore keys; saved to config.json or -o output",
    )
    parser.add_argument("--token", action="append", help="HTTP bearer token")
    parser.add_argument("--no-token", action="store_true", help="set http.tokens to []")
    parser.add_argument(
        "--utc-offset-minutes",
        type=int,
        help="local display offset from UTC; e.g. CST is -360",
    )
    parser.add_argument(
        "--no-time-sync",
        action="store_true",
        help="do not inject current host Unix time into config.time",
    )
    parser.add_argument("--output", "-o", help="write final JSON to this path")
    return parser


def normalize_args(args: argparse.Namespace) -> None:
    """Normalize shorthand forms before loading config."""
    if args.port is None and is_serial_path(args.template):
        args.port = args.template
        args.template = "config.json"


def is_serial_path(value: str) -> bool:
    """Return true when a positional argument looks like a serial device."""
    return value.startswith("/dev/cu.") or value.startswith("/dev/tty.")


def prepare_config(args: argparse.Namespace) -> bytes:
    """Load, update, validate, and serialize the requested config."""
    config = load_config(Path(args.template))
    apply_overrides(config, args)
    validate_config(config)
    return json.dumps(config, indent=2, ensure_ascii=False).encode("utf-8") + b"\n"


def write_output(args: argparse.Namespace, data: bytes) -> None:
    """Persist generated config output when requested or useful."""
    output_path = output_path_for_args(args)
    if output_path:
        output_path.write_bytes(data)
        print(f"Config file: {output_path} ({len(data)} bytes).", file=sys.stderr)
    elif args.keys:
        print("Generated keys are upload-only; use config.json or -o to save them.", file=sys.stderr)


def upload_config(port: str, data: bytes) -> None:
    """Upload config bytes to the firmware provisioning serial port."""
    fd = None
    try:
        # No automatic reset: ESP32-S3 native USB-Serial/JTAG (the Bifrost
        # ProS3 path) interprets `espflash reset` as "host wants to flash"
        # and drops the chip into the ROM bootloader, never reaching our
        # firmware. Both supported firmwares emit the ready marker
        # continuously while unprovisioned, so we just open the port and
        # wait — press RESET manually only if the marker never appears.
        print(
            f"Waiting for provisioning window on {port}. "
            "Press RESET if it does not appear.",
            file=sys.stderr,
        )
        fd, _active_port = wait_for_ready(port)
        print(f"Uploading config ({len(data)} bytes).", file=sys.stderr)
        for offset in range(0, len(data), CHUNK_SIZE):
            write_all(fd, data[offset : offset + CHUNK_SIZE])
            time.sleep(CHUNK_DELAY_SECS)
        print("Upload complete. Waiting for firmware validation.", file=sys.stderr)
        try:
            status, message = wait_for_response(fd)
        except SerialDisconnected as error:
            raise SystemExit(
                "ERROR: device disconnected before confirming the config. "
                "Rerun provisioning and press RESET only when the tool is waiting."
            ) from error
    except SerialDisconnected as error:
        raise SystemExit(
            "ERROR: device disconnected during upload. Rerun provisioning and "
            "press RESET only when the tool is waiting."
        ) from error
    finally:
        close_serial(fd)

    if status != "ok":
        raise SystemExit(f"ERROR: firmware rejected config: {message}")
    print(f"Config {message}.", file=sys.stderr)


def apply_overrides(config: dict[str, Any], args: argparse.Namespace) -> None:
    """Apply command-line overrides to the loaded config."""
    if args.name is not None:
        config["name"] = args.name

    wifi_ssid = args.wifi[0] if args.wifi is not None else None
    wifi_password = args.wifi[1] if args.wifi is not None else None

    if wifi_ssid is not None:
        http_section(config)["wifi_ssid"] = wifi_ssid
    if wifi_password is not None:
        http_section(config)["wifi_password"] = wifi_password
    if args.token is not None:
        http_section(config)["tokens"] = args.token
    if args.no_token:
        http_section(config)["tokens"] = []
    if args.utc_offset_minutes is not None:
        time_section(config)["utc_offset_minutes"] = args.utc_offset_minutes
    if not args.no_time_sync:
        time_section(config)["unix_time_seconds"] = int(time.time())
    if args.keys:
        public_key, private_key = generate_meshcore_keys()
        meshcore = config.get("meshcore")
        if not isinstance(meshcore, dict):
            meshcore = {}
            config["meshcore"] = meshcore
        meshcore["public_key"] = public_key
        meshcore["private_key"] = private_key


def load_config(path: Path) -> dict[str, Any]:
    """Load a JSON or JSONC configuration file."""
    try:
        text = path.read_text(encoding="utf-8")
        value = json.loads(strip_jsonc_comments(text))
    except OSError as error:
        raise SystemExit(f"ERROR: cannot read {path}: {error}") from error
    except json.JSONDecodeError as error:
        raise SystemExit(f"ERROR: {path}:{error.lineno}:{error.colno}: {error.msg}") from error

    if not isinstance(value, dict):
        raise SystemExit("ERROR: config root must be a JSON object")
    return value


def http_section(config: dict[str, Any]) -> dict[str, Any]:
    """Return the HTTP section, creating one when overrides need it."""
    http = config.get("http")
    if not isinstance(http, dict):
        http = {
            "port": 80,
            "tls": False,
            "wifi_ssid": "",
            "wifi_password": "",
            "tokens": [],
        }
        config["http"] = http
    return http


def time_section(config: dict[str, Any]) -> dict[str, Any]:
    """Return the time section, creating one when overrides need it."""
    section = config.get("time")
    if not isinstance(section, dict):
        section = {}
        config["time"] = section
    return section


def validate_config(config: dict[str, Any]) -> None:
    """Reject config that cannot pass firmware validation."""
    expect_text(config, "name", min_len=1, max_len=32)
    validate_http(config.get("http"))
    validate_display(config.get("display"))
    validate_polling(config.get("polling"))
    validate_radio(config.get("radio"))
    validate_time(config.get("time"))
    validate_meshcore(config.get("meshcore"))
    validate_producers(config.get("producers"))


def validate_http(http: Any) -> None:
    """Validate optional HTTP/WiFi configuration."""
    if http is None:
        return
    if not isinstance(http, dict):
        fail("http must be an object or null")
    port = http.get("port", 80)
    if not isinstance(port, int) or not 1 <= port <= 65535:
        fail("http.port must be 1..65535")
    if not isinstance(http.get("tls", False), bool):
        fail("http.tls must be true or false")
    expect_text(http, "wifi_ssid", min_len=1, max_len=32)
    password = http.get("wifi_password", "")
    if password is not None:
        expect_text(http, "wifi_password", min_len=0, max_len=64)
    tokens = http.get("tokens", [])
    if tokens is None:
        return
    if not isinstance(tokens, list) or len(tokens) > 8:
        fail("http.tokens must be null or a list of up to 8 tokens")
    for index, token in enumerate(tokens):
        if not isinstance(token, str) or not 1 <= utf8_len(token) <= 96:
            fail(f"http.tokens[{index}] must be 1..96 UTF-8 bytes")


def validate_display(display: Any) -> None:
    """Validate optional OLED display configuration."""
    if display is None:
        return
    if not isinstance(display, dict):
        fail("display must be an object or null")
    if not isinstance(display.get("enabled"), bool):
        fail("display.enabled must be true or false")
    value = display.get("node_display")
    if value is not None and value not in DISPLAY_VALUES:
        fail("display.node_display has an unsupported value")
    brightness = display.get("brightness_percent", 60)
    if not isinstance(brightness, int) or not 0 <= brightness <= 100:
        fail("display.brightness_percent must be 0..100")


def validate_polling(polling: Any) -> None:
    """Validate global polling policy."""
    if polling is None:
        return
    if not isinstance(polling, dict):
        fail("polling must be an object or null")
    interval = polling.get("default_interval_secs", 600)
    jitter = polling.get("jitter_secs", 10)
    if not isinstance(interval, int) or not 60 <= interval <= 3600:
        fail("polling.default_interval_secs must be 60..3600")
    if not isinstance(jitter, int) or not 0 <= jitter <= 60:
        fail("polling.jitter_secs must be 0..60")


def validate_radio(radio: Any) -> None:
    """Validate LoRa radio parameters accepted by firmware."""
    if radio is None:
        return
    if not isinstance(radio, dict):
        fail("radio must be an object or null")
    positive_int(radio, "frequency_hz", default=910525000)
    if radio.get("bandwidth_hz", 62500) not in RADIO_BANDWIDTH_HZ:
        fail("radio.bandwidth_hz is unsupported")
    int_range(radio, "spreading_factor", default=7, min_value=5, max_value=12)
    int_range(radio, "coding_rate", default=5, min_value=5, max_value=8)
    int_range(radio, "tx_power_level", default=14, min_value=-9, max_value=22)
    positive_int(radio, "preamble_len", default=16)
    if radio.get("tx_ramp_time_us", 200) not in RADIO_RAMP_US:
        fail("radio.tx_ramp_time_us is unsupported")
    if not isinstance(radio.get("iq_inverted", False), bool):
        fail("radio.iq_inverted must be true or false")


def validate_time(time_config: Any) -> None:
    """Validate optional wall-clock display configuration."""
    if time_config is None:
        return
    if not isinstance(time_config, dict):
        fail("time must be an object or null")
    offset = time_config.get("utc_offset_minutes", 0)
    if (
        not isinstance(offset, int)
        or not MIN_UTC_OFFSET_MINUTES <= offset <= MAX_UTC_OFFSET_MINUTES
    ):
        fail(
            "time.utc_offset_minutes must be "
            f"{MIN_UTC_OFFSET_MINUTES}..{MAX_UTC_OFFSET_MINUTES}"
        )
    unix_time = time_config.get("unix_time_seconds")
    if unix_time is not None and (
        not isinstance(unix_time, int) or unix_time < 0
    ):
        fail("time.unix_time_seconds must be a non-negative integer")


def validate_meshcore(meshcore: Any) -> None:
    """Validate gateway MeshCore identity."""
    if not isinstance(meshcore, dict):
        fail("meshcore must be an object")
    expect_text(meshcore, "public_key", min_len=1, max_len=128)
    private_key = meshcore.get("private_key")
    if private_key is not None:
        expect_text(meshcore, "private_key", min_len=1, max_len=128)
    routing = meshcore.get("routing", {})
    if routing is not None:
        if not isinstance(routing, dict):
            fail("meshcore.routing must be an object or null")
        path_mode = routing.get("path_mode", 2)
        if not isinstance(path_mode, int) or not 0 <= path_mode <= 2:
            fail("meshcore.routing.path_mode must be 0, 1, or 2")


def validate_producers(producers: Any) -> None:
    """Validate producer list."""
    if producers is None:
        return
    if not isinstance(producers, list) or len(producers) > 16:
        fail("producers must be a list of up to 16 items")
    seen: set[str] = set()
    for index, producer in enumerate(producers):
        if not isinstance(producer, dict):
            fail(f"producers[{index}] must be an object")
        public_key = expect_text(producer, "public_key", min_len=1, max_len=128)
        if public_key in seen:
            fail(f"producers[{index}].public_key duplicates another producer")
        seen.add(public_key)
        kind = producer.get("kind", "companion")
        if kind not in {"companion", "repeater"}:
            fail(f"producers[{index}].kind must be companion or repeater")
        name = producer.get("name", "")
        if name is not None:
            expect_text(producer, "name", min_len=0, max_len=32)
        password = producer.get("password")
        if password is not None:
            expect_text(producer, "password", min_len=1, max_len=128)
            if kind == "companion":
                fail(f"producers[{index}].password is only valid for repeater producers")
        interval = producer.get("polling_interval_secs")
        if interval is not None and (not isinstance(interval, int) or not 60 <= interval <= 3600):
            fail(f"producers[{index}].polling_interval_secs must be 60..3600")
        route = producer.get("route", "direct")
        if not isinstance(route, str) or not route:
            fail(f"producers[{index}].route must be direct, flood, or a path string")
        if route not in {"direct", "flood"} and utf8_len(route) > 96:
            fail(f"producers[{index}].route path must fit in 96 UTF-8 bytes")


def expect_text(
    section: dict[str, Any],
    field: str,
    *,
    min_len: int,
    max_len: int,
) -> str:
    """Return a required string field after checking UTF-8 byte length."""
    value = section.get(field)
    if not isinstance(value, str):
        fail(f"{field} must be a string")
    length = utf8_len(value)
    if length < min_len or length > max_len:
        fail(f"{field} must be {min_len}..{max_len} UTF-8 bytes")
    return value


def positive_int(section: dict[str, Any], field: str, default: int | None = None) -> None:
    """Validate an optional positive integer field."""
    value = section.get(field, default)
    if not isinstance(value, int) or value <= 0:
        fail(f"{field} must be a positive integer")


def int_range(
    section: dict[str, Any],
    field: str,
    *,
    default: int,
    min_value: int,
    max_value: int,
) -> None:
    """Validate an optional integer range field."""
    value = section.get(field, default)
    if not isinstance(value, int) or not min_value <= value <= max_value:
        fail(f"{field} must be {min_value}..{max_value}")


def utf8_len(value: str) -> int:
    """Return UTF-8 byte length used by firmware fixed strings."""
    return len(value.encode("utf-8"))


def fail(message: str) -> None:
    """Exit with one validation error."""
    raise SystemExit(f"ERROR: invalid config: {message}")


def output_path_for_args(args: argparse.Namespace) -> Path | None:
    """Return the config path that should receive generated output."""
    if args.output:
        return Path(args.output)

    template = Path(args.template)
    if args.keys and template.name == "config.json":
        return template

    return None


def strip_jsonc_comments(text: str) -> str:
    """Remove `//` comments while preserving quoted strings."""
    out: list[str] = []
    in_string = False
    escaped = False
    index = 0

    while index < len(text):
        ch = text[index]
        next_ch = text[index + 1] if index + 1 < len(text) else ""

        if in_string:
            out.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            index += 1
            continue

        if ch == '"':
            in_string = True
            out.append(ch)
            index += 1
            continue

        if ch == "/" and next_ch == "/":
            while index < len(text) and text[index] != "\n":
                index += 1
            continue

        out.append(ch)
        index += 1

    return "".join(out)


def generate_meshcore_keys() -> tuple[str, str]:
    """Generate MeshCore-compatible Ed25519 public and private key hex strings."""
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import ed25519
    except ImportError as error:
        raise SystemExit(
            "ERROR: --keys requires the single optional dependency `cryptography`; "
            "run `uv sync` first."
        ) from error

    seed = os.urandom(32)
    private_key = ed25519.Ed25519PrivateKey.from_private_bytes(seed)
    public_key = private_key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )
    private_key_bytes = meshcore_expanded_private_key(seed)
    return public_key.hex(), private_key_bytes.hex()


def meshcore_expanded_private_key(seed: bytes) -> bytes:
    """Return MeshCore's 64-byte expanded Ed25519 private-key representation."""
    if len(seed) != 32:
        raise ValueError("Ed25519 seed must be 32 bytes")

    digest = bytearray(hashlib.sha512(seed).digest())
    digest[0] &= 248
    digest[31] &= 63
    digest[31] |= 64
    return bytes(digest)


class SerialUnavailable(Exception):
    """Serial port is not currently openable."""


class SerialBusy(Exception):
    """Serial port is already owned by another process."""


class SerialDisconnected(Exception):
    """Serial port was disconnected after it was opened."""


def wait_for_ready(port: str) -> tuple[int, str]:
    """Wait for the firmware config upload prompt and return an open port."""
    start = time.monotonic()
    next_notice = start + 15.0
    fd: int | None = None
    active_port = port
    seen = ""
    last_seen_line: str | None = None
    last_open_error = ""
    reset_notice_printed = False
    notice_count = 0

    try:
        fd, active_port = open_serial(port)
    except SerialBusy as error:
        raise SystemExit(
            f"ERROR: {error} is busy. Close the serial monitor and rerun this command."
        ) from error
    except SerialUnavailable as error:
        last_open_error = str(error)

    print(
        f"Waiting for provisioning window on {port}. Press RESET if it does not appear.",
        file=sys.stderr,
    )

    while time.monotonic() - start < READY_TIMEOUT_SECS:
        if fd is None:
            try:
                fd, active_port = open_serial(port)
            except SerialBusy as error:
                raise SystemExit(
                    f"ERROR: {error} is busy. Close the serial monitor and rerun this command."
                ) from error
            except SerialUnavailable as error:
                last_open_error = str(error)
            else:
                if reset_notice_printed:
                    print("USB serial returned; waiting for config prompt.", file=sys.stderr)
                    reset_notice_printed = False
                seen = ""
                last_open_error = ""

        if fd is not None:
            try:
                text = read_available(fd)
            except SerialDisconnected:
                close_serial(fd)
                fd = None
                seen = ""
                if not reset_notice_printed:
                    print("Device reset detected; waiting for USB serial to return.", file=sys.stderr)
                    reset_notice_printed = True
                time.sleep(0.2)
                continue

            if text:
                seen += text
                last_seen_line = last_output_line(text) or last_seen_line
                if READY_MARKER in seen:
                    print("Provisioning window detected.", file=sys.stderr)
                    return fd, active_port

        now = time.monotonic()
        if now >= next_notice and notice_count < 2:
            message = "Still waiting. Press RESET once if the board is already running."
            if last_open_error and notice_count > 0:
                message += f" Last serial error: {last_open_error}."
            print(message, file=sys.stderr)
            notice_count += 1
            next_notice = now + 25.0

        time.sleep(0.05)

    close_serial(fd)
    ports = ", ".join(known_serial_ports()) or "none"
    detail = f" Last device line: {last_seen_line}" if last_seen_line else ""
    raise SystemExit(
        "ERROR: timed out waiting for firmware config window. "
        "Run this command first, keep serial monitors closed, then press RESET "
        f"while it is waiting. Available serial ports: {ports}.{detail}"
    )


def trigger_reset(port: str) -> bool:
    """Reset an ESP target through cargo-espflash when available."""
    command = [
        "cargo",
        "espflash",
        "reset",
        "--port",
        port,
        "--chip",
        "esp32s3",
        "--non-interactive",
        "--skip-update-check",
    ]
    try:
        result = subprocess.run(
            command,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


def wait_for_response(fd: int) -> tuple[str, str]:
    """Wait for firmware to accept or reject the uploaded config."""
    start = time.monotonic()
    seen = ""
    reported_lines: set[str] = set()
    last_device_line: str | None = None

    while time.monotonic() - start < RESPONSE_TIMEOUT_SECS:
        text = read_available(fd)
        if text:
            seen += text
            for line in relevant_device_lines(text):
                last_device_line = line
                if line not in reported_lines:
                    print(f"Device: {line}", file=sys.stderr)
                    reported_lines.add(line)
            if response := firmware_response(seen):
                return response
        time.sleep(0.05)

    if last_device_line:
        return "error", f"no firmware response after upload; last device line: {last_device_line}"
    return "error", "no firmware response after upload"


def relevant_device_lines(text: str) -> list[str]:
    """Return concise firmware lines worth showing during upload."""
    lines: list[str] = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.startswith("{"):
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("request") == "set_config":
                lines.append(line)
            continue
        if (
            "PANIC" in line
            or "panicked at" in line
            or "Exception:" in line
            or "Backtrace:" in line
            or line.startswith("0x")
            or line.startswith("Status:")
        ):
            lines.append(line)
    return lines


def firmware_response(text: str) -> tuple[str, str] | None:
    """Return firmware provisioning status from serial text."""
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("request") != "set_config":
            continue
        if event.get("type") == "ok":
            action = event.get("action")
            if action == "reboot_required":
                return "ok", "accepted; reboot the device"
            if action:
                return "ok", f"accepted; {action}"
            return "ok", "accepted"
        if event.get("type") == "error":
            return "error", str(event.get("code", "unknown error"))
    return None


def open_serial(port: str) -> tuple[int, str]:
    """Open and configure a serial port."""
    active_port = resolve_serial_port(port)
    try:
        fd = os.open(active_port, os.O_RDWR | os.O_NOCTTY)
    except OSError as error:
        if error.errno == errno.EBUSY:
            raise SerialBusy(active_port) from error
        raise SerialUnavailable(f"{active_port}: {error.strerror}") from error

    try:
        configure_serial(fd)
    except OSError as error:
        os.close(fd)
        if serial_error_is_disconnect(error):
            raise SerialUnavailable(f"{active_port}: {error.strerror}") from error
        raise

    return fd, active_port


def configure_serial(fd: int) -> None:
    """Set a USB serial file descriptor to raw 115200 baud."""
    tty.setraw(fd, termios.TCSANOW)
    attrs = termios.tcgetattr(fd)
    attrs[4] = BAUD_RATE
    attrs[5] = BAUD_RATE
    attrs[6][termios.VMIN] = 0
    attrs[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, attrs)


def close_serial(fd: int | None) -> None:
    """Close a serial descriptor while ignoring stale-device errors."""
    if fd is None:
        return
    try:
        os.close(fd)
    except OSError:
        pass


def read_available(fd: int) -> str:
    """Read currently available serial bytes."""
    chunks: list[bytes] = []
    while True:
        try:
            ready, _, _ = select.select([fd], [], [], 0)
        except OSError as error:
            if serial_error_is_disconnect(error):
                raise SerialDisconnected from error
            raise
        if not ready:
            break
        try:
            chunk = os.read(fd, 4096)
        except OSError as error:
            if serial_error_is_disconnect(error):
                raise SerialDisconnected from error
            raise
        if not chunk:
            break
        chunks.append(chunk)
    return b"".join(chunks).decode("utf-8", errors="replace")


def write_all(fd: int, data: bytes) -> None:
    """Write all bytes to a serial descriptor."""
    view = memoryview(data)
    deadline = time.monotonic() + WRITE_TIMEOUT_SECS
    while view:
        timeout = deadline - time.monotonic()
        if timeout <= 0:
            raise SerialDisconnected
        try:
            _, writable, _ = select.select([], [fd], [], timeout)
        except OSError as error:
            if serial_error_is_disconnect(error):
                raise SerialDisconnected from error
            raise
        if not writable:
            raise SerialDisconnected
        try:
            written = os.write(fd, view)
        except OSError as error:
            if serial_error_is_disconnect(error):
                raise SerialDisconnected from error
            raise
        if written == 0:
            raise SerialDisconnected
        view = view[written:]


def last_output_line(text: str) -> str | None:
    """Return the last non-empty line from device output."""
    for line in reversed(text.splitlines()):
        line = line.strip()
        if line:
            return line
    return None


def serial_error_is_disconnect(error: OSError) -> bool:
    """Return true when an OS error means the USB serial device went away."""
    return error.errno in DISCONNECT_ERRNOS


def known_serial_ports() -> list[str]:
    """Return likely local USB serial device paths."""
    ports: list[str] = []
    for pattern in SERIAL_PORT_PATTERNS:
        ports.extend(glob.glob(pattern))
    return sorted(set(ports))


def discover_serial_port(wait_secs: float) -> str:
    """Return the only likely USB serial port, waiting briefly if needed."""
    start = time.monotonic()
    printed_wait = False
    while True:
        ports = known_serial_ports()
        if len(ports) == 1:
            print(f"Serial port: {ports[0]}", file=sys.stderr)
            return ports[0]
        if len(ports) > 1:
            formatted = "\n  ".join(ports)
            raise SystemExit(
                "ERROR: multiple USB serial ports found; pass one explicitly:\n"
                f"  {formatted}"
            )

        if time.monotonic() - start >= wait_secs:
            raise SystemExit(
                "ERROR: no USB serial port found. Plug in the board or hold BOOT while "
                "connecting USB, then rerun this command."
            )
        if not printed_wait:
            print("Waiting for USB serial port.", file=sys.stderr)
            printed_wait = True
        time.sleep(0.25)


def resolve_serial_port(requested: str) -> str:
    """Use the requested port, or the only matching replacement after reset."""
    if os.path.exists(requested):
        return requested

    candidates = [port for port in known_serial_ports() if port != requested]
    if len(candidates) == 1:
        return candidates[0]
    return requested


if __name__ == "__main__":
    raise SystemExit(main())
