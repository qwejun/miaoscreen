"""A stand-in publisher, for testing the datagram channel without the product.

Binds a control socket and a datagram socket on adjacent ports, answers the
device's announcement, and streams empty frames as slices. Everything it prints
is evidence: the device's own counters come back over the control socket, so a
line saying `frames=37` is the device telling us it assembled thirty-seven
frames from datagrams.

    python scripts/dev/udp-probe.py --port 17400
    adb shell am start -n com.bongocat.catscreen/.MainActivity \
        --es address 192.168.31.87:17400

The device opens the datagram port one above the control port, which is why the
default here is 17401.
"""

from __future__ import annotations

import argparse
import socket
import struct
import threading
import time

PROTOCOL_VERSION = 1
KIND_HELLO = 0x10
KIND_FRAME = 0x12
KIND_LINK_STATS = 0x1A
KIND_FRAME_MARK = 0x1B

MAGIC = b"MCAT"
DATAGRAM_REGISTER = 1
DATAGRAM_REGISTER_ACK = 2
DATAGRAM_FRAME_SLICE = 3
DATAGRAM_CLEAR = 4
DATAGRAM_PAYLOAD = 1180


def message(kind: int, payload: bytes) -> bytes:
    return bytes([kind]) + struct.pack("<I", len(payload)) + payload


def empty_frame(generation: int, sequence: int) -> bytes:
    """A frame with no drawables: the panel goes blank, which is the point."""
    payload = struct.pack("<I", PROTOCOL_VERSION)
    payload += struct.pack("<Q", generation)
    payload += struct.pack("<Q", sequence)
    payload += struct.pack("<f", 1.0)          # model opacity
    payload += bytes([0])                      # mirror
    payload += struct.pack("<ffff", -1.0, 1.0, -1.0, 1.0)  # bounds
    payload += struct.pack("<I", 0)             # active keys
    payload += struct.pack("<I", 0)             # drawables
    return payload


def slices(channel_sequence: int, payload: bytes) -> list[bytes]:
    total = len(payload)
    count = max(1, (total + DATAGRAM_PAYLOAD - 1) // DATAGRAM_PAYLOAD)
    out = []
    for index in range(count):
        body = payload[index * DATAGRAM_PAYLOAD : (index + 1) * DATAGRAM_PAYLOAD]
        out.append(
            MAGIC
            + bytes([DATAGRAM_FRAME_SLICE, PROTOCOL_VERSION])
            + b"\x00\x00"
            + struct.pack("<Q", channel_sequence)
            + struct.pack("<I", total)
            + struct.pack("<H", index)
            + struct.pack("<H", count)
            + body
        )
    return out


class Probe:
    def __init__(self, port: int) -> None:
        self.port = port
        self.peer: tuple[str, int] | None = None
        self.registrations = 0
        self.accepted = 0
        self.stats: tuple[int, int, int] | None = None
        self.control: socket.socket | None = None

    def serve_control(self) -> None:
        server = socket.socket()
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(("0.0.0.0", self.port))
        server.listen(1)
        print(f"control listening on {self.port}", flush=True)
        while True:
            connection, peer = server.accept()
            print(f"control connection from {peer[0]}:{peer[1]}", flush=True)
            self.accepted += 1
            self.control = connection
            connection.sendall(
                message(KIND_HELLO, struct.pack("<I", PROTOCOL_VERSION) + struct.pack("<Q", 0))
            )
            threading.Thread(target=self.read_control, args=(connection,), daemon=True).start()

    def read_control(self, connection: socket.socket) -> None:
        header = b""
        while True:
            try:
                while len(header) < 5:
                    chunk = connection.recv(5 - len(header))
                    if not chunk:
                        return
                    header += chunk
                kind = header[0]
                length = struct.unpack("<I", header[1:5])[0]
                body = b""
                while len(body) < length:
                    chunk = connection.recv(length - len(body))
                    if not chunk:
                        return
                    body += chunk
                header = b""
                if kind == KIND_LINK_STATS and len(body) >= 21:
                    frames, received, wants_tcp = struct.unpack("<QQB", body[4:21])
                    self.stats = (frames, received, wants_tcp)
                    print(
                        f"device says frames={frames} datagrams={received} "
                        f"want_control={wants_tcp}",
                        flush=True,
                    )
                else:
                    print(f"control message kind=0x{kind:02x} length={length}", flush=True)
            except OSError:
                return

    def serve_datagram(self) -> None:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("0.0.0.0", self.port + 1))
        sock.settimeout(0.5)
        print(f"datagrams listening on {self.port + 1}", flush=True)
        while True:
            try:
                packet, peer = sock.recvfrom(2048)
            except socket.timeout:
                continue
            if len(packet) < 16 or packet[:4] != MAGIC:
                continue
            if packet[4] == DATAGRAM_REGISTER:
                self.registrations += 1
                self.peer = peer
                nonce = packet[8:16]
                sock.sendto(
                    MAGIC + bytes([DATAGRAM_REGISTER_ACK, PROTOCOL_VERSION]) + b"\x00\x00" + nonce,
                    peer,
                )
                if self.registrations <= 3 or self.registrations % 20 == 0:
                    print(f"announcement #{self.registrations} from {peer[0]}:{peer[1]}", flush=True)

    def stream(self, interval: float) -> None:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sequence = 0
        last_mark = 0.0
        while True:
            time.sleep(interval)
            if self.peer is None:
                continue
            sequence += 1
            payload = empty_frame(1, sequence)
            for packet in slices(sequence, payload):
                sock.sendto(packet, self.peer)
            now = time.time()
            if now - last_mark >= 1.0 and self.control is not None:
                last_mark = now
                try:
                    self.control.sendall(
                        message(KIND_FRAME_MARK, struct.pack("<I", PROTOCOL_VERSION) + struct.pack("<Q", sequence))
                    )
                except OSError:
                    self.control = None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=17400)
    parser.add_argument("--interval", type=float, default=0.05)
    args = parser.parse_args()

    probe = Probe(args.port)
    threading.Thread(target=probe.serve_datagram, daemon=True).start()
    threading.Thread(target=probe.stream, args=(args.interval,), daemon=True).start()
    probe.serve_control()


if __name__ == "__main__":
    main()
