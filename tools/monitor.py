#!/usr/bin/env python3
"""ESP32-S3 serial monitor for Muninn Gate firmware.

A robust serial monitoring tool designed for both interactive use and
automated CI workflows. Replaces `espflash monitor` which fails
in non-interactive contexts ("Failed to initialize input reader").

Defaults target the Bifrost ProS3 (native USB-Serial/JTAG on /dev/ttyACM0);
pass `--port auto` for first-available, or an explicit path otherwise.

Features:
  - Timed capture with automatic exit (--duration)
  - Log-level colorization (INFO/WARN/ERROR/PANIC)
  - Pattern filtering (--filter) to show only matching lines
  - Wait-for-pattern mode (--wait-for) exits 0 when pattern seen
  - Log-to-file (--log) for post-mortem analysis
  - Auto-detect USB JTAG serial port (--port auto)
  - Line timestamps (--timestamps)
  - End-of-run summary with event counts
  - Reconnect on USB disconnect (--reconnect)
  - Chip reset via DTR/RTS toggle (--reset)

Examples:
  # Basic 30s capture:
  python3 tools/monitor.py -d 30

  # Wait for WiFi IP (timeout 60s):
  python3 tools/monitor.py --wait-for "Got IP" -d 60

  # Filter DHCP and WiFi logs, save to file:
  python3 tools/monitor.py --filter "DHCP|WIFI" --log /tmp/wifi.log -d 120

  # Auto-detect port, colorize, run forever:
  python3 tools/monitor.py --port auto

  # Reset chip and capture boot sequence:
  python3 tools/monitor.py --reset -d 30

Exit codes:
  0  Normal exit (duration elapsed, or --wait-for pattern found)
  1  Fatal error (port not found, permission denied)
  2  Timeout without seeing --wait-for pattern
  3  Keyboard interrupt
"""

import argparse
import glob
import os
import re
import sys
import time


def find_serial_port():
    """Auto-detect ESP32-S3 USB JTAG serial port."""
    candidates = sorted(glob.glob("/dev/ttyACM*")) + sorted(glob.glob("/dev/ttyUSB*"))
    if not candidates:
        return None
    return candidates[0]


def colorize(line):
    """Add ANSI color codes based on log level."""
    if not sys.stdout.isatty():
        return line
    # Bright red for panics/crashes
    if "panic" in line.lower() or "EXCVADDR" in line or "Backtrace" in line:
        return f"\033[1;31m{line}\033[0m"
    # Red for errors
    if "[ERROR]" in line or "Error" in line:
        return f"\033[31m{line}\033[0m"
    # Yellow for warnings
    if "[WARN]" in line or "WARN" in line:
        return f"\033[33m{line}\033[0m"
    # Green for success indicators
    if "Got IP" in line or "[API] OK" in line or "DHCP recv Ack" in line:
        return f"\033[32m{line}\033[0m"
    # Cyan for WiFi/DHCP
    if "[WIFI]" in line or "[DHCP]" in line:
        return f"\033[36m{line}\033[0m"
    # Magenta for TX/RX radio
    if "[TX]" in line or "[RX]" in line or "REPEATER" in line:
        return f"\033[35m{line}\033[0m"
    return line


class MonitorStats:
    """Track event counts during monitoring session."""
    __slots__ = (
        "lines", "errors", "warnings", "panics",
        "wifi_connects", "dhcp_acks", "api_ok", "api_fail",
        "tx_count", "rx_count", "start_time",
    )

    def __init__(self):
        self.lines = 0
        self.errors = 0
        self.warnings = 0
        self.panics = 0
        self.wifi_connects = 0
        self.dhcp_acks = 0
        self.api_ok = 0
        self.api_fail = 0
        self.tx_count = 0
        self.rx_count = 0
        self.start_time = time.time()

    def count(self, line):
        self.lines += 1
        if "[ERROR]" in line or "Error" in line:
            self.errors += 1
        if "[WARN]" in line or "WARN" in line:
            self.warnings += 1
        if "panic" in line.lower() or "EXCVADDR" in line:
            self.panics += 1
        if "WIFI connected" in line or "Wifi connected" in line:
            self.wifi_connects += 1
        if "DHCP recv Ack" in line or "Got IP" in line:
            self.dhcp_acks += 1
        if "[API] OK" in line:
            self.api_ok += 1
        if "[API] FAIL" in line or "[API] ERR" in line:
            self.api_fail += 1
        if "[TX]" in line:
            self.tx_count += 1
        if "[RX]" in line:
            self.rx_count += 1

    def summary(self):
        elapsed = time.time() - self.start_time
        parts = [
            f"  duration: {elapsed:.1f}s",
            f"  lines:    {self.lines}",
        ]
        if self.errors:
            parts.append(f"  errors:   {self.errors}")
        if self.warnings:
            parts.append(f"  warnings: {self.warnings}")
        if self.panics:
            parts.append(f"  PANICS:   {self.panics}")
        if self.wifi_connects:
            parts.append(f"  wifi:     {self.wifi_connects} connect(s)")
        if self.dhcp_acks:
            parts.append(f"  dhcp:     {self.dhcp_acks} ack(s)")
        if self.api_ok or self.api_fail:
            parts.append(f"  api:      {self.api_ok} ok, {self.api_fail} fail")
        if self.tx_count or self.rx_count:
            parts.append(f"  radio:    {self.tx_count} tx, {self.rx_count} rx")
        return "\n".join(parts)


def main():
    parser = argparse.ArgumentParser(
        description="ESP32-S3 serial monitor for Muninn Gate firmware",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""examples:
  %(prog)s -d 30                          # capture 30 seconds
  %(prog)s --wait-for "Got IP" -d 60      # exit when IP obtained
  %(prog)s --filter "DHCP|WIFI" --log x   # filter + log to file
  %(prog)s --port auto --reset            # auto-detect, reset chip""",
    )
    parser.add_argument(
        "--port", "-p", default="/dev/ttyACM0",
        help="Serial port, or 'auto' to detect (default: /dev/ttyACM0)",
    )
    parser.add_argument(
        "--duration", "-d", type=int, default=60,
        help="Seconds to monitor; 0 = unlimited (default: 60)",
    )
    parser.add_argument(
        "--reset", "-r", action="store_true",
        help="Reset chip via DTR/RTS toggle before monitoring",
    )
    parser.add_argument(
        "--baud", "-b", type=int, default=115200,
        help="Baud rate (ignored for USB JTAG, default: 115200)",
    )
    parser.add_argument(
        "--filter", "-f",
        help="Regex pattern — only print matching lines (e.g. 'DHCP|WIFI|API')",
    )
    parser.add_argument(
        "--wait-for", "-w", dest="wait_for",
        help="Exit with code 0 when this pattern appears (e.g. 'Got IP')",
    )
    parser.add_argument(
        "--log", "-l", dest="logfile",
        help="Append all output (unfiltered) to this file",
    )
    parser.add_argument(
        "--timestamps", "-t", action="store_true",
        help="Prefix each line with a wall-clock timestamp",
    )
    parser.add_argument(
        "--reconnect", action="store_true",
        help="Reconnect automatically if the serial port disconnects",
    )
    parser.add_argument(
        "--no-color", action="store_true",
        help="Disable ANSI color output",
    )
    parser.add_argument(
        "--no-summary", action="store_true",
        help="Suppress end-of-run summary",
    )
    args = parser.parse_args()

    try:
        import serial
    except ImportError:
        print("pyserial not installed. Run: pip install pyserial", file=sys.stderr)
        sys.exit(1)

    # Resolve port
    port = args.port
    if port == "auto":
        port = find_serial_port()
        if not port:
            print("No serial port found (tried /dev/ttyACM*, /dev/ttyUSB*)", file=sys.stderr)
            sys.exit(1)
        print(f"[monitor] auto-detected port: {port}", file=sys.stderr)

    # Compile filter patterns
    line_filter = re.compile(args.filter) if args.filter else None
    wait_pattern = re.compile(args.wait_for) if args.wait_for else None

    # Open log file
    logfh = None
    if args.logfile:
        logfh = open(args.logfile, "a", encoding="utf-8", errors="replace")
        print(f"[monitor] logging to {args.logfile}", file=sys.stderr)

    stats = MonitorStats()
    exit_code = 0
    wait_found = False

    def open_port():
        try:
            s = serial.Serial(port, args.baud, timeout=0.5)
            if args.reset:
                s.dtr = False
                s.rts = True
                time.sleep(0.1)
                s.rts = False
                time.sleep(0.1)
                s.reset_input_buffer()
            return s
        except serial.SerialException as e:
            return e

    ser = open_port()
    if isinstance(ser, Exception):
        print(f"[monitor] cannot open {port}: {ser}", file=sys.stderr)
        sys.exit(1)

    end_time = time.time() + args.duration if args.duration > 0 else float("inf")
    partial = b""

    try:
        while time.time() < end_time:
            try:
                data = ser.read(4096)
            except (serial.SerialException, OSError):
                if not args.reconnect:
                    print("\n[monitor] serial disconnected", file=sys.stderr)
                    break
                print("\n[monitor] disconnected, reconnecting...", file=sys.stderr)
                ser.close()
                time.sleep(1)
                while time.time() < end_time:
                    ser = open_port()
                    if not isinstance(ser, Exception):
                        print("[monitor] reconnected", file=sys.stderr)
                        break
                    time.sleep(2)
                else:
                    break
                if isinstance(ser, Exception):
                    break
                continue

            if not data:
                continue

            # Split into lines, keeping a partial buffer for incomplete reads.
            chunks = (partial + data).split(b"\n")
            partial = chunks.pop()  # last element may be incomplete

            for raw_line in chunks:
                line = raw_line.decode("utf-8", errors="replace").rstrip("\r")
                stats.count(line)

                # Always log unfiltered
                if logfh:
                    ts = time.strftime("%H:%M:%S") if args.timestamps else ""
                    prefix = f"[{ts}] " if ts else ""
                    logfh.write(f"{prefix}{line}\n")

                # Check wait-for pattern
                if wait_pattern and wait_pattern.search(line):
                    wait_found = True

                # Apply display filter
                if line_filter and not line_filter.search(line):
                    continue

                # Format and print
                if args.timestamps:
                    line = f"[{time.strftime('%H:%M:%S')}] {line}"
                if not args.no_color:
                    line = colorize(line)
                print(line)

                if wait_found:
                    # Print the matching line, then exit
                    raise _WaitPatternFound()

    except KeyboardInterrupt:
        exit_code = 3
    except _WaitPatternFound:
        exit_code = 0
    finally:
        try:
            ser.close()
        except Exception:
            pass
        if logfh:
            logfh.close()

    # Summary
    if not args.no_summary:
        print("\n--- monitor summary ---", file=sys.stderr)
        print(stats.summary(), file=sys.stderr)

    # Exit code for --wait-for
    if args.wait_for and not wait_found:
        exit_code = 2

    sys.exit(exit_code)


class _WaitPatternFound(Exception):
    """Sentinel to break out of nested loops when --wait-for matches."""
    pass


if __name__ == "__main__":
    main()
