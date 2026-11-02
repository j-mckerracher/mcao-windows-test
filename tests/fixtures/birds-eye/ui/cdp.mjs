// Minimal Chrome DevTools Protocol client for the dashboard UI harness.
// It uses Node built-ins and the global WebSocket of Node 22 or later only.
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const KEY_SPECS = {
  Tab: { key: 'Tab', code: 'Tab', windowsVirtualKeyCode: 9 },
  Enter: { key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, text: '\r' },
  Escape: { key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 },
  Space: { key: ' ', code: 'Space', windowsVirtualKeyCode: 32, text: ' ' },
  ArrowDown: { key: 'ArrowDown', code: 'ArrowDown', windowsVirtualKeyCode: 40 },
  Slash: { key: '/', code: 'Slash', windowsVirtualKeyCode: 191, text: '/' }
};

class Connection {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.handlers = new Map();
    socket.addEventListener('message', (event) => this.dispatch(String(event.data)));
    socket.addEventListener('close', () => {
      for (const waiter of this.pending.values()) {
        waiter.reject(new Error(`DevTools connection closed during ${waiter.method}`));
      }
      this.pending.clear();
    });
  }

  static open(url) {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      socket.addEventListener('open', () => resolve(new Connection(socket)), { once: true });
      socket.addEventListener('error', () => reject(new Error(`cannot open DevTools socket ${url}`)), { once: true });
    });
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.nextId;
    this.nextId += 1;
    const message = sessionId ? { id, method, params, sessionId } : { id, method, params };
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method });
      this.socket.send(JSON.stringify(message));
    });
  }

  on(sessionId, method, handler) {
    const key = `${sessionId || ''}|${method}`;
    if (!this.handlers.has(key)) {
      this.handlers.set(key, new Set());
    }
    this.handlers.get(key).add(handler);
  }

  dispatch(raw) {
    const message = JSON.parse(raw);
    if (message.id !== undefined) {
      const waiter = this.pending.get(message.id);
      if (!waiter) {
        return;
      }
      this.pending.delete(message.id);
      if (message.error) {
        waiter.reject(new Error(`${waiter.method} failed: ${message.error.message}`));
      } else {
        waiter.resolve(message.result);
      }
      return;
    }
    const handlers = this.handlers.get(`${message.sessionId || ''}|${message.method}`);
    if (handlers) {
      for (const handler of handlers) {
        handler(message.params || {});
      }
    }
  }

  close() {
    this.socket.close();
  }
}

export class Page {
  constructor(connection, sessionId, targetId) {
    this.connection = connection;
    this.sessionId = sessionId;
    this.targetId = targetId;
    this.requestUrls = [];
    this.on('Network.requestWillBeSent', (params) => {
      this.requestUrls.push(params.request.url);
    });
    this.on('Page.javascriptDialogOpening', () => {
      this.send('Page.handleJavaScriptDialog', { accept: false }).catch(() => {});
    });
  }

  send(method, params = {}) {
    return this.connection.send(method, params, this.sessionId);
  }

  on(method, handler) {
    this.connection.on(this.sessionId, method, handler);
  }

  once(method, timeoutMs = 10000) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`timed out waiting for ${method}`)), timeoutMs);
      this.on(method, (params) => {
        clearTimeout(timer);
        resolve(params);
      });
    });
  }

  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true
    });
    if (result.exceptionDetails) {
      const detail = result.exceptionDetails.exception?.description || result.exceptionDetails.text;
      throw new Error(`page evaluation failed: ${detail}`);
    }
    return result.result.value;
  }

  async waitFor(expression, { timeoutMs = 8000, description = expression } = {}) {
    const deadline = Date.now() + timeoutMs;
    let lastError = null;
    for (;;) {
      try {
        const value = await this.evaluate(expression);
        if (value) {
          return value;
        }
      } catch (error) {
        lastError = error;
      }
      if (Date.now() > deadline) {
        const suffix = lastError ? ` (${lastError.message})` : '';
        throw new Error(`timed out waiting for ${description}${suffix}`);
      }
      await sleep(40);
    }
  }

  async navigate(url) {
    const loaded = this.once('Page.loadEventFired');
    await this.send('Page.navigate', { url });
    await loaded;
  }

  async setViewport({ width, height = 900, mobile = false }) {
    await this.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile });
  }

  async addInitScript(source) {
    await this.send('Page.addScriptToEvaluateOnNewDocument', { source });
  }

  async press(name) {
    const spec = KEY_SPECS[name];
    if (!spec) {
      throw new Error(`unknown key ${name}`);
    }
    await this.send('Input.dispatchKeyEvent', { type: 'keyDown', ...spec });
    const { text, ...withoutText } = spec;
    await this.send('Input.dispatchKeyEvent', { type: 'keyUp', ...withoutText });
  }

  async typeText(text) {
    await this.send('Input.insertText', { text });
  }

  // Clicks with a real mouse event at the element's centre, so hit testing and
  // focus behave as they would for a user.
  async clickSelector(selector) {
    const point = await this.evaluate(`(() => {
      const el = document.querySelector(${JSON.stringify(selector)});
      if (!el) { return null; }
      el.scrollIntoView({ block: 'center' });
      const r = el.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    })()`);
    if (!point) {
      throw new Error(`no element matches ${selector}`);
    }
    await this.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: point.x, y: point.y, button: 'left', clickCount: 1 });
    await this.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: point.x, y: point.y, button: 'left', clickCount: 1 });
  }

  async close() {
    await this.connection.send('Target.closeTarget', { targetId: this.targetId }).catch(() => {});
  }
}

export async function launchBrowser({ chromePath, userDataDir, timeoutMs = 20000, extraArgs = [] }) {
  const args = [
    '--headless',
    '--remote-debugging-port=0',
    `--user-data-dir=${userDataDir}`,
    '--window-size=1280,900',
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-background-networking',
    '--disable-component-update',
    '--disable-sync',
    '--metrics-recording-only',
    '--disable-gpu',
    ...extraArgs,
    'about:blank'
  ];
  const child = spawn(chromePath, args, { stdio: ['ignore', 'ignore', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (chunk) => {
    stderr = `${stderr}${chunk}`.slice(-2000);
  });

  const deadline = Date.now() + timeoutMs;
  let endpoint = null;
  while (!endpoint) {
    if (child.exitCode !== null) {
      throw new Error(`Chrome exited with code ${child.exitCode}: ${stderr.trim()}`);
    }
    if (Date.now() > deadline) {
      child.kill('SIGKILL');
      throw new Error(`Chrome did not expose a DevTools endpoint within ${timeoutMs} ms`);
    }
    const file = join(userDataDir, 'DevToolsActivePort');
    if (existsSync(file)) {
      const [port, path] = (await readFile(file, 'utf8')).split('\n');
      if (port && path) {
        endpoint = `ws://127.0.0.1:${port.trim()}${path.trim()}`;
      }
    }
    if (!endpoint) {
      await sleep(50);
    }
  }

  const connection = await Connection.open(endpoint);
  const { product: version } = await connection.send('Browser.getVersion');

  return {
    version,
    async newPage() {
      const { targetId } = await connection.send('Target.createTarget', { url: 'about:blank' });
      const { sessionId } = await connection.send('Target.attachToTarget', { targetId, flatten: true });
      const page = new Page(connection, sessionId, targetId);
      await page.send('Page.enable');
      await page.send('Network.enable');
      return page;
    },
    async close() {
      await connection.send('Browser.close').catch(() => {});
      connection.close();
      if (child.exitCode === null) {
        child.kill('SIGTERM');
      }
    }
  };
}
