// quiz-tv-screen-link (autonomy/quiz-tv-screen-link/brief.md): the card's Big screen link is /p/<code>#screen=<token>,
// a one-time token from POST /api/pockets/:code/link {role:"screen"}. pa1b's host/signin spends a link only for
// {token}; {key} is the host key, so a TV that sent the token as {key} got 401 not_host and showed "This screen link
// has expired". Every TV entry point (quiz screen, queue TV, wall TV) must trade its token as {token}, once.
// The server half (that body signs in as the big screen once, then is refused) is tests/pocket_links_test.rs.
import assert from 'node:assert/strict';
import test from 'node:test';
import { JSDOM } from 'jsdom';
import { loadBundled } from './bundle.mjs';

const dom = new JSDOM('<!doctype html><html><body><div id="app"></div></body></html>', { url: 'http://localhost/p/tv-1', pretendToBeVisual: true });
const g = globalThis;
for (const k of ['window', 'document', 'navigator', 'HTMLElement', 'Element', 'Node', 'Event', 'MouseEvent', 'SVGElement']) {
  if (!(k in dom.window)) continue;
  Object.defineProperty(g, k, { value: dom.window[k], configurable: true, writable: true });
}

const m = await loadBundled(`
  export { ScreenApp } from './guest/src/screen/app';
  export { QueueTV } from './guest/src/queue/tv';
  export { WallTV } from './guest/src/wall/tv';
  export { screenSignIn, Api } from './guest/src/api';
`);

/** A box that records every request and answers the sign-in like pa1b: 200 only for {token}, else 401 not_host. */
function env(code) {
  const calls = [];
  const fetch = async (url, init = {}) => {
    const path = url.split(`/api/pocket/${code}/`)[1] ?? '';
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ method: init.method ?? 'GET', path, body });
    // Stop each TV right after its sign-in: the body is what this test is about.
    const r = body?.token ? { status: 410, error: 'ended' } : { status: 401, error: 'not_host' };
    return { ok: false, status: r.status, json: async () => ({ error: r.error, message: 'Fake box.' }) };
  };
  const timers = { next: 1 };
  return {
    calls,
    env: {
      fetch,
      WebSocket: undefined,
      setTimeout: () => timers.next++,
      clearTimeout: () => {},
      setInterval: () => timers.next++,
      clearInterval: () => {},
      now: () => 1_000_000,
      location: { pathname: `/p/${code}`, protocol: 'http:', host: 'localhost:8790' },
      origin: 'http://localhost:8790',
      still: () => true,
    },
  };
}

for (const [name, make] of [
  ['quiz screen (screen/app.ts)', (root, e, code, tok) => new m.ScreenApp(root, e, code, tok)],
  ['queue TV (queue/tv.ts)', (root, e, code, tok) => new m.QueueTV(root, e, code, tok)],
  ['wall TV (wall/tv.ts)', (root, e, code, tok) => new m.WallTV(root, e, code, tok)],
]) {
  test(`${name}: the #screen= token is sent to host/signin as {token}, never as {key}`, async () => {
    const code = 'tv-1';
    const box = env(code);
    const root = document.getElementById('app');
    root.replaceChildren();
    const app = make(root, box.env, code, 'tok-screen-1');
    await app.start();
    const ins = box.calls.filter((c) => c.path === 'host/signin');
    assert.equal(ins.length, 1, JSON.stringify(box.calls));
    assert.deepEqual(ins[0], { method: 'POST', path: 'host/signin', body: { token: 'tok-screen-1' } });
    assert.equal(box.calls[0].path, 'host/signin', 'the sign-in is the first request');
    assert.ok(!box.calls.some((c) => c.body && 'key' in c.body), 'no request carries the token as the host key');
  });
}

test('screenSignIn posts exactly {token} to host/signin', async () => {
  const box = env('tv-2');
  const api = new m.Api(box.env, 'tv-2');
  await m.screenSignIn(api, 'abc');
  assert.deepEqual(box.calls, [{ method: 'POST', path: 'host/signin', body: { token: 'abc' } }]);
});
