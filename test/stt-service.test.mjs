import test from 'node:test';
import assert from 'node:assert/strict';
import { MODELS, status } from '../desktop-host/stt/service.mjs';

test('IRIS STT manifest pins the two verified command assets', async () => {
  assert.equal(MODELS.length, 2);
  assert.equal(MODELS[0].bytes, 116204861);
  assert.match(MODELS[0].sha256, /^[a-f0-9]{64}$/);
  const root = await import('node:fs/promises').then(fs => fs.mkdtemp('/tmp/async-stt-'));
  const value = await status(root);
  assert.equal(value.ready, false);
});
