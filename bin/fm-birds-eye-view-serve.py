#!/usr/bin/env python3
"""Loopback server and collector behind bin/fm-birds-eye-view.sh (DPES-798).

Run it only through that CLI, which validates arguments and picks the helpers.
One collector thread runs bin/fm-birds-eye-model.sh once per interval and never
starts a cycle before the previous one ends. It keeps the last good
fm-birds-eye-view.v1 document, and a failed cycle keeps serving that document.

The HTTP server answers GET only, on 127.0.0.1, and only for the allowlisted
dashboard assets, GET /api/view, and GET /api/capture. A capture runs
bin/fm-birds-eye-capture.sh for one worker the current document names, with the
requested spawn generation forwarded unchanged. Every child runs in its own
session, so shutdown signals only the observer's own process groups.
"""

import argparse
import errno
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VIEW_SCHEMA = 'fm-birds-eye-view.v1'
CAPTURE_SCHEMA = 'fm-birds-eye-capture.v1'
MIN_INTERVAL = 5
DEFAULT_LINES = 40
MAX_LINES = 100
MAX_KEY_LENGTH = 512
MAX_GENERATION_LENGTH = 64
CYCLE_TIMEOUT = 120
CAPTURE_TIMEOUT = 20
TERM_GRACE = 5
CSP = ("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; "
       "connect-src 'self'; base-uri 'none'; form-action 'none'")
ASSET_ROUTES = {
    '/': ('index.html', 'text/html; charset=utf-8'),
    '/app.css': ('app.css', 'text/css; charset=utf-8'),
    '/app.js': ('app.js', 'application/javascript; charset=utf-8'),
}
CAPTURE_PARAMS = frozenset(('key', 'spawn_gen', 'lines'))


def utc_now():
    return datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')


def log(message):
    print(f'fm-birds-eye-view: {message}', file=sys.stderr, flush=True)


def tail(data):
    text = data.decode('utf-8', 'replace').strip()
    return text[-300:] if text else '(no stderr)'


def signal_group(proc, signum):
    try:
        os.killpg(proc.pid, signum)
    except ProcessLookupError:
        pass


def parse_json_object(data, schema):
    try:
        value = json.loads(data.decode('utf-8'))
    except (UnicodeDecodeError, ValueError):
        return None
    if isinstance(value, dict) and value.get('schema') == schema:
        return value
    return None


def find_worker(doc, key):
    entities = doc.get('entities')
    for entity in entities if isinstance(entities, list) else []:
        if (isinstance(entity, dict) and entity.get('key') == key
                and entity.get('kind') == 'worker' and entity.get('task_id')):
            return entity
    return None


def home_path_for(doc, worker):
    observation = worker.get('observation')
    root_id = observation.get('root_id') if isinstance(observation, dict) else None
    roots = doc.get('roots')
    for root in roots if isinstance(roots, list) else []:
        if (isinstance(root, dict) and root_id is not None and root.get('root_id') == root_id
                and isinstance(root.get('home_path'), str) and root['home_path']):
            return root['home_path']
    return None


def capture_error(key, generation, lines, status, reason):
    body = {
        'schema': CAPTURE_SCHEMA,
        'key': key,
        'task_id': None,
        'spawn_gen_requested': generation or None,
        'spawn_gen_current': None,
        'captured_at': utc_now(),
        'source_backend': None,
        'lines_requested': lines,
        'lines_returned': 0,
        'truncated': False,
        'status': status,
        'reason': reason,
        'text': '',
    }
    return json.dumps(body).encode()


class Observer:
    """Owns the collector thread, the last good view, and every child process."""

    def __init__(self, model, capture_helper, interval, roots, state_dir):
        self.model = model
        self.capture_helper = capture_helper
        self.interval = interval
        self.roots = roots
        self.last_good_path = os.path.join(state_dir, 'last-good.json')
        self.stop = threading.Event()
        self._doc_lock = threading.Lock()
        self._doc = None
        self._doc_bytes = None
        self._proc_lock = threading.Lock()
        self._children = set()
        self._stopping = False
        self._collector = threading.Thread(target=self._collect_forever, name='birds-eye-collector')

    def start(self):
        self._collector.start()

    def view(self):
        with self._doc_lock:
            return self._doc_bytes

    def capture(self, key, generation, lines):
        """Return capture JSON for one worker in the current view; None before the first collection."""
        with self._doc_lock:
            doc = self._doc
        if doc is None:
            return None
        worker = find_worker(doc, key)
        if worker is None:
            return capture_error(key, generation, lines, 'unknown_identity',
                                 'key is not a worker in the current view')
        if not generation:
            return capture_error(key, generation, lines, 'generation_mismatch',
                                 'spawn_gen is missing or empty')
        home = home_path_for(doc, worker)
        if home is None:
            return capture_error(key, generation, lines, 'unknown_identity',
                                 'the worker home is not in the current view')
        argv = [self.capture_helper, '--root', home, '--task', str(worker['task_id']),
                '--spawn-gen', generation, '--lines', str(lines)]
        rc, out, err = self._run(argv, CAPTURE_TIMEOUT)
        if rc is None:
            return capture_error(key, generation, lines, 'error', 'capture did not finish')
        if rc != 0 or parse_json_object(out, CAPTURE_SCHEMA) is None:
            return capture_error(key, generation, lines, 'error',
                                 f'capture helper exit {rc}: {tail(err)}')
        return out

    def begin_shutdown(self):
        with self._proc_lock:
            self._stopping = True
            children = list(self._children)
        self.stop.set()
        for proc in children:
            signal_group(proc, signal.SIGTERM)

    def finish_shutdown(self):
        self._collector.join()
        deadline = time.monotonic() + 2 * TERM_GRACE + 2
        while time.monotonic() < deadline:
            with self._proc_lock:
                if not self._children:
                    return
            time.sleep(0.1)
        with self._proc_lock:
            leftover = list(self._children)
        for proc in leftover:
            signal_group(proc, signal.SIGKILL)

    def _run(self, argv, timeout):
        """Run one observer-owned child; return (returncode, stdout, stderr).

        returncode is None when the child timed out or was stopped.
        """
        with self._proc_lock:
            if self._stopping:
                return None, b'', b'observer is stopping'
            try:
                proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, start_new_session=True)
            except OSError as exc:
                return None, b'', str(exc).encode()
            self._children.add(proc)
        try:
            return self._wait(proc, timeout)
        finally:
            with self._proc_lock:
                self._children.discard(proc)

    def _wait(self, proc, timeout):
        deadline = time.monotonic() + timeout
        terminated = False
        while True:
            try:
                out, err = proc.communicate(timeout=0.5)
                return (None if terminated else proc.returncode), out, err
            except subprocess.TimeoutExpired:
                pass
            now = time.monotonic()
            if not terminated and (self._stopping or now >= deadline):
                signal_group(proc, signal.SIGTERM)
                terminated = True
                deadline = now + TERM_GRACE
            elif terminated and now >= deadline:
                signal_group(proc, signal.SIGKILL)
                out, err = proc.communicate()
                return None, out, err

    def _collect_forever(self):
        while not self.stop.is_set():
            started = time.monotonic()
            self._collect_once()
            remaining = self.interval - (time.monotonic() - started)
            if remaining > 0:
                self.stop.wait(remaining)

    def _collect_once(self):
        argv = [self.model, '--interval', str(self.interval)]
        for root in self.roots:
            argv += ['--root', root]
        if os.path.exists(self.last_good_path):
            argv += ['--previous', self.last_good_path]
        rc, out, err = self._run(argv, CYCLE_TIMEOUT)
        if self.stop.is_set():
            return
        if rc is None:
            log('collection did not finish in time; keeping the last good view')
            return
        if rc != 0:
            log(f'collection failed with exit {rc}; keeping the last good view: {tail(err)}')
            return
        doc = parse_json_object(out, VIEW_SCHEMA)
        if doc is None:
            log('collection output is not an fm-birds-eye-view.v1 document; keeping the last good view')
            return
        try:
            self._write_last_good(out)
        except OSError as exc:
            log(f'cannot save the last good view, serving it from memory only: {exc}')
        with self._doc_lock:
            self._doc = doc
            self._doc_bytes = out

    def _write_last_good(self, data):
        tmp = self.last_good_path + '.tmp'
        with open(tmp, 'wb') as fh:
            fh.write(data)
        os.replace(tmp, self.last_good_path)


class Handler(BaseHTTPRequestHandler):
    server_version = 'fm-birds-eye-view'
    sys_version = ''

    def log_message(self, fmt, *args):
        return

    def do_GET(self):
        if not self._trusted():
            self._send_json(403, {'error': 'host or origin not allowed'})
            return
        target = urllib.parse.urlsplit(self.path)
        if target.scheme or target.netloc or not target.path.startswith('/'):
            self._send_json(400, {'error': 'unsupported request target'})
            return
        pairs = urllib.parse.parse_qsl(target.query, keep_blank_values=True)
        names = [name for name, _ in pairs]
        if len(names) != len(set(names)):
            self._send_json(400, {'error': 'repeated parameter'})
            return
        if target.path in ASSET_ROUTES:
            if pairs:
                self._send_json(400, {'error': 'unexpected parameter'})
                return
            name, content_type = ASSET_ROUTES[target.path]
            self._send(200, content_type, self.server.assets[name])
            return
        if target.path == '/api/view':
            if pairs:
                self._send_json(400, {'error': 'unexpected parameter'})
                return
            body = self.server.observer.view()
            if body is None:
                self._send_json(503, {'error': 'no collection has completed yet'})
                return
            self._send(200, 'application/json; charset=utf-8', body)
            return
        if target.path == '/api/capture':
            self._capture(dict(pairs))
            return
        self._send_json(404, {'error': 'not found'})

    def _reject_method(self):
        self._send_json(405, {'error': 'only GET is served'}, headers=(('Allow', 'GET'),))

    do_HEAD = do_POST = do_PUT = do_DELETE = do_PATCH = do_OPTIONS = _reject_method

    def _capture(self, params):
        if set(params) - CAPTURE_PARAMS:
            self._send_json(400, {'error': 'unexpected parameter'})
            return
        key = params.get('key', '')
        if not key or len(key) > MAX_KEY_LENGTH:
            self._send_json(400, {'error': 'key names one worker in the current view'})
            return
        generation = params.get('spawn_gen', '')
        if len(generation) > MAX_GENERATION_LENGTH:
            self._send_json(400, {'error': 'spawn_gen is too long'})
            return
        raw_lines = params.get('lines')
        if raw_lines is None:
            lines = DEFAULT_LINES
        elif raw_lines.isascii() and raw_lines.isdigit() and int(raw_lines) >= 1:
            lines = min(int(raw_lines), MAX_LINES)
        else:
            self._send_json(400, {'error': 'lines must be a whole number of at least 1'})
            return
        body = self.server.observer.capture(key, generation, lines)
        if body is None:
            self._send_json(503, {'error': 'no collection has completed yet'})
            return
        self._send(200, 'application/json; charset=utf-8', body)

    def _trusted(self):
        hosts = self.headers.get_all('Host') or []
        if len(hosts) != 1 or hosts[0] != self.server.expected_host:
            return False
        origins = self.headers.get_all('Origin') or []
        if len(origins) > 1 or (origins and origins[0] != self.server.expected_origin):
            return False
        if self.headers.get('Transfer-Encoding') is not None:
            return False
        return self.headers.get('Content-Length', '0') == '0'

    def _send_json(self, status, obj, headers=()):
        self._send(status, 'application/json; charset=utf-8', json.dumps(obj).encode(), headers)

    def _send(self, status, content_type, body, headers=()):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Security-Policy', CSP)
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Cross-Origin-Resource-Policy', 'same-origin')
        self.send_header('Referrer-Policy', 'no-referrer')
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, port, observer, assets):
        super().__init__(('127.0.0.1', port), Handler)
        self.observer = observer
        self.assets = assets
        bound = self.server_address[1]
        self.expected_host = f'127.0.0.1:{bound}'
        self.expected_origin = f'http://127.0.0.1:{bound}'


def load_assets(directory):
    assets = {}
    for name, _ in ASSET_ROUTES.values():
        with open(os.path.join(directory, name), 'rb') as fh:
            assets[name] = fh.read()
    return assets


def report_open(url):
    opener = 'open' if sys.platform == 'darwin' else 'xdg-open'
    command = shutil.which(opener)
    if command is None:
        print(f'open unavailable: no {opener} command on PATH; open {url} yourself', flush=True)
        return
    try:
        result = subprocess.run([command, url], stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    except (OSError, subprocess.TimeoutExpired) as exc:
        print(f'open failed ({exc}); open {url} yourself', flush=True)
        return
    if result.returncode == 0:
        print('opened the dashboard in the default browser', flush=True)
    else:
        print(f'open failed (exit {result.returncode}); open {url} yourself', flush=True)


def parse_args(argv):
    parser = argparse.ArgumentParser(prog='fm-birds-eye-view-serve.py')
    parser.add_argument('--assets', required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--capture', required=True)
    parser.add_argument('--interval', type=int, required=True)
    parser.add_argument('--port', type=int, default=None)
    parser.add_argument('--open', action='store_true')
    parser.add_argument('--root', action='append', default=[])
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    if args.interval < MIN_INTERVAL:
        log(f'--interval must be at least {MIN_INTERVAL} seconds')
        return 2
    for label, path in (('model', args.model), ('capture', args.capture)):
        if not (os.path.isfile(path) and os.access(path, os.X_OK)):
            log(f'the {label} helper is not available at {path}; the live view cannot run')
            return 1
    try:
        assets = load_assets(args.assets)
    except OSError as exc:
        log(f'dashboard assets are not readable: {exc}')
        return 1

    state_dir = tempfile.mkdtemp(prefix='fm-birds-eye-view.')
    observer = Observer(args.model, args.capture, args.interval, args.root, state_dir)
    try:
        server = Server(args.port or 0, observer, assets)
    except OSError as exc:
        shutil.rmtree(state_dir, ignore_errors=True)
        if exc.errno == errno.EADDRINUSE and args.port:
            log(f'port {args.port} is already in use on 127.0.0.1; choose another --port '
                'or omit --port for a free port')
        else:
            log(f'cannot bind 127.0.0.1: {exc}')
        return 1

    url = f'http://127.0.0.1:{server.server_address[1]}/'
    serve_thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.5},
                                    name='birds-eye-http', daemon=True)

    def on_signal(signum, frame):
        observer.stop.set()

    signal.signal(signal.SIGINT, on_signal)
    signal.signal(signal.SIGTERM, on_signal)
    observer.start()
    serve_thread.start()
    print(f'birds-eye view: {url}', flush=True)
    print(f'working files: {state_dir} (removed on exit)', flush=True)
    if args.open:
        report_open(url)
    print('stop with Ctrl-C or SIGTERM', flush=True)
    try:
        while not observer.stop.wait(0.5):
            pass
    finally:
        observer.begin_shutdown()
        server.shutdown()
        serve_thread.join()
        server.server_close()
        observer.finish_shutdown()
        shutil.rmtree(state_dir, ignore_errors=True)
    print('birds-eye view stopped', flush=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())
