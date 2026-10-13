const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { createProjectStore } = require('./project-store');
const { isTransientAuthenticationAddress, restoreAddress } = require('./navigation-addresses');

const stableUrl = 'https://www.figma.com/design/project/Sketch?node-id=1-2';
const callbackUrl = 'https://www.figma.com/finish_google_sso?state=fixture-state&code=fixture-code&authuser=0';

test('Figma authentication callbacks recover stable navigation without affecting design URLs', () => {
  assert.equal(isTransientAuthenticationAddress(stableUrl), false);
  assert.equal(isTransientAuthenticationAddress(callbackUrl), true);
  assert.equal(restoreAddress({ kind: 'figma', url: callbackUrl, state: { restoreUrl: stableUrl } }), stableUrl);
  assert.equal(restoreAddress({ kind: 'figma', url: callbackUrl, state: { figma: { url: stableUrl } } }), stableUrl);
  assert.equal(restoreAddress({ kind: 'browser', url: callbackUrl }), 'https://www.figma.com/login');
  assert.equal(restoreAddress({ kind: 'browser', url: 'https://example.com/callback?code=ordinary-content' }), 'https://example.com/callback?code=ordinary-content');
});

test('project persistence removes OAuth callbacks from both tab URLs and saved history', async context => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'orbit-navigation-test-'));
  context.after(async () => {
    const relative = path.relative(os.tmpdir(), directory);
    assert.ok(relative.startsWith('orbit-navigation-test-') && !relative.includes(path.sep));
    await fs.rm(directory, { recursive: true, force: true });
  });
  const store = createProjectStore({ directory });
  const project = await store.ensureProject({ id: 'project', title: 'OAuth fixture' });
  await store.save({ schemaVersion: 1, initialized: true, projects: [project], tabs: [{
    id: 'figma', projectId: project.id, title: 'Figma', kind: 'figma', url: callbackUrl,
    state: { restoreUrl: stableUrl, navigation: { entries: [{ url: stableUrl }, { url: callbackUrl }], index: 1 }, surface: { url: callbackUrl } }
  }], positions: { figma: { x: 10, y: 20 } }, links: [], scene: {} });
  for (const filename of [path.join(directory, 'workspace.info'), path.join(project.directory, 'project.info')]) {
    const contents = await fs.readFile(filename, 'utf8');
    assert.doesNotMatch(contents, /fixture-code|fixture-state|finish_google_sso/);
  }
  const restored = await store.load();
  assert.equal(restored.tabs[0].url, stableUrl);
  assert.deepEqual(restored.positions.figma, { x: 10, y: 20 });
  const projectFilename = path.join(project.directory, 'project.info');
  const legacy = JSON.parse(await fs.readFile(projectFilename, 'utf8'));
  legacy.session.elements[0].url = callbackUrl;
  legacy.session.elements[0].state.surface.url = callbackUrl;
  await fs.writeFile(projectFilename, JSON.stringify(legacy));
  const migrated = createProjectStore({ directory });
  assert.equal((await migrated.load()).tabs[0].url, stableUrl);
  await migrated.ensureProject({ id: project.id, title: project.title });
  assert.doesNotMatch(await fs.readFile(projectFilename, 'utf8'), /fixture-code|fixture-state|finish_google_sso/);
});
