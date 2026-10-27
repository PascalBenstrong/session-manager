// Minimal static file server for tools/preview.html and tools/real/.  Usage: node tools/serve.mjs [port]
// /ws is a WebSocket that says {"hello":1} and answers each text message with "echo:<message>".
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const port = Number(process.argv[2] || 8123);
const types = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css',
  '.wasm': 'application/wasm', '.json': 'application/json', '.png': 'image/png' };

const server = createServer(async (req, res) => {
  const file = join(root, normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)));
  if (!file.startsWith(root)) return res.writeHead(403).end();
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': types[extname(file)] || 'application/octet-stream' }).end(body);
  } catch {
    res.writeHead(404).end('not found');
  }
});

const CRLF = String.fromCharCode(13, 10);
server.on('upgrade', (req, socket) => {
  if (!req.url.startsWith('/ws')) return socket.destroy();
  const accept = createHash('sha1').update(`${req.headers['sec-websocket-key']}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
  const lines = ['HTTP/1.1 101 Switching Protocols', 'Upgrade: websocket', 'Connection: Upgrade', `Sec-WebSocket-Accept: ${accept}`];
  socket.write(`${lines.join(CRLF)}${CRLF}${CRLF}`);
  const send = (text) => {
    const body = Buffer.from(text);
    const head = body.length < 126 ? Buffer.from([0x81, body.length]) : Buffer.from([0x81, 126, body.length >> 8, body.length & 255]);
    socket.write(Buffer.concat([head, body]));
  };
  send(JSON.stringify({ hello: 1 }));
  let buf = Buffer.alloc(0);
  socket.on('data', (d) => {
    buf = Buffer.concat([buf, d]);
    while (buf.length >= 6) {
      const op = buf[0] & 15;
      let len = buf[1] & 127;
      let at = 2;
      if (len === 126) {
        len = buf.readUInt16BE(2);
        at = 4;
      } else if (len === 127) return socket.destroy();
      if (buf.length < at + 4 + len) return;
      const mask = buf.subarray(at, at + 4);
      const data = Buffer.from(buf.subarray(at + 4, at + 4 + len).map((b, i) => b ^ mask[i % 4]));
      buf = buf.subarray(at + 4 + len);
      if (op === 8) return socket.end();
      if (op === 1) send(`echo:${data.toString()}`);
    }
  });
  socket.on('error', () => {});
});

server.listen(port, '127.0.0.1', () => console.log(`http://127.0.0.1:${port}/tools/preview.html`));
