#!/usr/bin/env python3
# Mock AIS receiver output (!AIVDM sentences) for testing aisnomon.
#   mockais.py [options] tcp PORT        TCP server, every client gets the stream
#   mockais.py [options] udp HOST:PORT   send datagrams (unicast or multicast,
#                                        IPv4 or [IPv6]:PORT)
#   mockais.py [options] pty             pseudo-terminal; prints the slave path
#   mockais.py [options] stdout          write to stdout (e.g. a capture file)
#
# Traffic: class A vessels (type 1/3, type 5 in two parts), class B vessels
# (type 18, type 24 A/B), one base station (type 4) and AtoNs (type 21),
# alternating between channels A and B. Faults can be injected.
import argparse, heapq, os, random, socket, sys, threading, time

ap = argparse.ArgumentParser()
ap.add_argument("--vessels", type=int, default=30, help="number of vessels (default 30)")
ap.add_argument("--class-b", type=float, default=0.3, help="fraction of class B vessels")
ap.add_argument("--speedup", type=float, default=1.0, help="time compression factor")
ap.add_argument("--ck-errors", type=float, default=0.0, help="probability of a bad checksum")
ap.add_argument("--drop-frag", type=float, default=0.0, help="probability of losing a fragment")
ap.add_argument("--noise", type=float, default=0.0, help="probability of a garbage line per message")
ap.add_argument("--tag", action="store_true", help="prefix tag blocks (s:, c:)")
ap.add_argument("--udpbc", action="store_true",
                help="udp only: IEC 61162-450 style datagrams, one sentence each, "
                     "with a UdPbC header and an s:/n: tag block")
ap.add_argument("--own", action="store_true", help="also emit own-ship !AIVDO every 10 s")
ap.add_argument("--fade", type=float, default=0.0,
                help="seconds per cycle in which channel B fades out and back (0 = off)")
ap.add_argument("--duration", type=float, default=0.0, help="stop after N seconds (simulated)")
ap.add_argument("--seed", type=int, default=None)
ap.add_argument("mode", choices=["tcp", "udp", "pty", "stdout"])
ap.add_argument("where", nargs="?", default="")
a = ap.parse_args()
if a.udpbc and a.mode != "udp":
    ap.error("--udpbc only applies to udp")
rng = random.Random(a.seed)


def armor(bits):
    """Bit string -> (6-bit armored payload, fill bits)."""
    fill = (-len(bits)) % 6
    bits += "0" * fill
    out = []
    for i in range(0, len(bits), 6):
        v = int(bits[i:i + 6], 2)
        out.append(chr(v + 48 if v < 40 else v + 56))
    return "".join(out), fill


def message(mtype, mmsi, nbits):
    head = format(mtype, "06b") + "00" + format(mmsi, "030b")
    body = "".join(rng.choice("01") for _ in range(nbits - len(head)))
    return head + body


def checksum(body):
    c = 0
    for b in body.encode():
        c ^= b
    return c


seq = 0
line_no = 0


def sentences(bits, chan, own=False):
    """Encapsulate a message; long payloads are split at 60 characters."""
    global seq, line_no
    payload, fill = armor(bits)
    parts = [payload[i:i + 60] for i in range(0, len(payload), 60)]
    sid = ""
    if len(parts) > 1:
        sid = str(seq)
        seq = (seq + 1) % 10
    out = []
    for n, p in enumerate(parts, 1):
        f = fill if n == len(parts) else 0
        body = "AIVD%s,%d,%d,%s,%s,%s,%d" % ("O" if own else "M", len(parts), n, sid,
                                          "" if own else chan, p, f)
        ck = checksum(body)
        if rng.random() < a.ck_errors:
            ck ^= 1 << rng.randrange(8)
        if rng.random() < a.drop_frag and len(parts) > 1:
            continue
        line = "!%s*%02X" % (body, ck)
        fields = []
        if a.udpbc:
            line_no = line_no % 999 + 1
            fields += ["s:AI0001", "n:%d" % line_no]
        elif a.tag:
            fields.append("s:mockais")
        if a.tag:
            fields.append("c:%d" % int(sim_now()))
        if fields:
            tb = ",".join(fields)
            line = "\\%s*%02X\\%s" % (tb, checksum(tb), line)
        out.append(line)
    if rng.random() < a.noise:
        out.append("".join(chr(rng.randrange(32, 127)) for _ in range(rng.randrange(5, 60))))
    return out


t0 = time.time()


def sim_now():
    return t0 + (time.time() - t0) * a.speedup


def mid():
    return rng.choice([244, 245, 246, 211, 218, 235, 257, 265, 431, 563])


# Stations: (next_time, id, kind, mmsi, period)
stations = []
for i in range(a.vessels):
    mmsi = mid() * 1_000_000 + rng.randrange(1_000_000)
    kind = "B" if rng.random() < a.class_b else "A"
    period = rng.choice([2, 3.3, 6, 10]) if kind == "A" else 30
    stations.append([rng.uniform(0, period), i, kind, mmsi, period])
    stations.append([rng.uniform(0, 360), i + 10_000, kind + "static", mmsi, 360])
base = 2_000_000 + mid() * 1000 + rng.randrange(1000)
stations.append([0.5, 20_000, "base", base, 10])
for j in range(3):
    stations.append([rng.uniform(0, 180), 30_000 + j, "aton", 990_000_000 + mid() * 1000 + j, 180])
if a.own:
    stations.append([1.0, 40_000, "own", mid() * 1_000_000 + 1, 10])
heapq.heapify(stations)


def generate(emit):
    chan_toggle = 0
    start = time.time()
    while True:
        st = heapq.heappop(stations)
        due, sid, kind, mmsi, period = st
        if a.duration and due > a.duration:
            return
        wait = start + due / a.speedup - time.time()
        if wait > 0:
            time.sleep(wait)
        chan_toggle ^= 1
        chan = "AB"[chan_toggle]
        faded = a.fade and chan == "B" and (due % a.fade) > a.fade / 2
        lines = []
        if kind == "A":
            lines = sentences(message(rng.choice([1, 1, 3]), mmsi, 168), chan)
        elif kind == "B":
            lines = sentences(message(18, mmsi, 168), chan)
        elif kind == "Astatic":
            lines = sentences(message(5, mmsi, 424), chan)
        elif kind == "Bstatic":
            lines = sentences(message(24, mmsi, 160), chan) + sentences(message(24, mmsi, 168), chan)
        elif kind == "base":
            lines = sentences(message(4, mmsi, 168), chan)
        elif kind == "aton":
            lines = sentences(message(21, mmsi, 272), chan)
        elif kind == "own":
            lines = sentences(message(1, mmsi, 168), "", own=True)
        if not faded:
            emit(lines)
        # Reporting intervals are not constant: jitter them.
        st[0] = due + period * rng.uniform(0.8, 1.2)
        heapq.heappush(stations, st)


def run_tcp(port):
    clients, lock = [], threading.Lock()
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("", port))
    srv.listen()

    def accept():
        while True:
            c, _ = srv.accept()
            with lock:
                clients.append(c)

    threading.Thread(target=accept, daemon=True).start()

    def emit(lines):
        data = "".join(l + "\r\n" for l in lines).encode()
        with lock:
            for c in clients[:]:
                try:
                    c.sendall(data)
                except OSError:
                    clients.remove(c)

    generate(emit)


def run_udp(where):
    host, _, port = where.rpartition(":")
    host = host.strip("[]") or "127.0.0.1"
    if ":" in host:
        s = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
        s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_HOPS, 1)
    else:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
    dst = (host, int(port))
    head = b"UdPbC\0" if a.udpbc else b""
    generate(lambda lines: [s.sendto(head + (l + "\r\n").encode(), dst) for l in lines])


def run_pty():
    import pty, tty
    master, slave = pty.openpty()
    tty.setraw(slave)
    print(os.ttyname(slave), flush=True)
    generate(lambda lines: os.write(master, "".join(l + "\r\n" for l in lines).encode()))


def run_stdout():
    def emit(lines):
        for l in lines:
            sys.stdout.write(l + "\r\n")
        sys.stdout.flush()
    generate(emit)


try:
    if a.mode == "tcp":
        run_tcp(int(a.where or 10110))
    elif a.mode == "udp":
        run_udp(a.where or "127.0.0.1:10110")
    elif a.mode == "pty":
        run_pty()
    else:
        run_stdout()
except (KeyboardInterrupt, BrokenPipeError):
    pass
