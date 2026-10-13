const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { spawn } = require('node:child_process');
const { createHash } = require('node:crypto');
const { createInterface } = require('node:readline');
const { once } = require('node:events');
const { WebSocket } = require('ws');
const { createProjectStore } = require('./project-store');

const applicationDirectory = path.resolve(__dirname, '..');

async function digestFiles(directory) {
  const digests = new Map();
  async function visit(current) {
    for (const entry of await fs.readdir(current, { withFileTypes: true })) {
      const filename = path.join(current, entry.name);
      if (entry.isDirectory()) await visit(filename);
      else digests.set(path.relative(directory, filename), createHash('sha256').update(await fs.readFile(filename)).digest('hex'));
    }
  }
  await visit(directory);
  return digests;
}

function launchHost(projectsDirectory) {
  const child = spawn(process.execPath, [path.join(__dirname, 'host.js'), '--projects', projectsDirectory], {
    cwd: applicationDirectory, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, ORBIT_EMBEDDINGS: '0' }
  });
  const pending = new Map();
  const packets = [];
  let sequence = 0;
  let errors = '';
  let resolveReady;
  let rejectReady;
  const ready = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
  const reader = createInterface({ input: child.stdout });
  child.stderr.on('data', chunk => { errors += chunk.toString(); });
  reader.on('line', line => {
    let packet;
    try { packet = JSON.parse(line); } catch { rejectReady(new Error(`Non-JSON stdout: ${line}`)); return; }
    packets.push(packet);
    if (packet.type === 'ready') resolveReady(packet);
    if (packet.type === 'response') {
      const waiting = pending.get(packet.id);
      if (!waiting) return;
      pending.delete(packet.id);
      clearTimeout(waiting.timeout);
      packet.error ? waiting.reject(new Error(packet.error)) : waiting.resolve(packet.result);
    }
  });
  child.on('error', rejectReady);
  child.on('exit', code => {
    const error = new Error(`Sidecar exited ${code}: ${errors}`);
    rejectReady(error);
    for (const waiting of pending.values()) { clearTimeout(waiting.timeout); waiting.reject(error); }
    pending.clear();
  });
  function invoke(method, params, duration = 15000) {
    return new Promise((resolve, reject) => {
      const id = ++sequence;
      const timeout = setTimeout(() => { pending.delete(id); reject(new Error(`Timeout ${method}: ${errors}`)); }, duration);
      pending.set(id, { resolve, reject, timeout });
      child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
    });
  }
  return { child, ready, invoke, packets, errors: () => errors };
}

async function api(ready, tabId, method, params = {}) {
  const response = await fetch(`${ready.origin}/api/tab/${encodeURIComponent(tabId)}/${method}`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${ready.token}`, Origin: ready.origin }, body: JSON.stringify(params)
  });
  const result = await response.json();
  assert.equal(response.status, 200, result.error);
  assert.equal(result.error, undefined);
  return result.result;
}

test('local sidecar preserves original projects and serves real apps', { timeout: 240000 }, async context => {
  const originalDirectory = path.resolve(applicationDirectory, '..', 'new-ux-mockup', 'test', 'browser-universe', 'projects');
  const originalDigest = await digestFiles(originalDirectory).catch(error => { if (error.code !== 'ENOENT') throw error; return null; });
  const temporaryDirectory = await fs.mkdtemp(path.join(os.tmpdir(), 'orbit-rust-test-'));
  const fixtureDirectory = path.join(temporaryDirectory, 'projects');
  const store = createProjectStore({ directory: fixtureDirectory });
  const projects = [];
  for (let index = 0; index < 3; index += 1) {
    projects.push(await store.ensureProject({ id: `fixture-${index}`, title: `Fixture ${index}` }));
  }
  const pdfFilename = path.join(temporaryDirectory, 'fixture.pdf');
  await fs.writeFile(pdfFilename, '%PDF-1.4\n%%EOF\n');
  const resource = await store.importFile({ projectId: projects[0].id, sourcePath: pdfFilename });
  await store.save({
    schemaVersion: 1, initialized: true, projects,
    tabs: [{ id: 'fixture-pdf', projectId: projects[0].id, title: 'Fixture PDF', kind: 'pdf', url: '', resource }],
    positions: {}, links: [], scene: { currentProjectId: projects[0].id }
  });
  const host = launchHost(fixtureDirectory);
  context.after(async () => {
    if (host.child.exitCode === null) await host.invoke('shutdown', {}, 20000).catch(() => host.child.kill());
    const relative = path.relative(os.tmpdir(), temporaryDirectory);
    assert.ok(relative.startsWith('orbit-rust-test-') && !relative.includes(path.sep));
    await fs.rm(temporaryDirectory, { recursive: true, force: true });
  });
  const ready = await host.ready;
  assert.equal(ready.type, 'ready');
  assert.ok(ready.workspace.projects.length >= 3);
  assert.ok(ready.workspace.projects.every(project => project.directory.startsWith(fixtureDirectory)));
  assert.match(ready.origin, /^http:\/\/127\.0\.0\.1:\d+$/);
  assert.equal((await fetch(`${ready.origin}/fonts/space-grotesk.ttf`)).status, 403);
  assert.equal((await fetch(`${ready.origin}/fonts/space-grotesk.ttf?token=${ready.token}`)).status, 200);
  assert.equal((await fetch(`${ready.origin}/fonts/space-grotesk.ttf?token=${ready.token}`, { headers: { Origin: 'https://example.com' } })).status, 403);

  const projectId = ready.workspace.projects[0].id;
  const browser = await host.invoke('createTab', { projectId, kind: 'browser', url: 'https://example.com/path', title: 'Real browser' });
  const browserAddress = await host.invoke('resolveTab', { tab: browser });
  assert.equal(browserAddress.url, 'https://example.com/path');
  const figma = await host.invoke('createTab', { projectId, kind: 'figma' });
  assert.equal(new URL((await host.invoke('resolveTab', { id: figma.id })).url).hostname, 'www.figma.com');
  await assert.rejects(host.invoke('navigate', { id: browser.id, url: 'javascript:alert(1)' }), /HTTP o HTTPS/);
  const navigated = await host.invoke('navigate', { id: browser.id, url: 'https://example.com/changed' });
  assert.equal(navigated.url, 'https://example.com/changed');

  const pdf = ready.workspace.tabs.find(tab => tab.kind === 'pdf');
  assert.ok(pdf);
  const pdfAddress = await host.invoke('resolveTab', { id: pdf.id });
  assert.equal(new URL(pdfAddress.url).origin, ready.origin);
  const pdfResponse = await fetch(pdfAddress.url, { headers: { Range: 'bytes=0-3' } });
  assert.equal(pdfResponse.status, 206);
  assert.equal(await pdfResponse.text(), '%PDF');

  const chat = await host.invoke('createTab', { projectId, kind: 'chat' });
  const chatAddress = await host.invoke('resolveTab', { id: chat.id });
  const chatDocument = await fetch(chatAddress.url);
  assert.equal(chatDocument.status, 200);
  assert.match(await chatDocument.text(), /bridge\.js/);
  const cookie = chatDocument.headers.get('set-cookie').split(';')[0];
  assert.equal((await fetch(`${ready.origin}/content/bridge.js`, { headers: { Cookie: cookie } })).status, 200);
  assert.equal((await api(ready, chat.id, 'context')).project.id, projectId);
  await api(ready, chat.id, 'conversation');
  await api(ready, chat.id, 'saveConversation', { draft: 'Persisted draft' });
  assert.equal((await api(ready, chat.id, 'conversation')).draft, 'Persisted draft');
  const chatRuntime = await api(ready, chat.id, 'runtime');
  assert.ok(Array.isArray(chatRuntime.providers));
  assert.equal(chatRuntime.conversation.draft, 'Persisted draft');

  const terminal = await host.invoke('createTab', { projectId, kind: 'terminal' });
  const terminalAddress = new URL((await host.invoke('resolveTab', { id: terminal.id })).url);
  assert.equal((await fetch(terminalAddress)).status, 200);
  const socketAddress = new URL('/terminal/socket', terminalAddress);
  socketAddress.protocol = 'ws:';
  socketAddress.search = terminalAddress.search;
  const connection = new WebSocket(socketAddress, { origin: terminalAddress.origin });
  let transcript = '';
  const terminalOutput = new Promise((resolve, reject) => {
    const deadline = setTimeout(() => reject(new Error(`PTY output missing: ${transcript}`)), 15000);
    connection.on('error', reject);
    connection.on('message', contents => {
      const packet = JSON.parse(contents);
      if (packet.type === 'ready') {
        assert.equal(packet.workspace, ready.workspace.projects[0].directory);
        connection.send(JSON.stringify({ type: 'input', text: "Write-Output ('ORBIT' + '-RUST-' + 'PTY-' + 'SUCCESS')\r" }));
      }
      if (packet.type === 'output') {
        transcript += packet.text;
        if (transcript.includes('ORBIT-RUST-PTY-SUCCESS')) { clearTimeout(deadline); resolve(); }
      }
    });
  });
  await terminalOutput;
  connection.close();
  await once(connection, 'close');

  const code = await host.invoke('createTab', { projectId, kind: 'code' });
  const codeAddress = await host.invoke('resolveTab', { id: code.id }, 150000);
  const challenge = await fetch(codeAddress.url, { redirect: 'manual' });
  const editor = challenge.status >= 300 && challenge.status < 400
    ? await fetch(new URL(challenge.headers.get('location'), codeAddress.url), { headers: { Cookie: challenge.headers.getSetCookie().map(value => value.split(';')[0]).join('; ') } })
    : challenge;
  assert.equal(editor.status, 200);
  assert.match(await editor.text(), /workbench|vscode-workbench-web-configuration/i);
  assert.ok(new URL(codeAddress.url).searchParams.get('workspace').includes(code.id));

  await host.invoke('saveWorkspace', { scene: { orbit: { distance: 25, yaw: 1.5 }, currentProjectId: projectId }, links: [{ from: chat.id, to: browser.id }] });
  assert.equal((await api(ready, chat.id, 'context')).elements[0].id, browser.id);
  const events = await fetch(`${ready.origin}/api/tab/${chat.id}/events?token=${ready.token}`, { signal: AbortSignal.timeout(10000) });
  assert.equal(events.headers.get('content-type'), 'text/event-stream; charset=utf-8');
  const reader = events.body.getReader();
  const quit = once(host.child, 'exit');
  const shutdown = host.invoke('shutdown', {}, 20000);
  let eventsText = '';
  while (!eventsText.includes('event: beforeClose')) {
    const event = await reader.read();
    assert.equal(event.done, false);
    eventsText += Buffer.from(event.value).toString();
  }
  await api(ready, chat.id, 'saveConversation', { draft: 'Last draft before close' });
  await api(ready, chat.id, 'readyToClose');
  assert.deepEqual(await shutdown, { stopped: true });
  await reader.cancel();
  assert.equal((await quit)[0], 0);
  assert.doesNotMatch(host.errors(), /AttachConsole failed|conpty_console_list_agent/);
  if (originalDigest) assert.deepEqual(await digestFiles(originalDirectory), originalDigest);
  await assert.rejects(fetch(ready.origin), /fetch failed/);
  await assert.rejects(fetch(terminalAddress), /fetch failed/);
  await assert.rejects(fetch(codeAddress.url), /fetch failed/);

  const restarted = launchHost(fixtureDirectory);
  context.after(async () => { if (restarted.child.exitCode === null) await restarted.invoke('shutdown', {}, 20000).catch(() => restarted.child.kill()); });
  const restored = await restarted.ready;
  assert.deepEqual(restored.workspace.scene.orbit, { distance: 25, yaw: 1.5 });
  assert.ok(restored.workspace.tabs.find(tab => tab.id === terminal.id).state.terminal.transcript.includes('ORBIT-RUST-PTY-SUCCESS'));
  assert.equal((await api(restored, chat.id, 'conversation')).draft, 'Last draft before close');
  await restarted.invoke('closeTab', { id: browser.id });
  assert.ok(!(await restarted.invoke('getWorkspace')).tabs.some(tab => tab.id === browser.id));
  assert.equal((await api(restored, chat.id, 'context')).elements.length, 0);
  const restartedQuit = once(restarted.child, 'exit');
  await restarted.invoke('shutdown', {}, 20000);
  assert.equal((await restartedQuit)[0], 0);
});
