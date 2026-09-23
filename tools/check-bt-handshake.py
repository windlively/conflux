import argparse
import hashlib
import http.server
import os
import socket
import struct
import subprocess
import threading
import time
import urllib.parse
from pathlib import Path


def encode(value):
    if isinstance(value, int):
        return b'i' + str(value).encode() + b'e'
    if isinstance(value, bytes):
        return str(len(value)).encode() + b':' + value
    if isinstance(value, dict):
        return b'd' + b''.join(encode(key) + encode(value[key]) for key in sorted(value)) + b'e'
    raise TypeError(type(value))


def decode(data, pos=0):
    marker = data[pos:pos + 1]
    if marker == b'i':
        end = data.index(b'e', pos)
        return int(data[pos + 1:end]), end + 1
    if marker == b'd':
        result = {}
        pos += 1
        while data[pos:pos + 1] != b'e':
            key, pos = decode(data, pos)
            result[key], pos = decode(data, pos)
        return result, pos + 1
    end = data.index(b':', pos)
    length = int(data[pos:end])
    start = end + 1
    return data[start:start + length], start + length


def receive(connection, length):
    data = bytearray()
    while len(data) < length:
        chunk = connection.recv(length - len(data))
        if not chunk:
            raise EOFError(f'closed with {len(data)}/{length} bytes')
        data.extend(chunk)
    return bytes(data)


def message(connection):
    length, = struct.unpack('!I', receive(connection, 4))
    if length > 1024 * 1024:
        raise ValueError('oversized frame')
    return receive(connection, length)


def extended(extension_id, data):
    payload = bytes([20, extension_id]) + data
    return struct.pack('!I', len(payload)) + payload


INFO = encode({
    b'length': 1,
    b'name': b'conflux-protocol-check.bin',
    b'piece length': 16384,
    b'pieces': hashlib.sha1(b'x').digest(),
})
INFO_HASH = hashlib.sha1(INFO).digest()


def run_case(exe, mode, expected):
    observations = []
    failures = []
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        listener.settimeout(6)
        peer_port = listener.getsockname()[1]

        class Tracker(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                body = encode({
                    b'interval': 60,
                    b'peers': socket.inet_aton('127.0.0.1') + struct.pack('!H', peer_port),
                })
                observations.append('tracker requested')
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        def peer():
            try:
                with listener.accept()[0] as connection:
                    connection.settimeout(6)
                    request = receive(connection, 68)
                    assert request[:20] == b'\x13BitTorrent protocol'
                    assert request[28:48] == INFO_HASH
                    observations.append('received client handshake')
                    reserved = bytearray(8)
                    reserved[5] = 0x10
                    response = b'\x13BitTorrent protocol' + reserved + INFO_HASH + b'-TEST01-123456789012'
                    extension = extended(0, encode({b'm': {b'ut_metadata': 7}, b'metadata_size': len(INFO)}))
                    if mode == 'fragmented':
                        connection.sendall(response[:20])
                        time.sleep(0.1)
                        connection.sendall(response[20:])
                    elif mode == 'bytewise':
                        for byte in response:
                            connection.sendall(bytes([byte]))
                            time.sleep(0.005)
                    elif mode == 'coalesced':
                        connection.sendall(response + extension)
                    else:
                        connection.sendall(response)
                    while True:
                        frame = message(connection)
                        if frame[:2] == bytes([20, 0]):
                            handshake, _ = decode(frame[2:])
                            client_id = handshake[b'm'][b'ut_metadata']
                            break
                    if mode != 'coalesced':
                        connection.sendall(extension)
                    while True:
                        frame = message(connection)
                        if frame[:2] == bytes([20, 7]):
                            request, _ = decode(frame[2:])
                            assert request[b'msg_type'] == 0 and request[b'piece'] == 0
                            connection.sendall(extended(client_id, encode({
                                b'msg_type': 1, b'piece': 0, b'total_size': len(INFO),
                            }) + INFO))
                            observations.append('sent metadata only')
                            while connection.recv(1024):
                                pass
                            break
            except (OSError, EOFError) as error:
                if expected == 0:
                    failures.append(repr(error))
            except Exception as error:
                failures.append(repr(error))

        with http.server.HTTPServer(('127.0.0.1', 0), Tracker) as server:
            tracker_thread = threading.Thread(target=server.serve_forever, daemon=True)
            peer_thread = threading.Thread(target=peer, daemon=True)
            tracker_thread.start()
            peer_thread.start()
            try:
                tracker = f'http://127.0.0.1:{server.server_port}/announce'
                magnet = f'magnet:?xt=urn:btih:{INFO_HASH.hex()}&tr={urllib.parse.quote(tracker, safe="")}'
                result = subprocess.run(
                    [str(exe), magnet, '4'], capture_output=True, text=True, timeout=10,
                    env={**os.environ, 'RUST_LOG': 'debug'},
                )
            finally:
                server.shutdown()
                tracker_thread.join(2)
                peer_thread.join(7)
            assert not tracker_thread.is_alive() and not peer_thread.is_alive()

    output = result.stdout + result.stderr
    summary = [line for line in output.splitlines() if any(
        term in line for term in ('取得元数据', 'error reading metainfo', 'Error:')
    )]
    print(f'{mode}: exit={result.returncode}; ' + '\n'.join(summary), flush=True)
    assert not failures, failures
    assert 'tracker requested' in observations and 'received client handshake' in observations, output
    assert result.returncode == expected, output
    if expected == 0:
        assert 'sent metadata only' in observations and f'info hash={INFO_HASH.hex()}' in result.stdout, output
    else:
        assert 'error deserializing handshake: not enough data' in output, output


def main():
    parser = argparse.ArgumentParser(description='Compare complete, fragmented and coalesced BT handshakes using metadata only.')
    parser.add_argument('--before', type=Path, help='Unpatched bt_probe binary for the same-fixture baseline')
    parser.add_argument('--after', type=Path, default=Path(__file__).resolve().parents[1] / 'src-tauri/target/debug/examples/bt_probe')
    args = parser.parse_args()
    for label, exe in [('before', args.before), ('after', args.after)]:
        if exe is None:
            continue
        print(f'[{label}] {exe}', flush=True)
        for mode in ('whole', 'fragmented', 'bytewise', 'coalesced'):
            expected = int(label == 'before' and mode in ('fragmented', 'bytewise'))
            run_case(exe.resolve(), mode, expected)


if __name__ == '__main__':
    main()
