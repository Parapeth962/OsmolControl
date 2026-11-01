"""Tests for ble-bridge.py against a stand-in DirCon server, the way MyWhoosh's shim uses it.

    python3 -I -m unittest discover -s packaging/linux/mywhoosh    (also run by CI)
"""
import importlib.util
import json
import pathlib
import socket
import struct
import threading
import unittest
import uuid

spec = importlib.util.spec_from_file_location(
    "ble_bridge", pathlib.Path(__file__).with_name("ble-bridge.py"))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)

INDOOR_BIKE_DATA = "00002ad2-0000-1000-8000-00805f9b34fb"
CONTROL_POINT = "00002ad9-0000-1000-8000-00805f9b34fb"
HR_MEASUREMENT = "00002a37-0000-1000-8000-00805f9b34fb"


def frame(message_id, seq, code, body):
    return struct.pack(">BBBBH", 1, message_id, seq, code, len(body)) + body


class FakeDirCon:
    """Answers DirCon requests for an FTMS trainer with a Heart Rate service; records writes."""

    TABLE = {bridge.FTMS: [(INDOOR_BIKE_DATA, 0x04), (CONTROL_POINT, 0x0a)],
             bridge.HR_SERVICE: [(HR_MEASUREMENT, 0x04)]}

    def __init__(self):
        self.server = socket.create_server(("127.0.0.1", 0))
        self.address = self.server.getsockname()
        self.writes = []
        self.clients = []
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            try:
                conn, _ = self.server.accept()
            except OSError:
                return
            self.clients.append(conn)
            threading.Thread(target=self.serve, args=(conn,), daemon=True).start()

    def serve(self, conn):
        buffer = b""
        while chunk := conn.recv(4096):
            buffer += chunk
            while len(buffer) >= 6:
                _, message_id, seq, _, length = struct.unpack(">BBBBH", buffer[:6])
                if len(buffer) < 6 + length:
                    break
                body, buffer = buffer[6:6 + length], buffer[6 + length:]
                conn.sendall(self.reply(conn, message_id, seq, body))

    def reply(self, conn, message_id, seq, body):
        if message_id == 0x01:
            return frame(0x01, seq, 0, b"".join(uuid.UUID(s).bytes for s in self.TABLE))
        if message_id == 0x02:
            chars = self.TABLE[str(uuid.UUID(bytes=body[:16]))]
            return frame(0x02, seq, 0, body[:16] + b"".join(
                uuid.UUID(c).bytes + bytes([props]) for c, props in chars))
        if message_id == 0x03:
            return frame(0x03, seq, 0, body[:16] + b"\x2a")
        if message_id == 0x04:
            self.writes.append((str(uuid.UUID(bytes=body[:16])), body[16:]))
            return frame(0x04, seq, 0, body[:16])
        if message_id == 0x05:
            # Subscribed: answer, then send a first value as a trainer would.
            return frame(0x05, seq, 0, body[:16]) + frame(0x06, 0, 0, body[:16] + b"\x44\x00")
        return frame(message_id, seq, 0x01, b"")

    def close(self):
        self.server.close()
        for conn in self.clients:
            conn.close()


class BridgeTest(unittest.TestCase):
    def setUp(self):
        self.dircon = FakeDirCon()
        self.shim, theirs = socket.socketpair()
        self.shim.settimeout(5)
        self.session = bridge.Session(theirs, self.dircon.address, "OsmolControl")
        self.runner = threading.Thread(target=self.session.run, daemon=True)
        self.runner.start()
        self.next_id = 0
        self.unread = b""

    def tearDown(self):
        self.shim.close()
        self.runner.join(5)
        self.dircon.close()

    def receive(self):
        while b"\n" not in self.unread:
            self.unread += self.shim.recv(65536)
        line, self.unread = self.unread.split(b"\n", 1)
        return json.loads(line)

    def call(self, op, **fields):
        """Sends a request; returns its reply and the events that arrived meanwhile."""
        self.next_id += 1
        self.shim.sendall((json.dumps({"id": self.next_id, "op": op, **fields}) + "\n").encode())
        events = []
        while (message := self.receive()).get("id") != self.next_id:
            events.append(message)
        return message, events

    def test_relays_gatt_operations_to_dircon(self):
        reply, _ = self.call("connect", addr=bridge.TRAINER)
        self.assertEqual(reply, {"id": 1, "ok": True, "name": "OsmolControl"})
        reply, _ = self.call("services", addr=bridge.TRAINER)
        self.assertEqual([s["uuid"] for s in reply["services"]], [bridge.FTMS, bridge.HR_SERVICE])
        reply, _ = self.call("chars", addr=bridge.TRAINER, service=bridge.FTMS)
        self.assertEqual(reply["chars"], [
            {"uuid": INDOOR_BIKE_DATA, "flags": ["notify"]},
            {"uuid": CONTROL_POINT, "flags": ["write", "indicate"]}])
        reply, _ = self.call("read", addr=bridge.TRAINER, char=INDOOR_BIKE_DATA)
        self.assertEqual(reply["value"], "2a")
        self.call("write", addr=bridge.TRAINER, char=CONTROL_POINT, value="0001")
        self.assertEqual(self.dircon.writes, [(CONTROL_POINT, b"\x00\x01")])
        reply, events = self.call("notify", addr=bridge.TRAINER, char=INDOOR_BIKE_DATA, enable=True)
        self.assertTrue(reply["ok"])
        if not events:
            events.append(self.receive())
        self.assertEqual(events, [{"ev": "value", "addr": bridge.TRAINER,
                                   "char": INDOOR_BIKE_DATA, "value": "4400"}])

    def test_heart_rate_device_offers_only_its_service(self):
        self.call("connect", addr=bridge.HEART_RATE)
        reply, _ = self.call("services", addr=bridge.HEART_RATE)
        self.assertEqual(reply["services"], [{"uuid": bridge.HR_SERVICE}])

    def test_bad_requests_are_answered_not_fatal(self):
        reply, _ = self.call("services", addr=bridge.TRAINER)
        self.assertEqual(reply, {"id": 1, "ok": False, "error": "not connected"})
        reply, _ = self.call("connect", addr="11:22:33:44:55:66")
        self.assertFalse(reply["ok"])
        self.call("connect", addr=bridge.TRAINER)
        reply, _ = self.call("read", addr=bridge.TRAINER, char="not-a-uuid")
        self.assertFalse(reply["ok"])
        reply, _ = self.call("radios")
        self.assertEqual(reply["radios"], [bridge.RADIO])

    def test_a_closing_old_link_leaves_the_new_one_alone(self):
        self.call("connect", addr=bridge.TRAINER)
        old = self.session.links[bridge.TRAINER]
        self.call("disconnect", addr=bridge.TRAINER)
        self.call("connect", addr=bridge.TRAINER)
        new = self.session.links[bridge.TRAINER]
        self.assertIsNot(old, new)
        # The old session's reader may report its end only now, after the reconnect.
        old.on_closed()
        self.assertIs(self.session.links.get(bridge.TRAINER), new)
        reply, _ = self.call("read", addr=bridge.TRAINER, char=INDOOR_BIKE_DATA)
        self.assertTrue(reply["ok"], reply)

    def test_connect_replaces_a_link_whose_server_went_away(self):
        self.call("connect", addr=bridge.TRAINER)
        dead = self.session.links[bridge.TRAINER]
        dead.closed = True
        self.call("connect", addr=bridge.TRAINER)
        self.assertIsNot(self.session.links[bridge.TRAINER], dead)
        reply, _ = self.call("read", addr=bridge.TRAINER, char=INDOOR_BIKE_DATA)
        self.assertTrue(reply["ok"], reply)

    def test_server_closing_reports_the_disconnect(self):
        self.call("connect", addr=bridge.TRAINER)
        for conn in self.dircon.clients:
            conn.shutdown(socket.SHUT_RDWR)
        self.assertEqual(self.receive(), {"ev": "connection", "addr": bridge.TRAINER,
                                          "connected": False})
        self.assertNotIn(bridge.TRAINER, self.session.links)


if __name__ == "__main__":
    unittest.main()
