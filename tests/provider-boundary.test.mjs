import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const mise = process.env.STACK_TEST_MISE;
const binary = process.env.STACK_TEST_BINARY;
test('real mise cannot reinterpret locked tools through project, parent, global or inherited aliases',
  { skip: !mise || !binary, timeout: 180000 }, () => {
  const work = realpathSync(mkdtempSync('/tmp/stack-provider-'));
  try {
    const project = path.join(work, 'parent', 'app');
    const home = path.join(work, 'home');
    mkdirSync(project, { recursive: true });
    mkdirSync(path.join(home, '.config/mise'), { recursive: true });
    writeFileSync(path.join(project, 'stack.toml'), '[tools]\npython="3.13"\n');
    writeFileSync(path.join(project, 'stack.lock'), 'version=2\n[[tool]]\nname="python"\nrequested="3.13"\nresolved="3.13.16"\n');
    const aliases = '[alias.python.versions]\n"3.13"="3.12.9"\n"3.13.16"="3.12.9"\n[env]\nSTACK_ALIAS_LEAK="yes"\n';
    writeFileSync(path.join(home, '.config/mise/config.toml'), aliases);
    writeFileSync(path.join(project, 'mise.toml'), aliases);
    writeFileSync(path.join(work, 'parent/mise.toml'), '[alias.python]\nbackend="asdf:invalid.example/python"\n');
    writeFileSync(path.join(work, 'override.toml'), aliases);
    const env = { ...process.env, HOME: home, PATH: `${path.dirname(mise)}:${process.env.PATH}`,
      STACK_STATE_DIR: path.join(work, 'state'), STACK_CACHE_DIR: path.join(work, 'parent/cache'),
      MISE_DATA_DIR: path.join(work, 'data'), MISE_CACHE_DIR: path.join(work, 'cache'),
      MISE_STATE_DIR: path.join(work, 'mise-state'), MISE_CONFIG_FILE: path.join(work, 'override.toml'),
      MISE_GLOBAL_CONFIG_FILE: path.join(home, '.config/mise/config.toml'),
      MISE_SYSTEM_CONFIG_FILE: path.join(work, 'override.toml'), MISE_YES: '1' };
    const stack = (...args) => execFileSync(binary, ['-C', project, ...args], { env, encoding: 'utf8', timeout: 150000 });
    stack('compile', '--locked');
    const tools = JSON.parse(stack('exec', '--', mise, 'ls', '--json'));
    assert.ok(tools.python, JSON.stringify(tools));
    assert.equal(tools.python[0].version, '3.13.16');
    assert.equal(tools.python[0].requested_version, '3.13.16');
    const runtime = JSON.parse(stack('exec', '--', mise, 'env', '--json'));
    assert.equal(runtime.STACK_ALIAS_LEAK, undefined);
    // The cache lies underneath an aliased parent; resolution must still use the requested line.
    const updated = JSON.parse(stack('compile', '--update', '--json'));
    const python = updated.data.versions.find(v => v.name === 'python');
    assert.match(python.resolved, /^3\.13\.[0-9]+/);
    assert.notEqual(python.resolved, '3.12.9');
  } finally { rmSync(work, { recursive: true, force: true }); }
});
