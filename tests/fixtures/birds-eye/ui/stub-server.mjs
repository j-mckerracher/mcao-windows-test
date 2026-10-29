// Local stub for the dashboard's two data endpoints plus the static assets.
// Binds 127.0.0.1 only. Every request is recorded so tests can audit traffic.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { captureFor, withGeneratedEpoch } from './views.mjs';

const CONTENT_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png'
};

function sendJson(res, status, body) {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Cache-Control': 'no-store',
    'Content-Length': Buffer.byteLength(text)
  });
  res.end(text);
}

export async function startStub({ assetsDir, initialView, stampNow = false, port = 0 }) {
  const root = resolve(assetsDir);
  const state = {
    view: initialView,
    viewDelayMs: 0,
    failNextView: 0,
    viewRequests: 0,
    captureLines: [],
    requests: []
  };

  function currentView() {
    if (!stampNow) {
      return state.view;
    }
    return withGeneratedEpoch(state.view, Math.floor(Date.now() / 1000));
  }

  async function serveStatic(pathname, res) {
    const relative = pathname === '/' ? 'index.html' : pathname.replace(/^\/+/, '');
    const target = normalize(join(root, relative));
    if (target !== root && !target.startsWith(root + sep)) {
      res.writeHead(403).end('forbidden');
      return;
    }
    try {
      const body = await readFile(target);
      res.writeHead(200, {
        'Content-Type': CONTENT_TYPES[extname(target)] || 'application/octet-stream',
        'Content-Length': body.length
      });
      res.end(body);
    } catch {
      res.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' }).end('not found');
    }
  }

  const server = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    state.requests.push({ method: req.method, path: url.pathname, search: url.search });

    if (req.method !== 'GET' && req.method !== 'HEAD') {
      res.writeHead(405, { Allow: 'GET, HEAD' }).end();
      return;
    }

    if (url.pathname === '/api/view') {
      state.viewRequests += 1;
      if (state.viewDelayMs > 0) {
        await new Promise((resolveDelay) => setTimeout(resolveDelay, state.viewDelayMs));
      }
      if (state.failNextView > 0) {
        state.failNextView -= 1;
        sendJson(res, 500, { error: 'stub failure' });
        return;
      }
      sendJson(res, 200, currentView());
      return;
    }

    if (url.pathname === '/api/capture') {
      const key = url.searchParams.get('key') || '';
      const lines = Number(url.searchParams.get('lines') || '40');
      state.captureLines.push({ key, lines, spawnGen: url.searchParams.get('spawn_gen') });
      sendJson(res, 200, captureFor(key, lines));
      return;
    }

    if (url.pathname.startsWith('/api/')) {
      sendJson(res, 404, { error: 'unknown endpoint' });
      return;
    }

    await serveStatic(url.pathname, res);
  });

  await new Promise((resolveListen, rejectListen) => {
    server.once('error', rejectListen);
    server.listen(port, '127.0.0.1', resolveListen);
  });
  const address = server.address();

  return {
    url: `http://127.0.0.1:${address.port}/`,
    state,
    setView(view) {
      state.view = view;
    },
    close() {
      return new Promise((resolveClose) => {
        server.closeAllConnections();
        server.close(() => resolveClose());
      });
    }
  };
}
