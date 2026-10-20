// Existing smoke test for the cases slug() already handles.
import assert from 'node:assert/strict';
import { slug } from './slug.mjs';
assert.equal(slug('hello world'), 'hello-world');
console.log('smoke ok');
