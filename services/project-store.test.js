const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { createProjectStore } = require('./project-store');

async function createFixture(context, count = 3) {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'orbit-project-store-test-'));
  context.after(async () => {
    const relative = path.relative(os.tmpdir(), directory);
    assert.ok(relative.startsWith('orbit-project-store-test-') && !relative.includes(path.sep));
    await fs.rm(directory, { recursive: true, force: true });
  });
  const store = createProjectStore({ directory });
  const projects = [];
  for (let index = 0; index < count; index += 1) {
    projects.push(await store.ensureProject({ id: `project-${index}`, title: `Project ${index}` }));
  }
  const tabs = projects.map(project => ({ id: `tab-${project.id}`, projectId: project.id, title: 'Browser', kind: 'browser', url: 'https://example.com' }));
  const workspace = await store.save({
    schemaVersion: 1, initialized: true, projects, tabs,
    positions: Object.fromEntries(tabs.map((tab, index) => [tab.id, { x: index * 100, y: 20 }])),
    links: count > 2 ? [{ from: tabs[0].id, to: tabs[1].id }, { from: tabs[0].id, to: tabs[2].id }] : [],
    scene: { currentProjectId: projects[0].id, activeTabId: tabs[1]?.id || tabs[0].id, expandedId: tabs[0].id, order: Object.fromEntries(tabs.map((tab, index) => [tab.id, index])) }
  });
  const filename = path.join(directory, 'workspace.info');
  const saved = JSON.parse(await fs.readFile(filename, 'utf8'));
  const original = Buffer.from(`${JSON.stringify(saved, null, '\t').replaceAll('\n', '\r\n')}\r\n\r\n`);
  await fs.writeFile(filename, original);
  return { directory, filename, workspace, original };
}

async function removeProject(directory, project) {
  const relative = path.relative(directory, project.directory);
  assert.ok(relative && !relative.startsWith('..') && !path.isAbsolute(relative));
  await fs.rm(project.directory, { recursive: true, force: true });
}

async function recoveryFiles(directory) {
  return (await fs.readdir(directory)).filter(filename => /^workspace\.recovery-[\da-f-]+\.info$/.test(filename));
}

test('missing project folders recover surviving projects and archive the original bytes without rewriting files', async context => {
  const fixture = await createFixture(context);
  const removed = fixture.workspace.projects[0];
  const remaining = fixture.workspace.projects.slice(1);
  const originalProject = await fs.readFile(path.join(remaining[0].directory, 'project.info'));
  await removeProject(fixture.directory, removed);

  const restored = await createProjectStore({ directory: fixture.directory }).load();
  assert.deepEqual(restored.projects.map(project => project.id), remaining.map(project => project.id));
  assert.deepEqual(restored.tabs.map(tab => tab.id), ['tab-project-1', 'tab-project-2']);
  assert.deepEqual(Object.keys(restored.positions), ['tab-project-1', 'tab-project-2']);
  assert.deepEqual(restored.links, []);
  assert.equal(restored.scene.currentProjectId, null);
  assert.equal(restored.scene.activeTabId, 'tab-project-1');
  assert.equal(restored.scene.expandedId, null);
  assert.deepEqual(restored.scene.order, { 'tab-project-1': 1, 'tab-project-2': 2 });
  const archives = await recoveryFiles(fixture.directory);
  assert.equal(archives.length, 1);
  assert.deepEqual(await fs.readFile(path.join(fixture.directory, archives[0])), fixture.original);
  assert.deepEqual(await fs.readFile(fixture.filename), fixture.original);
  assert.deepEqual(await fs.readFile(path.join(remaining[0].directory, 'project.info')), originalProject);
  await assert.rejects(fs.stat(removed.directory), { code: 'ENOENT' });
});

test('recovery retains links and selections between surviving projects', async context => {
  const fixture = await createFixture(context);
  await removeProject(fixture.directory, fixture.workspace.projects[1]);
  const restored = await createProjectStore({ directory: fixture.directory }).load();
  assert.deepEqual(restored.links, [{ from: 'tab-project-0', to: 'tab-project-2' }]);
  assert.equal(restored.scene.currentProjectId, 'project-0');
  assert.equal(restored.scene.activeTabId, null);
  assert.equal(restored.scene.expandedId, 'tab-project-0');
});

test('recovery with every project folder missing allows new projects and windows to persist across restart', async context => {
  const fixture = await createFixture(context);
  for (const project of fixture.workspace.projects) await removeProject(fixture.directory, project);
  const store = createProjectStore({ directory: fixture.directory });
  const restored = await store.load();
  assert.deepEqual(restored.projects, []);
  assert.deepEqual(restored.tabs, []);
  assert.deepEqual(restored.positions, {});
  assert.deepEqual(restored.links, []);
  assert.equal(restored.scene.currentProjectId, null);
  assert.equal(restored.scene.activeTabId, null);
  assert.equal(restored.scene.expandedId, null);
  assert.deepEqual(restored.scene.order, {});
  assert.deepEqual(await fs.readFile(fixture.filename), fixture.original);
  const archives = await recoveryFiles(fixture.directory);
  assert.equal(archives.length, 1);

  const project = await store.ensureProject({ id: 'new-project', title: 'New project' });
  const tab = { id: 'new-tab', projectId: project.id, title: 'New browser', kind: 'browser', url: 'https://example.com/new' };
  await store.save({ ...restored, projects: [project], tabs: [tab], positions: { [tab.id]: { x: 40, y: 50 } }, scene: { ...restored.scene, currentProjectId: project.id, activeTabId: tab.id } });
  const restarted = await createProjectStore({ directory: fixture.directory }).load();
  assert.deepEqual(restarted.projects.map(record => record.id), [project.id]);
  assert.deepEqual(restarted.tabs, [tab]);
  assert.deepEqual(restarted.positions, { [tab.id]: { x: 40, y: 50 } });
  assert.equal(restarted.scene.currentProjectId, project.id);
  assert.equal(restarted.scene.activeTabId, tab.id);
  assert.deepEqual(await recoveryFiles(fixture.directory), archives);
  assert.deepEqual(await fs.readFile(path.join(fixture.directory, archives[0])), fixture.original);
});

test('complete workspaces load without recovery archives', async context => {
  const fixture = await createFixture(context);
  const restored = await createProjectStore({ directory: fixture.directory }).load();
  assert.equal(restored.projects.length, 3);
  assert.equal(restored.tabs.length, 3);
  assert.deepEqual(await recoveryFiles(fixture.directory), []);
  assert.deepEqual(await fs.readFile(fixture.filename), fixture.original);
});

for (const failure of ['missing project information', 'mismatched project id', 'invalid project schema']) {
  test(`recovery refuses ${failure} in an existing folder before creating an archive`, async context => {
    const fixture = await createFixture(context);
    await removeProject(fixture.directory, fixture.workspace.projects[0]);
    const filename = path.join(fixture.workspace.projects[1].directory, 'project.info');
    if (failure === 'missing project information') await fs.rm(filename);
    else {
      const document = JSON.parse(await fs.readFile(filename, 'utf8'));
      if (failure === 'mismatched project id') document.id = 'other-project';
      else document.schemaVersion = 2;
      await fs.writeFile(filename, JSON.stringify(document));
    }
    await assert.rejects(createProjectStore({ directory: fixture.directory }).load(), failure === 'invalid project schema' ? /Unsupported or incomplete schema/ : /Missing or mismatched project information/);
    assert.deepEqual(await recoveryFiles(fixture.directory), []);
    assert.deepEqual(await fs.readFile(fixture.filename), fixture.original);
  });
}

test('recovery rejects invalid workspace schemas and paths outside the projects directory', async context => {
  const fixture = await createFixture(context);
  await removeProject(fixture.directory, fixture.workspace.projects[0]);
  const document = JSON.parse(fixture.original);
  document.projects[1].directory = '../outside';
  await fs.writeFile(fixture.filename, JSON.stringify(document));
  await assert.rejects(createProjectStore({ directory: fixture.directory }).load(), /Path outside project directory/);
  assert.deepEqual(await recoveryFiles(fixture.directory), []);
  document.projects[1].directory = 'Project 1';
  document.schemaVersion = 2;
  await fs.writeFile(fixture.filename, JSON.stringify(document));
  await assert.rejects(createProjectStore({ directory: fixture.directory }).load(), /Unsupported or incomplete schema/);
  assert.deepEqual(await recoveryFiles(fixture.directory), []);
});
