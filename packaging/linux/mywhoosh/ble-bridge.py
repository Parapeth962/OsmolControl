#!/usr/bin/env python3
"""Show a DirCon virtual trainer (OsmolControl, qdomyos-zwift, ...) to MyWhoosh under Wine as a
Bluetooth trainer.

MyWhoosh's LAN (DirCon) path cannot work under Wine: it discovers trainers through Bonjour
COM events, which wine-mono does not implement. Its Bluetooth path goes through the Lutris
"bleshim" (MyWhooshShim.dll), which forwards GATT operations as newline-delimited JSON to a
helper on 127.0.0.1:27019. This is that helper, but instead of BlueZ it offers a virtual
device and relays every GATT operation to a DirCon server on this PC, which already presents the
trainer's GATT table. The real Bluetooth adapter stays with the DirCon app alone.

When the DirCon table also has a Heart Rate service (OsmolControl with a heart rate strap), a
second virtual device "<name> HR" offers it, so MyWhoosh pairs it as a separate heart rate
sensor. Each virtual device has its own DirCon session, so unpairing one never drops the other.

Shim requests:  {"id":N,"op":"radios"|"scan"|"connect"|"disconnect"|"services"|"chars"|
                 "read"|"write"|"notify", ...}
Replies:        {"id":N,"ok":true,...} or {"id":N,"ok":false,"error":"..."}
Events:         {"ev":"advert"|"value"|"connection", ...}

Usage: ble-bridge.py [--dircon HOST:PORT] [--name NAME] [--port 27019]
"""
import argparse
import json
import socket
import struct
import threading
import time
import uuid

TRAINER = "0E:57:00:00:00:01"
HEART_RATE = "0E:57:00:00:00:02"
FTMS = "00001826-0000-1000-8000-00805f9b34fb"
CYCLING_POWER = "00001818-0000-1000-8000-00805f9b34fb"
HR_SERVICE = "0000180d-0000-1000-8000-00805f9b34fb"
RADIO = {"name": "hci0", "address": TRAINER, "powered": True}
PROBE_INTERVAL = 15.0
DIRCON_PORT = 36866
TIMEOUT = 5.0
FLAGS = [(0x01, "read"), (0x02, "write"), (0x04, "notify"), (0x08, "indicate")]


def log(message):
    print(f"[{time.strftime('%H:%M:%S')} ble-bridge] {message}", flush=True)


def lan_address():
    """The address DirCon apps serve on: the source of the default route."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.connect(("192.0.2.1", 9))
        return s.getsockname()[0]


class BridgeError(Exception):
    pass


class DirCon:
    """One DirCon TCP session: numbered requests, and notifications passed to `on_value`."""

    def __init__(self, host, port, on_value, on_closed):
        self.sock = socket.create_connection((host, port), timeout=TIMEOUT)
        self.sock.settimeout(None)
        self.on_value = on_value
        self.on_closed = on_closed
        self.lock = threading.Lock()
        self.seq = 0
        self.pending = {}
        threading.Thread(target=self.read_loop, daemon=True).start()

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()

    def request(self, message_id, payload=b""):
        with self.lock:
            self.seq = (self.seq + 1) % 256 or 1
            seq = self.seq
            waiter = {"event": threading.Event()}
            self.pending[seq] = waiter
            self.sock.sendall(struct.pack(">BBBBH", 1, message_id, seq, 0, len(payload)) + payload)
        if not waiter["event"].wait(TIMEOUT):
            self.pending.pop(seq, None)
            raise BridgeError(f"DirCon message {message_id:#04x} timed out")
        code, body = waiter["reply"]
        if code != 0:
            raise BridgeError(f"DirCon message {message_id:#04x} failed with code {code:#04x}")
        return body

    def read_loop(self):
        buffer = b""
        try:
            while chunk := self.sock.recv(65536):
                buffer += chunk
                while len(buffer) >= 6:
                    _, message_id, seq, code, length = struct.unpack(">BBBBH", buffer[:6])
                    if len(buffer) < 6 + length:
                        break
                    body, buffer = buffer[6:6 + length], buffer[6 + length:]
                    if message_id == 0x06 and len(body) >= 16:
                        self.on_value(str(uuid.UUID(bytes=body[:16])), body[16:])
                    elif (waiter := self.pending.pop(seq, None)) is not None:
                        waiter["reply"] = (code, body)
                        waiter["event"].set()
        except OSError:
            pass
        self.on_closed()


def service_list(body):
    return [str(uuid.UUID(bytes=body[i:i + 16])) for i in range(0, len(body) - 15, 16)]


class Session:
    """The shim's connection: answers its requests and forwards events to it."""

    def __init__(self, conn, dircon_address, name):
        self.conn = conn
        self.dircon_address = dircon_address
        self.name = name
        self.send_lock = threading.Lock()
        self.links = {}                     # virtual device address -> DirCon session
        self.scanning = False
        self.has_hr = False
        self.probed_at = 0.0

    def send(self, message):
        data = (json.dumps(message) + "\n").encode()
        with self.send_lock:
            try:
                self.conn.sendall(data)
            except OSError:
                pass

    def dircon_up(self):
        try:
            socket.create_connection(self.dircon_address, timeout=1).close()
            return True
        except OSError:
            return False

    def probe(self):
        """Whether the DirCon table has a Heart Rate service, rechecked now and then; a ride
        restarted with or without a strap changes it. Uses a session open for a device if any."""
        if time.monotonic() - self.probed_at < PROBE_INTERVAL:
            return
        self.probed_at = time.monotonic()
        try:
            link = next(iter(self.links.values()), None)
            if link is not None:
                body = link.request(0x01)
            else:
                link = DirCon(*self.dircon_address, lambda *_: None, lambda: None)
                try:
                    body = link.request(0x01)
                finally:
                    link.close()
        except (BridgeError, OSError):
            return
        has_hr = HR_SERVICE in service_list(body)
        if has_hr != self.has_hr:
            log("heart rate sensor " + ("offered" if has_hr else "gone"))
        self.has_hr = has_hr

    def advertise(self):
        """Advertise while the game scans and the DirCon server is reachable."""
        while self.scanning:
            if self.links or self.dircon_up():
                self.probe()
                self.send({"ev": "advert", "addr": TRAINER, "name": self.name,
                           "uuids": [FTMS, CYCLING_POWER], "rssi": -40})
                if self.has_hr:
                    self.send({"ev": "advert", "addr": HEART_RATE, "name": f"{self.name} HR",
                               "uuids": [HR_SERVICE], "rssi": -40})
            time.sleep(2)

    def on_value(self, addr, char, value):
        self.send({"ev": "value", "addr": addr, "char": char, "value": value.hex()})

    def on_closed(self, addr):
        if self.links.pop(addr, None) is not None:
            log(f"DirCon connection of {addr} closed")
            self.send({"ev": "connection", "addr": addr, "connected": False})

    def link(self, addr):
        if addr not in self.links:
            raise BridgeError("not connected")
        return self.links[addr]

    def dispatch(self, op, req):
        if op == "radios":
            return {"radios": [RADIO]}
        if op == "scan":
            enable = bool(req.get("enable"))
            if enable and not self.scanning:
                self.scanning = True
                threading.Thread(target=self.advertise, daemon=True).start()
            elif not enable:
                self.scanning = False
            return {}
        addr = (req.get("addr") or "").upper()
        if addr not in (TRAINER, HEART_RATE):
            raise BridgeError("unknown device")
        name = self.name if addr == TRAINER else f"{self.name} HR"
        if op == "connect":
            if addr not in self.links:
                try:
                    self.links[addr] = DirCon(*self.dircon_address,
                                              lambda c, v: self.on_value(addr, c, v),
                                              lambda: self.on_closed(addr))
                except OSError as e:
                    raise BridgeError(f"no DirCon trainer at {self.dircon_address}: {e}")
                log(f"{name}: connected to the DirCon server")
            return {"name": name}
        if op == "disconnect":
            if (d := self.links.pop(addr, None)) is not None:
                d.close()
            return {}
        if op == "services":
            services = service_list(self.link(addr).request(0x01))
            if addr == HEART_RATE:
                services = [s for s in services if s == HR_SERVICE]
            return {"services": [{"uuid": s} for s in services]}
        char = req.get("char") or ""
        if op == "chars":
            service = uuid.UUID(req.get("service") or "").bytes
            body = self.link(addr).request(0x02, service)[16:]
            chars = []
            for i in range(0, len(body) - 16, 17):
                props = body[i + 16]
                chars.append({"uuid": str(uuid.UUID(bytes=body[i:i + 16])),
                              "flags": [name for bit, name in FLAGS if props & bit]})
            return {"chars": chars}
        if op == "read":
            return {"value": self.link(addr).request(0x03, uuid.UUID(char).bytes)[16:].hex()}
        if op == "write":
            value = bytes.fromhex(req.get("value") or "")
            self.link(addr).request(0x04, uuid.UUID(char).bytes + value)
            return {}
        if op == "notify":
            on = 1 if req.get("enable") else 0
            self.link(addr).request(0x05, uuid.UUID(char).bytes + bytes([on]))
            return {}
        raise BridgeError(f"unknown op {op!r}")

    def handle(self, line):
        try:
            req = json.loads(line)
        except ValueError:
            return
        try:
            reply = self.dispatch(req.get("op"), req)
            reply.update(ok=True)
        except (BridgeError, ValueError, OSError) as e:
            log(f"{req.get('op')}: {e}")
            reply = {"ok": False, "error": str(e)}
        reply["id"] = req.get("id")
        self.send(reply)

    def run(self):
        buffer = b""
        try:
            while chunk := self.conn.recv(65536):
                buffer += chunk
                while b"\n" in buffer:
                    line, buffer = buffer.split(b"\n", 1)
                    if line.strip():
                        self.handle(line)
        except OSError:
            pass
        self.scanning = False
        for d in list(self.links.values()):
            d.close()
        self.links.clear()
        self.conn.close()


def main():
    ap = argparse.ArgumentParser(description="MyWhoosh Bluetooth shim -> DirCon trainer")
    ap.add_argument("--port", type=int, default=27019)
    ap.add_argument("--name", default="DirCon Trainer", help="device name shown in MyWhoosh")
    ap.add_argument("--dircon", help=f"HOST:PORT (default: LAN address:{DIRCON_PORT})")
    args = ap.parse_args()
    if args.dircon:
        host, _, port = args.dircon.rpartition(":")
        dircon = (host, int(port))
    else:
        dircon = (lan_address(), DIRCON_PORT)
    server = socket.create_server(("127.0.0.1", args.port))
    log(f"listening on 127.0.0.1:{args.port}, DirCon trainer at {dircon[0]}:{dircon[1]}")
    while True:
        conn, _ = server.accept()
        log("MyWhoosh connected")
        Session(conn, dircon, args.name).run()
        log("MyWhoosh disconnected")


if __name__ == "__main__":
    main()
