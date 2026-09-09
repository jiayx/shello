import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { test } from 'node:test';
import { promisify } from 'node:util';

const exec = promisify(execFile);
const source = stripTypeScriptTypes(await readFile(new URL('../src/worker/bootstrap.ts', import.meta.url), 'utf8'));
const { renderShellBootstrap } = await import('data:text/javascript;base64,' + Buffer.from(source).toString('base64'));

test('concurrent bootstraps isolate downloads and clean up after download failure', async () => {
  const root = await mkdtemp(join(tmpdir(), 'shello-bootstrap-test-'));
  try {
    const bin = join(root, 'bin');
    const downloads = join(root, 'downloads');
    await mkdir(bin);
    await mkdir(downloads);
    const script = join(root, 'start.sh');
    await writeFile(script, renderShellBootstrap({ binaryBaseURL: 'https://invalid.test', checksumsURL: 'https://invalid.test/checksums', serverOrigin: 'https://invalid.test' }));
    await exec('sh', ['-n', script]);
    // Stop at the download boundary; no real network, Agent or terminal needed.
    await writeFile(join(bin, 'curl'), '#!/bin/sh\nfor arg do destination="$arg"; done\nprintf "%s\\n" "$destination" >> "$DOWNLOAD_LOG"\nexit 22\n', { mode: 0o755 });
    const log = join(root, 'paths');
    const env = { ...process.env, PATH: bin + ':' + process.env.PATH, TMPDIR: downloads, DOWNLOAD_LOG: log };
    const results = await Promise.allSettled([exec('sh', [script], { env }), exec('sh', [script], { env })]);
    assert.ok(results.every(result => result.status === 'rejected' && result.reason.code === 22));
    const paths = (await readFile(log, 'utf8')).trim().split('\n');
    assert.equal(paths.length, 2);
    assert.notEqual(dirname(paths[0]), dirname(paths[1]));
    assert.deepEqual(await readdir(downloads), []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
