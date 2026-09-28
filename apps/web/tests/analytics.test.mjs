import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import { test } from 'node:test';

const source = stripTypeScriptTypes(await readFile(new URL('../src/client/analytics.ts', import.meta.url), 'utf8'));
let instance = 0;

async function setup(env) {
  const scripts = [];
  const listeners = new Map();
  globalThis.window = {
    location: new URL('https://shello.example/s/abc-234?token=secret#private'),
    addEventListener: (name, callback) => listeners.set(name, callback),
  };
  globalThis.document = {
    title: 'Shello',
    referrer: 'https://shello.example/s/def-567?token=previous',
    createElement: () => ({}),
    head: { appendChild: (script) => scripts.push(script) },
  };
  const code = source.replaceAll('import.meta.env', JSON.stringify(env));
  const analytics = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64') + `#${instance++}`);
  return { ...analytics, scripts, listeners, commands: () => (window.dataLayer || []).map((args) => Array.from(args)) };
}

test('development and missing IDs disable both loading and events', async () => {
  for (const env of [{ PROD: false, VITE_GA_MEASUREMENT_ID: 'G-TEST' }, { PROD: true }]) {
    const analytics = await setup(env);
    analytics.initAnalytics();
    analytics.trackEvent('test_event');
    assert.equal(analytics.scripts.length, 0);
    assert.deepEqual(analytics.commands(), []);
  }
});

test('initialization is idempotent and leaves pageviews to GA', async () => {
  const analytics = await setup({ PROD: true, VITE_GA_MEASUREMENT_ID: 'G-TEST' });
  analytics.initAnalytics();
  analytics.initAnalytics();
  assert.equal(analytics.scripts.length, 1);
  assert.equal(analytics.scripts[0].async, true);
  assert.equal(analytics.scripts[0].src, 'https://www.googletagmanager.com/gtag/js?id=G-TEST');
  const commands = analytics.commands();
  assert.equal(commands.length, 2);
  assert.equal(commands[0][0], 'js');
  assert.deepEqual(commands[1], ['config', 'G-TEST']);
  assert.equal(analytics.listeners.size, 0);

  analytics.trackEvent('session_created', { open_in_new_tab: false });
  assert.deepEqual(analytics.commands().at(-1), ['event', 'session_created', { open_in_new_tab: false }]);
});
