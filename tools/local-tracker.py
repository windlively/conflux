#!/usr/bin/env python3
"""本地 HTTP tracker：给开发期验证磁力下载用，announce 返回紧凑格式的 peer 列表。

磁力任务的生产会话靠 DHT／tracker 找 peer，本机联调需要一个能被找到的做种者：
先起这个 tracker，再让 examples/bt_seed.rs 带上同一个 tracker 地址做种，
磁力里就会带上它，应用即可在完全离线的环境下走完真实的磁力下载链路。

用法：python3 tools/local-tracker.py [端口，默认 8790]
"""
import struct
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote_to_bytes

PEERS = {}  # info_hash(bytes) -> {peer_id(bytes): (ip, port)}


def raw_param(query: str, key: str):
    # info_hash／peer_id 是二进制，必须按字节取，不能走 parse_qs 的字符解码
    for part in query.split("&"):
        if part.startswith(key + "="):
            return unquote_to_bytes(part[len(key) + 1 :])
    return None


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path, _, query = self.path.partition("?")
        if path != "/announce":
            self.send_error(404)
            return

        info_hash = raw_param(query, "info_hash")
        peer_id = raw_param(query, "peer_id") or b"unknown"
        port = int(raw_param(query, "port") or b"0")
        event = (raw_param(query, "event") or b"").decode("ascii", "replace")
        ip = (raw_param(query, "ip") or b"").decode("ascii", "replace") or self.client_address[0]

        table = PEERS.setdefault(info_hash, {})
        if event == "stopped":
            table.pop(peer_id, None)
        else:
            table[peer_id] = (ip, port)

        peers = b""
        for other_id, (peer_ip, peer_port) in table.items():
            if other_id == peer_id:
                continue
            peers += struct.pack("!4sH", bytes(map(int, peer_ip.split("."))), peer_port)

        body = b"d8:intervali15e5:peers%d:%se" % (len(peers), peers)
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        print(
            f"announce {info_hash.hex()[:12]} event={event or '-'} from={ip}:{port} peers={len(table)}",
            flush=True,
        )

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8790
    print(f"tracker 监听 127.0.0.1:{port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
