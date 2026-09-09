import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import { test } from 'node:test';

const source = stripTypeScriptTypes(await readFile(new URL('../src/client/pairing.ts', import.meta.url), 'utf8'));
const { parsePairingInput } = await import('data:text/javascript;base64,' + Buffer.from(source).toString('base64'));
const origin = 'https://shello.tools.tf';

test('accepts formatted codes, compact codes, and same-site sharing links', () => {
  for (const value of ['abc-234', ' ABC234 ', 'ABC-234', `${origin}/s/abc-234`, `${origin}/s/abc-234/?x=1`]) {
    assert.equal(parsePairingInput(value, origin), 'abc-234');
  }
});

test('rejects malformed codes, unrelated URLs, and links from other deployments', () => {
  for (const value of ['', 'abc-01l', 'abc--234', 'ab-c234', 'abc2345', `${origin}/start?session=abc-234`, 'https://other.example/s/abc-234', 'javascript:alert(1)', `${origin}.evil.example/s/abc-234`]) {
    assert.equal(parsePairingInput(value, origin), null, value);
  }
});
