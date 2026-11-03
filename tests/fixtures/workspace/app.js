'use strict';
globalThis.fixtureMarker = require('persist-dep');
function add(a, b) {
  const sum = a + b;
  return sum;
}
const server = require('node:http').createServer((req, res) => {
  if (req.url === '/download') { res.setHeader('Content-Type', 'application/octet-stream'); return res.end(Buffer.alloc(4 * 1024 * 1024, 120)); }
  const answer = req.url === '/debug' ? add(20, 22) : 0;
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify({ marker: globalThis.fixtureMarker, answer, headers: req.headers }));
});
server.listen(3000, '0.0.0.0', () => console.log('persistent project ready'));
