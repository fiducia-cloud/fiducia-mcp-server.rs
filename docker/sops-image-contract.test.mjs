import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import test from 'node:test';

const image = process.env.CONTAINER_TEST_IMAGE;
const arch = process.env.CONTAINER_TEST_ARCH;
assert.ok(image, 'CONTAINER_TEST_IMAGE is required');
assert.ok(['amd64', 'arm64'].includes(arch), 'CONTAINER_TEST_ARCH must be amd64 or arm64');
function docker(args) {
  const result = spawnSync('docker', args, { encoding: 'utf8', timeout: 30000, maxBuffer: 1024 * 1024 });
  assert.ifError(result.error);
  return result;
}
function run(args, options = []) {
  const name = `sops-contract-${randomUUID()}`;
  try {
    return docker(['run', '--rm', '--name', name, '--platform', `linux/${arch}`,
      '--read-only', '--network', 'none', '--cap-drop=ALL', '--security-opt=no-new-privileges',
      '--env', 'SOPS_SECRETS_FILE=/entrypoint-contract/missing', '--env', 'SOPS_REQUIRE_KEY=0',
      '--env', 'SOPS_AGE_KEY=', '--env', 'SOPS_AGE_KEY_FILE=', ...options, image, ...args]);
  } finally {
    spawnSync('docker', ['container', 'rm', '--force', name], { stdio: 'ignore', timeout: 10000 });
  }
}
const wrapper = '/usr/local/bin/sops-entrypoint.sh';
test('MCP image keeps its fixed executable, platform, and explicit non-root tool-runner profile', () => {
  const result = docker(['image', 'inspect', image]);
  assert.equal(result.status, 0, result.stderr);
  const [metadata] = JSON.parse(result.stdout);
  assert.equal(metadata.Os, 'linux');
  assert.equal(metadata.Architecture, arch);
  assert.deepEqual(metadata.Config.Entrypoint, [wrapper, '/usr/local/bin/fiducia-mcp']);
  assert.deepEqual(metadata.Config.Cmd ?? [], []);
  assert.equal(metadata.Config.User, '65532:65532');
  assert.equal(metadata.Config.Labels['org.fiducia.runtime-profile'], 'tool-runner-nonroot');
});
test('runtime flags reach the actual MCP parser without stdout contamination', () => {
  const result = run(['--entrypoint-contract-invalid-option']);
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /unknown command-line option/);
  assert.equal(result.stdout, '');
  assert.doesNotMatch(result.stderr, /not found|cannot execute|permission denied/i);
});
test('SOPS wrapper preserves explicit command operands, streams, and exit', () => {
  const value = 'SYNTHETIC_$(not-a-command) * two words';
  const result = run(['/bin/sh', '-c', 'printf "%s" "$1"; printf diagnostic >&2; exit 23', 'probe', value], ['--entrypoint', wrapper]);
  assert.equal(result.status, 23);
  assert.equal(result.stdout, value);
  assert.equal(result.stderr, 'diagnostic');
});
test('runtime binaries/schema are root-owned, read-only to the service, and CA is present', () => {
  const result = run(['/bin/sh', '-c',
    'test "$(id -u)" = 65532 && test "$(stat -c %u:%g:%a /usr/local/bin/fiducia-mcp)" = 0:0:555 && test "$(stat -c %u:%g:%a /usr/local/bin/kubectl)" = 0:0:555 && test "$(stat -c %u:%g:%a /usr/local/share/fiducia-mcp-server/.cli-flags.toml)" = 0:0:444 && test -s /etc/ssl/certs/ca-certificates.crt'],
    ['--entrypoint', wrapper]);
  assert.equal(result.status, 0, result.stderr);
});
test('required ciphertext and real SOPS decryption failure both fail closed', () => {
  const missing = run([], ['--env', 'SOPS_REQUIRE_KEY=1']);
  assert.equal(missing.status, 1);
  assert.equal(missing.stdout, '');
  assert.equal(missing.stderr, 'sops-entrypoint: required ciphertext is unavailable or not a regular file\n');
  const invalid = run([], ['--env', 'SOPS_REQUIRE_KEY=1', '--env', 'SOPS_SECRETS_FILE=/app/secrets/app.env',
    '--env', 'SOPS_AGE_KEY=SYNTHETIC_NOT_AN_AGE_IDENTITY']);
  assert.equal(invalid.status, 1);
  assert.equal(invalid.stdout, '');
  assert.equal(invalid.stderr, 'sops-entrypoint: decryption failed\n');
});
test('checksum-selected kubectl actually executes on the requested architecture without a cluster', () => {
  const result = run(['/usr/local/bin/kubectl', 'version', '--client=true', '--output=json'], ['--entrypoint', wrapper]);
  assert.equal(result.status, 0, result.stderr);
  const version = JSON.parse(result.stdout).clientVersion;
  assert.equal(version.gitVersion, 'v1.34.1');
  assert.equal(version.platform, `linux/${arch}`);
});
