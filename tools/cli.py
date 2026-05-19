#!/usr/bin/env python3
"""Muninn Gate host-side CLI.

Install the tool environment:

    uv sync

Validate and upload a config when the firmware provisioning window opens:

    uv run python tools/cli.py config.json /dev/cu.usbmodemXXXX

Generate/persist MeshCore gateway keys, set WiFi, and upload:

    uv run python tools/cli.py config.json /dev/cu.usbmodemXXXX \
      --keys --wifi "wifi-name" "wifi-password"

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
HANDSHAKE_TIMEOUT_SECS = 1.5
WRITE_TIMEOUT_SECS = 5.0
READY_MARKER = r'{"type":"ready","request":"set_config"}'
STATUS_REQUEST = b'{"type":"status"}\n'
SERIAL_PORT_PATTERNS = (
    "/dev/cu.usbmodem*",
    "/dev/cu.usbserial*",
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


def main() -> int:
    """Run the CLI."""
    args = build_parser().parse_args()
    data = prepare_config(args)
    write_output(args, data)
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
    parser.add_argument("--output", "-o", help="write final JSON to this path")
    return parser


def prepare_config(args: argparse.Namespace) -> bytes:
    """Load, update, validate, and serialize the requested config."""
    config = load_config(Path(args.template))
    apply_overrides(config, args)
    return json.dumps(config, indent=2).encode("utf-8") + b"\n"


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
        if trigger_reset(port):
            print("Reset sent; waiting for provisioning window.", file=sys.stderr)
        else:
            print("Automatic reset failed; press RESET when prompted.", file=sys.stderr)
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
                    if verify_live_config_receiver(fd):
                        print("Provisioning window detected.", file=sys.stderr)
                        return fd, active_port
                    seen = ""
                    last_seen_line = "stale provisioning prompt ignored"

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


def verify_live_config_receiver(fd: int) -> bool:
    """Return true when firmware responds to a live provisioning command."""
    try:
        write_all(fd, STATUS_REQUEST)
    except SerialDisconnected:
        raise
    except OSError:
        return False

    start = time.monotonic()
    seen = ""
    while time.monotonic() - start < HANDSHAKE_TIMEOUT_SECS:
        text = read_available(fd)
        if text:
            seen += text
            if firmware_status(seen):
                return True
        time.sleep(0.05)
    return False


def firmware_status(text: str) -> bool:
    """Return true when serial text contains a firmware status response."""
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "status":
            return True
    return False


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
        termios.tcflush(fd, termios.TCIOFLUSH)
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
