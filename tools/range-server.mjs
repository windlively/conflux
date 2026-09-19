// 本地 Range 测试服务器：给开发期验证多连接分段下载用
// 用法：node tools/range-server.mjs [--port 8787] [--size 120] [--file /path/to/file]
import { randomBytes } from 'node:crypto';
import { closeSync, createReadStream, existsSync, openSync, statSync, writeSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const args = process.argv.slice(2);
const arg = (name, fallback) => {
  const index = args.indexOf(`--${name}`);
  return index >= 0 && args[index + 1] ? args[index + 1] : fallback;
};

const port = Number(arg('port', '8787'));
const sizeMb = Number(arg('size', '120'));
// 每连接限速（KB/s），0 表示不限速；本机回环太快，限速后便于观察各通道进度
const throttleKbps = Number(arg('throttle', '0'));
let file = arg('file', '');

if (!file) {
  file = join(tmpdir(), `conflux-test-${sizeMb}mb.bin`);
  const target = sizeMb * 1024 * 1024;
  if (!existsSync(file) || statSync(file).size !== target) {
    process.stdout.write(`生成测试文件 ${file} …\n`);
    const fd = openSync(file, 'w');
    let written = 0;
    while (written < target) {
      const buf = randomBytes(Math.min(4 * 1024 * 1024, target - written));
      writeSync(fd, buf);
      written += buf.length;
    }
    closeSync(fd);
  }
}

const total = statSync(file).size;
const mb = (bytes) => `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
console.log(`文件：${file}（${mb(total)}）`);
console.log(`地址：http://127.0.0.1:${port}/test.bin`);

const server = createServer((req, res) => {
  if (!req.url?.startsWith('/test.bin')) {
    res.writeHead(404).end('not found');
    return;
  }

  const range = req.headers.range;
  const match = range ? /^bytes=(\d+)-(\d*)$/.exec(range.trim()) : null;
  const start = match ? Number(match[1]) : 0;
  const end = match && match[2] ? Math.min(Number(match[2]), total - 1) : total - 1;

  if (match && (start >= total || start > end)) {
    res.writeHead(416, { 'content-range': `bytes */${total}` }).end();
    return;
  }

  const headers = {
    'content-length': String(end - start + 1),
    'accept-ranges': 'bytes',
    'content-type': 'application/octet-stream',
  };
  if (match) {
    headers['content-range'] = `bytes ${start}-${end}/${total}`;
  }
  res.writeHead(match ? 206 : 200, headers);

  if (req.method === 'HEAD') {
    res.end();
    return;
  }

  const stream = createReadStream(file, { start, end, highWaterMark: 32 * 1024 });
  if (throttleKbps <= 0) {
    stream.pipe(res);
    return;
  }
  stream.on('data', (chunk) => {
    const ready = res.write(chunk);
    stream.pause();
    const delay = (chunk.length / (throttleKbps * 1024)) * 1000;
    setTimeout(() => {
      if (ready) stream.resume();
      else res.once('drain', () => stream.resume());
    }, delay);
  });
  stream.on('end', () => res.end());
  req.on('close', () => stream.destroy());
});

server.listen(port, '127.0.0.1');
