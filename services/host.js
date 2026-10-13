const fs = require('node:fs/promises');
const path = require('node:path');
const http = require('node:http');
const { fork, spawn } = require('node:child_process');
const { randomBytes, randomUUID, timingSafeEqual } = require('node:crypto');
const { createInterface } = require('node:readline');
const { createProjectStore } = require('./project-store');
const { startCodeServer, prepareCodeWorkspace, buildCodeAddress } = require('./code-server');
const { createKnowledgeService } = require('./knowledge-service');
const { createCliRuntime } = require('./cli-runtime');
const { createChatService } = require('./chat-service');
const { cleanText } = require('./knowledge-extractors');
const { isFigmaAddress, isTransientAuthenticationAddress, navigationStateForDisk } = require('./navigation-addresses');

const applicationDirectory = path.resolve(__dirname, '..');
const projectsArgument = process.argv.indexOf('--projects');
const projectsDirectory = path.resolve(projectsArgument >= 0 ? process.argv[projectsArgument + 1] || '' : process.env.ORBIT_PROJECTS_DIRECTORY || path.join(applicationDirectory, 'projects'));
const runtimeDirectory = path.resolve(process.env.ORBIT_RUNTIME_DIRECTORY || path.join(applicationDirectory, '.runtime'));
const token = randomBytes(32).toString('hex');
const projectStore = createProjectStore({ directory: projectsDirectory });
const tabs = new Map();
const pageCaptures = new Map();
const subscribers = new Map();
const closeAcknowledgements = new Map();
const terminalRequests = new Map();
const lifetime = new AbortController();
const mimeTypes = {
  '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8', '.json': 'application/json; charset=utf-8',
  '.ttf': 'font/ttf', '.woff2': 'font/woff2', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg',
  '.webp': 'image/webp', '.avif': 'image/avif', '.gif': 'image/gif',
  '.pdf': 'application/pdf', '.txt': 'text/plain; charset=utf-8'
};
let workspace;
let origin;
let cookieName;
let terminalProcess;
let terminalReady;
let terminalAddress;
let terminalFlush;
let codeReady;
let codeServer;
let stopping = false;
let stopPromise;
let saveTimer;
let writes = Promise.resolve();
let protocolOpen = true;

const diagnosticMessage = error => cleanText(error?.message || error).replaceAll(token, '[redacted]');
const publish = packet => { if (protocolOpen) process.stdout.write(`${JSON.stringify(packet)}\n`); };
const report = error => process.stderr.write(`${diagnosticMessage(error)}\n`);
const clone = value => structuredClone(value);
const knowledge = createKnowledgeService({ nodeExecutable: process.execPath, cacheDirectory: process.env.ORBIT_EMBEDDING_CACHE || path.join(runtimeDirectory, 'model-cache'), embeddings: process.env.ORBIT_EMBEDDINGS !== '0', onStatus: status => chat.handleKnowledgeStatus(status) });
const cli = createCliRuntime({ nodeExecutable: process.execPath, onEvent: event => chat.handleCliEvent(event) });
const chat = createChatService({ projectStore, knowledge, cli, collectSources, getContext: chatContext, onTurn: (id, turn) => {
  emitTab(id, 'turn', turn);
  if (['completed', 'error', 'cancelled'].includes(turn.type)) scheduleSave();
} });

function snapshot() {
  return {
    ...clone(workspace),
    projects: workspace.projects.map(project => ({ ...projectStore.getProject(project.id), x: project.x, y: project.y })),
    tabs: [...tabs.values()].map(tab => clone(tab))
  };
}

function save() {
  clearTimeout(saveTimer);
  saveTimer = undefined;
  writes = writes.catch(() => {}).then(async () => {
    workspace = await projectStore.save(snapshot());
    return snapshot();
  });
  return writes;
}

function scheduleSave() {
  if (!stopping && !saveTimer) saveTimer = setTimeout(() => save().catch(report), 400);
}

function emitTab(id, event, contents) {
  for (const response of subscribers.get(id) || []) response.write(`event: ${event}\ndata: ${JSON.stringify(contents)}\n\n`);
}

function flushChatDocument(tab) {
  if (tab.kind !== 'chat' || !subscribers.get(tab.id)?.size) return Promise.resolve();
  return new Promise(resolve => {
    const deadline = setTimeout(finish, 1500);
    function finish() { clearTimeout(deadline); closeAcknowledgements.delete(tab.id); resolve(); }
    closeAcknowledgements.set(tab.id, finish);
    emitTab(tab.id, 'beforeClose', { closing: true });
  });
}

function publishWorkspace() {
  for (const tab of tabs.values()) if (tab.kind === 'chat') emitTab(tab.id, 'context', chatContext(tab));
  publish({ type: 'event', event: 'workspace', workspace: snapshot() });
}

function chatContext(tab) {
  const linkedIds = new Set(workspace.links.flatMap(link => link.from === tab.id ? [link.to] : link.to === tab.id ? [link.from] : []));
  return {
    project: { id: tab.projectId, title: projectStore.getProject(tab.projectId)?.title || 'Progetto' },
    elements: [...linkedIds].map(id => tabs.get(id)).filter(Boolean).map(({ id, title, kind, url }) => ({ id, title, kind, url: publicAddress(url) }))
  };
}

function publicAddress(address) {
  try {
    const url = new URL(address);
    if (isTransientAuthenticationAddress(address)) return '';
    if (!['http:', 'https:'].includes(url.protocol) || /(?:^|[/._-])(login|signin|oauth|callback|auth)(?:[/._-]|$)/i.test(`${url.hostname}${url.pathname}`)) return '';
    url.username = '';
    url.password = '';
    for (const name of [...url.searchParams.keys()]) if (/token|secret|auth|session|credential|key/i.test(name)) url.searchParams.delete(name);
    return url.href;
  } catch { return ''; }
}

async function collectSources(chatTab) {
  await captureSoftwareStates();
  const linked = new Set(chatContext(chatTab).elements.map(element => element.id));
  const candidates = [...tabs.values()].filter(tab => tab.projectId === chatTab.projectId || linked.has(tab.id));
  const liveSources = [];
  const capturedAt = new Date().toISOString();
  for (const tab of candidates) {
    const captured = pageCaptures.get(tab.id);
    if (['browser', 'figma'].includes(tab.kind) && captured?.text && publicAddress(captured.url)) {
      liveSources.push({ id: `page:${tab.id}`, tabId: tab.id, title: tab.title, kind: tab.kind, text: captured.text, url: publicAddress(captured.url), capturedAt: captured.capturedAt });
    }
    if (tab.kind === 'terminal' && tab.state?.terminal?.transcript) {
      const text = tab.state.terminal.transcript.replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, '').replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '').replace(/[\x00-\x08\x0b-\x1f]/g, '').slice(-80000);
      liveSources.push({ id: `terminal:${tab.id}`, tabId: tab.id, title: tab.title, kind: 'terminal', text, capturedAt });
    }
    if (tab.kind === 'code') for (const group of tab.state?.editor?.groups || []) for (const editor of group.editors || []) {
      if (typeof editor.text !== 'string' || !editor.dirty) continue;
      const project = projectStore.getProject(tab.projectId);
      const filename = editor.relativePath && !path.isAbsolute(editor.relativePath) && !editor.relativePath.split(/[\\/]/).includes('..') ? path.resolve(project.directory, editor.relativePath) : undefined;
      liveSources.push({ id: `buffer:${tab.id}:${editor.uri}`, tabId: tab.id, title: `${editor.relativePath || 'Documento senza nome'} · modifiche non salvate`, kind: 'code', text: editor.text.slice(0, 80000), ...(filename ? { path: filename } : {}), capturedAt });
    }
  }
  return {
    tabs: candidates.map(tab => ({ ...clone(tab), state: navigationStateForDisk(tab), url: ['browser', 'figma'].includes(tab.kind) ? publicAddress(tab.url) : '' })),
    liveSources
  };
}

function requireTab(id, kind) {
  const tab = tabs.get(id);
  if (!tab || kind && tab.kind !== kind) throw new Error('Elemento non disponibile.');
  return tab;
}

async function conversation(tab, legacy = []) {
  const existing = await projectStore.readChat({ projectId: tab.projectId, tabId: tab.id });
  if (existing) return existing;
  const saved = await projectStore.writeChat({ projectId: tab.projectId, tabId: tab.id, title: tab.title, conversation: { messages: Array.isArray(legacy) ? legacy : [], draft: '' } });
  tab.resource = saved.reference;
  scheduleSave();
  return saved.conversation;
}

function beginTerminal() {
  if (terminalReady) return terminalReady;
  terminalReady = new Promise((resolve, reject) => {
    terminalProcess = fork(path.join(__dirname, 'terminal-server.js'), [], { execPath: process.execPath, cwd: __dirname, silent: true, windowsHide: true });
    const deadline = setTimeout(() => reject(new Error('Il terminale locale non risponde.')), 15000);
    terminalProcess.stdout.on('data', chunk => report(chunk.toString()));
    terminalProcess.stderr.on('data', chunk => report(chunk.toString()));
    terminalProcess.on('message', message => {
      if (message.type === 'ready') {
        clearTimeout(deadline);
        terminalAddress = message.url;
        resolve(message.url);
      }
      if (message.type === 'terminal-state') {
        const tab = tabs.get(message.id);
        if (tab?.kind === 'terminal') { tab.state.terminal = message.state; scheduleSave(); }
      }
      if (message.type === 'session-states-ready') terminalFlush?.();
      if (message.type === 'session-configured' || message.type === 'session-error') {
        const pending = terminalRequests.get(message.id);
        terminalRequests.delete(message.id);
        if (pending) { clearTimeout(pending.timeout); message.type === 'session-error' ? pending.reject(new Error(message.message)) : pending.resolve(); }
      }
      if (message.type === 'error') { clearTimeout(deadline); reject(new Error(message.message)); }
    });
    terminalProcess.on('error', error => { clearTimeout(deadline); reject(error); });
    terminalProcess.on('exit', code => {
      clearTimeout(deadline);
      terminalReady = undefined;
      terminalAddress = undefined;
      for (const pending of terminalRequests.values()) { clearTimeout(pending.timeout); pending.reject(new Error('Il terminale si è fermato.')); }
      terminalRequests.clear();
      if (!stopping) reject(new Error(`Il terminale si è fermato (${code}).`));
    });
  });
  return terminalReady;
}

async function configureTerminal(tab, project) {
  await beginTerminal();
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { terminalRequests.delete(tab.id); reject(new Error('La sessione terminale non risponde.')); }, 5000);
    terminalRequests.set(tab.id, { resolve, reject, timeout });
    terminalProcess.send({ type: 'configure-session', id: tab.id, cwd: project.directory, state: tab.state?.terminal });
  });
  const url = new URL(terminalAddress);
  url.searchParams.set('element', tab.id);
  return url.href;
}

async function beginCode() {
  if (!codeReady) {
    codeReady = startCodeServer({ workspaceDirectory: path.join(__dirname, 'workspace'), runtimeDirectory: path.join(runtimeDirectory, 'code-server'), signal: lifetime.signal }).then(server => { codeServer = server; return server; }).catch(error => { codeReady = undefined; throw error; });
  }
  return codeReady;
}

async function captureSoftwareStates() {
  if (terminalProcess?.connected && terminalAddress) await new Promise(resolve => {
    const timeout = setTimeout(finish, 1000);
    function finish() { clearTimeout(timeout); terminalFlush = undefined; resolve(); }
    terminalFlush = finish;
    terminalProcess.send({ type: 'get-session-states' });
  });
  for (const tab of tabs.values()) if (tab.kind === 'code') {
    const project = projectStore.getProject(tab.projectId);
    try {
      const editor = JSON.parse(await fs.readFile(path.join(project.directory, '.orbit', 'editor', `${tab.id}.json`), 'utf8'));
      if (editor.projectId === tab.projectId && editor.elementId === tab.id) tab.state.editor = editor;
    } catch (error) { if (error.code !== 'ENOENT') report(error); }
  }
}

function localAddress(pathname, parameters = {}) {
  const url = new URL(pathname, origin);
  for (const [key, value] of Object.entries({ ...parameters, token })) url.searchParams.set(key, value);
  return url.href;
}

function normalizeAddress(value) {
  const address = String(value || '').trim();
  const url = new URL(/^[a-z][a-z\d+.-]*:/i.test(address) ? address : `https://${address}`);
  if (!['http:', 'https:'].includes(url.protocol)) throw new Error('Inserisci un indirizzo HTTP o HTTPS.');
  return url.href;
}

async function resolveTab(options = {}) {
  const requested = options.tab || options;
  const tab = requireTab(requested.id);
  const project = projectStore.getProject(tab.projectId);
  let url;
  if (tab.kind === 'terminal') url = await configureTerminal(tab, project);
  else if (tab.kind === 'code') {
    const server = await beginCode();
    const editorWorkspace = await prepareCodeWorkspace({ projectDirectory: project.directory, projectId: tab.projectId, tabId: tab.id, state: tab.state?.editor });
    url = buildCodeAddress(server.url, editorWorkspace.workspaceFile);
  } else if (tab.kind === 'chat') {
    await conversation(tab);
    url = localAddress('/content/chat.html', { tab: tab.id });
  } else if (tab.kind === 'pdf') url = localAddress(`/resource/${encodeURIComponent(tab.id)}`);
  else if (tab.kind === 'image') {
    const source = tab.resource?.path ? localAddress(`/resource/${encodeURIComponent(tab.id)}`) : normalizeAddress(tab.url);
    url = localAddress('/content/image.html', { tab: tab.id, source, name: tab.title });
  } else if (tab.kind === 'file') url = localAddress('/content/file.html', { tab: tab.id });
  else if (tab.kind === 'draft') url = localAddress('/content/empty.html', { tab: tab.id });
  else url = normalizeAddress(tab.url || (tab.kind === 'figma' ? 'https://www.figma.com' : 'https://www.google.com'));
  return { id: tab.id, projectId: tab.projectId, title: tab.title, kind: tab.kind, url };
}

async function createTab(options = {}) {
  const projectId = options.projectId || workspace.scene.currentProjectId || workspace.projects[0]?.id;
  if (!projectStore.getProject(projectId)) throw new Error('Progetto non disponibile.');
  const kind = options.kind || 'browser';
  if (!['browser', 'figma', 'terminal', 'code', 'chat', 'pdf', 'image', 'file', 'draft'].includes(kind)) throw new Error('Tipo di elemento non valido.');
  const titles = { browser: 'Browser', figma: 'Figma', terminal: 'PowerShell', code: 'VS Code', chat: 'Chat', image: 'Immagine', pdf: 'PDF', file: 'File', draft: 'Elemento' };
  const tab = { id: randomUUID(), projectId, kind, title: String(options.title || titles[kind]).trim().slice(0, 200), url: '', state: {} };
  if (['browser', 'figma'].includes(kind)) tab.url = normalizeAddress(options.url || (kind === 'figma' ? 'https://www.figma.com' : 'https://www.google.com'));
  if (isFigmaAddress(tab.url) && !isTransientAuthenticationAddress(tab.url, tab)) tab.state.restoreUrl = tab.url;
  if (options.filePath) {
    tab.resource = await projectStore.importFile({ projectId, sourcePath: options.filePath });
    tab.kind = tab.resource.kind;
    tab.title = options.title || tab.resource.title;
  } else if (options.resource) {
    const project = projectStore.getProject(projectId);
    const resource = project.references.find(reference => reference.id === options.resource.id);
    if (!resource) throw new Error('Risorsa non disponibile.');
    tab.resource = resource;
  }
  if (['pdf', 'file'].includes(tab.kind) && !tab.resource) throw new Error('Seleziona un file del progetto.');
  if (tab.kind === 'image' && !tab.resource) tab.url = normalizeAddress(options.url);
  tabs.set(tab.id, tab);
  await save();
  publishWorkspace();
  return clone(tab);
}

async function createProject(options = {}) {
  const project = await projectStore.ensureProject({ id: randomUUID(), title: String(options.title || 'Nuovo progetto').trim().slice(0, 100), x: workspace.projects.length * 1200, y: 0 });
  workspace.projects.push(project);
  workspace.scene.currentProjectId = project.id;
  await save();
  publishWorkspace();
  return project;
}

async function closeTab(options = {}) {
  const tab = requireTab(options.id);
  await flushChatDocument(tab);
  if (tab.kind === 'chat') await chat.cancelTurn(tab);
  if (tab.kind === 'terminal' && terminalProcess?.connected) terminalProcess.send({ type: 'close-session', id: tab.id });
  for (const response of subscribers.get(tab.id) || []) response.end();
  subscribers.delete(tab.id);
  tabs.delete(tab.id);
  pageCaptures.delete(tab.id);
  workspace.links = workspace.links.filter(link => link.from !== tab.id && link.to !== tab.id);
  delete workspace.positions[tab.id];
  if (workspace.scene.activeTabId === tab.id) workspace.scene.activeTabId = null;
  if (workspace.scene.expandedId === tab.id) workspace.scene.expandedId = null;
  await save();
  publishWorkspace();
  return { closed: true, id: tab.id };
}

async function openSource(tab, requested = {}) {
  const stored = await conversation(tab);
  const messages = requested.messageId ? stored.messages.filter(message => message.id === requested.messageId) : [...stored.messages].reverse();
  let source = requested.id && messages.flatMap(message => message.sources || []).find(candidate => candidate.id === requested.id);
  if (!source && !requested.id && typeof requested.url === 'string' && /^https?:/.test(requested.url) && messages.some(message => message.role === 'assistant' && message.text.includes(requested.url))) source = { url: normalizeAddress(requested.url), title: new URL(requested.url).hostname };
  if (!source) throw new Error('Questa fonte non appartiene alla conversazione.');
  let target = source.tabId && tabs.get(source.tabId);
  if (!target && source.url) target = await createTab({ projectId: tab.projectId, kind: 'browser', title: source.title, url: source.url });
  if (!target && source.path) {
    const project = projectStore.getProject(tab.projectId);
    const resource = project.references.find(reference => reference.path === source.path);
    if (resource && ['image', 'pdf', 'file'].includes(resource.kind)) target = await createTab({ projectId: tab.projectId, kind: resource.kind, title: resource.title, resource });
    else { await openFileInSystem(await projectFilename(tab.projectId, source.path)); return { opened: true }; }
  }
  if (!target) return { opened: false };
  publish({ type: 'event', event: 'focus', id: target.id, ...(source.page ? { page: source.page } : {}) });
  return { opened: true, id: target.id };
}

async function projectFilename(projectId, filename) {
  const directory = await fs.realpath(projectStore.getProject(projectId).directory);
  const resolved = await fs.realpath(filename);
  const relative = path.relative(directory, resolved);
  if (!relative || relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) throw new Error('Il file non si trova nel progetto.');
  if (!(await fs.stat(resolved)).isFile()) throw new Error('File non disponibile.');
  return resolved;
}

function openFileInSystem(filename) {
  return new Promise((resolve, reject) => {
    const executable = process.platform === 'win32' ? 'explorer.exe' : process.platform === 'darwin' ? 'open' : 'xdg-open';
    const child = spawn(executable, [filename], { windowsHide: true, stdio: 'ignore', detached: true });
    child.once('error', reject);
    child.once('spawn', () => { child.unref(); resolve({ opened: true }); });
  });
}

async function handleChat(tab, method, parameters = {}) {
  if (method === 'conversation') return chat.restore(tab, await conversation(tab, parameters.legacy));
  if (method === 'saveConversation') { await conversation(tab); const saved = await chat.saveFromRenderer(tab, parameters); scheduleSave(); return saved; }
  if (method === 'context') return chatContext(tab);
  if (method === 'runtime') { await conversation(tab); return chat.getRuntime(tab); }
  if (method === 'updateSettings') { await conversation(tab); return chat.updateSettings(tab, parameters); }
  if (method === 'sendMessage') { await conversation(tab); return chat.sendMessage(tab, parameters); }
  if (method === 'cancelTurn') return chat.cancelTurn(tab);
  if (method === 'openSource') return openSource(tab, parameters);
  if (method === 'readyToClose') { closeAcknowledgements.get(tab.id)?.(); return { ready: true }; }
  throw new Error('Operazione chat non valida.');
}

async function handle(method, parameters = {}) {
  if (method === 'getWorkspace') return snapshot();
  if (method === 'resolveTab') return resolveTab(parameters);
  if (method === 'createTab') return createTab(parameters);
  if (method === 'closeTab') return closeTab(parameters);
  if (method === 'project' || method === 'createProject') return createProject(parameters);
  if (method === 'navigate') {
    const tab = requireTab(parameters.id);
    if (!['browser', 'figma'].includes(tab.kind)) throw new Error('Questo elemento non è un browser.');
    tab.url = normalizeAddress(parameters.url);
    if (isFigmaAddress(tab.url) && !isTransientAuthenticationAddress(tab.url, tab)) tab.state.restoreUrl = tab.url;
    pageCaptures.delete(tab.id);
    await save();
    publishWorkspace();
    return resolveTab({ id: tab.id });
  }
  if (method === 'capturePage') {
    const tab = requireTab(parameters.id);
    if (!['browser', 'figma'].includes(tab.kind)) return { captured: false };
    const url = normalizeAddress(parameters.url || tab.url);
    tab.url = url;
    if (isTransientAuthenticationAddress(url, tab)) {
      pageCaptures.delete(tab.id);
      scheduleSave();
      return { captured: false };
    }
    if (isFigmaAddress(url)) tab.state.restoreUrl = url;
    pageCaptures.set(tab.id, { text: String(parameters.text || '').slice(0, 80000), url, capturedAt: new Date().toISOString() });
    if (typeof parameters.title === 'string' && parameters.title.trim()) tab.title = parameters.title.trim().slice(0, 200);
    scheduleSave();
    return { captured: true };
  }
  if (method === 'saveWorkspace') {
    const incoming = parameters.workspace || parameters;
    for (const project of incoming.projects || []) {
      const current = workspace.projects.find(candidate => candidate.id === project.id);
      if (current) Object.assign(current, { title: project.title || current.title, x: project.x ?? current.x, y: project.y ?? current.y });
    }
    for (const incomingTab of incoming.tabs || []) {
      const current = tabs.get(incomingTab.id);
      if (current && incomingTab.state) current.state = { ...current.state, ...clone(incomingTab.state) };
    }
    if (incoming.positions) workspace.positions = clone(incoming.positions);
    if (incoming.links) workspace.links = incoming.links.filter(link => tabs.has(link.from) && tabs.has(link.to) && link.from !== link.to).map(({ from, to }) => ({ from, to }));
    if (incoming.scene) workspace.scene = { ...workspace.scene, ...clone(incoming.scene) };
    const saved = await save();
    publishWorkspace();
    return saved;
  }
  if (method === 'shutdown') { await stop(); return { stopped: true }; }
  throw new Error(`Operazione non disponibile: ${method}`);
}

function authenticated(request, url) {
  const cookies = String(request.headers.cookie || '').split(';').map(value => value.trim());
  const cookie = cookies.find(value => value.startsWith(`${cookieName}=`))?.slice(cookieName.length + 1);
  const candidate = Buffer.from(url.searchParams.get('token') || request.headers.authorization?.replace(/^Bearer /, '') || cookie || '');
  const expected = Buffer.from(token);
  return candidate.length === expected.length && timingSafeEqual(candidate, expected);
}

function json(response, status, contents) {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  response.end(JSON.stringify(contents));
}

async function readBody(request) {
  let bytes = 0;
  const pieces = [];
  for await (const chunk of request) {
    bytes += chunk.length;
    if (bytes > 2 * 1024 * 1024) throw new Error('Richiesta troppo grande.');
    pieces.push(chunk);
  }
  return JSON.parse(Buffer.concat(pieces).toString('utf8') || '{}');
}

async function sendFile(request, response, filename, mime) {
  const content = await fs.readFile(filename);
  const range = /^bytes=(\d*)-(\d*)$/.exec(request.headers.range || '');
  response.setHeader('Accept-Ranges', 'bytes');
  response.setHeader('Content-Type', mime || mimeTypes[path.extname(filename).toLowerCase()] || 'application/octet-stream');
  if (range) {
    const start = range[1] ? Number(range[1]) : Math.max(0, content.length - Number(range[2]));
    const end = range[1] && range[2] ? Math.min(Number(range[2]), content.length - 1) : content.length - 1;
    if (start > end || start >= content.length) { response.writeHead(416, { 'Content-Range': `bytes */${content.length}` }); response.end(); return; }
    response.writeHead(206, { 'Content-Range': `bytes ${start}-${end}/${content.length}`, 'Content-Length': end - start + 1 });
    response.end(request.method === 'HEAD' ? undefined : content.subarray(start, end + 1));
  } else { response.writeHead(200, { 'Content-Length': content.length }); response.end(request.method === 'HEAD' ? undefined : content); }
}

const server = http.createServer((request, response) => {
  void serve(request, response).catch(error => { if (!response.headersSent) json(response, error.code === 'ENOENT' ? 404 : 400, { error: error.message }); else response.end(); });
});

async function serve(request, response) {
  response.setHeader('Cache-Control', 'no-store');
  response.setHeader('X-Content-Type-Options', 'nosniff');
  response.setHeader('Referrer-Policy', 'no-referrer');
  response.setHeader('X-Frame-Options', 'DENY');
  const url = new URL(request.url, origin);
  if (request.headers.host !== new URL(origin).host || !authenticated(request, url)) { json(response, 403, { error: 'Sessione non autorizzata.' }); return; }
  if (request.headers.origin && request.headers.origin !== origin) { json(response, 403, { error: 'Origine non autorizzata.' }); return; }
  if (request.headers['sec-fetch-site'] === 'cross-site') { json(response, 403, { error: 'Richiesta non autorizzata.' }); return; }
  if (url.searchParams.get('token') === token) response.setHeader('Set-Cookie', `${cookieName}=${token}; HttpOnly; SameSite=Strict; Path=/`);

  const api = /^\/api\/tab\/([^/]+)\/([a-zA-Z]+)$/.exec(url.pathname);
  if (api) {
    const tab = requireTab(decodeURIComponent(api[1]));
    const method = api[2];
    if (method === 'events' && request.method === 'GET') {
      if (tab.kind !== 'chat') throw new Error('La chat non è disponibile.');
      response.writeHead(200, { 'Content-Type': 'text/event-stream; charset=utf-8', Connection: 'keep-alive' });
      response.write(': connected\n\n');
      let clients = subscribers.get(tab.id);
      if (!clients) { clients = new Set(); subscribers.set(tab.id, clients); }
      clients.add(response);
      const keepalive = setInterval(() => response.write(': keepalive\n\n'), 20000);
      response.on('close', () => { clearInterval(keepalive); clients.delete(response); if (!clients.size) subscribers.delete(tab.id); });
      return;
    }
    if (request.method !== 'POST' || !String(request.headers['content-type'] || '').startsWith('application/json')) throw new Error('Richiesta non valida.');
    const parameters = await readBody(request);
    if (tab.kind === 'chat') json(response, 200, { result: await handleChat(tab, method, parameters) });
    else if (tab.kind === 'file' && method === 'file') json(response, 200, { result: { name: tab.resource.title, path: tab.resource.path, mime: tab.resource.mimeType, size: tab.resource.size } });
    else if (tab.kind === 'file' && method === 'openFile') json(response, 200, { result: await openFileInSystem(await projectFilename(tab.projectId, tab.resource.path)) });
    else throw new Error('Operazione non autorizzata per questo elemento.');
    return;
  }
  if (!['GET', 'HEAD'].includes(request.method)) { json(response, 405, { error: 'Metodo non disponibile.' }); return; }
  const resource = /^\/resource\/([^/]+)$/.exec(url.pathname);
  if (resource) {
    const tab = requireTab(decodeURIComponent(resource[1]));
    if (!['pdf', 'image', 'file'].includes(tab.kind) || !tab.resource?.path) throw new Error('Risorsa non disponibile.');
    await sendFile(request, response, await projectFilename(tab.projectId, tab.resource.path), tab.resource.mimeType);
    return;
  }
  const relative = url.pathname === '/' ? 'shell.html' : decodeURIComponent(url.pathname.slice(1));
  if (!/^(?:shell\.(?:html|css|js)|content\/[a-zA-Z0-9._-]+|fonts\/[a-zA-Z0-9._-]+)$/.test(relative)) { json(response, 404, { error: 'Pagina non trovata.' }); return; }
  const filename = path.resolve(__dirname, relative);
  response.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' https: http: data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; frame-src 'none'");
  await sendFile(request, response, filename);
}

async function stopTerminal() {
  const child = terminalProcess;
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise(resolve => {
    const deadline = setTimeout(() => { child.kill(); resolve(); }, 2500);
    child.once('exit', () => { clearTimeout(deadline); resolve(); });
    if (child.connected) child.send({ type: 'shutdown' }); else child.kill();
  });
}

function stop() {
  if (stopPromise) return stopPromise;
  stopping = true;
  clearTimeout(saveTimer);
  lifetime.abort();
  stopPromise = (async () => {
    const failures = [];
    const recordFailure = error => { failures.push(error); };
    await Promise.all([...tabs.values()].map(tab => flushChatDocument(tab).catch(recordFailure)));
    await captureSoftwareStates().catch(recordFailure);
    await chat.stop().catch(recordFailure);
    const services = await Promise.allSettled([codeServer?.stop(), codeReady?.then(server => server.stop()), stopTerminal()]);
    for (const service of services) if (service.status === 'rejected' && service.reason?.name !== 'AbortError') recordFailure(service.reason);
    if (workspace) await save().catch(recordFailure);
    await projectStore.flush().catch(recordFailure);
    for (const clients of subscribers.values()) for (const response of clients) response.end();
    subscribers.clear();
    if (server.listening) await new Promise(resolve => { server.close(resolve); server.closeAllConnections(); });
    if (failures.length) throw failures[0];
  })();
  return stopPromise;
}

async function main() {
  if (projectsArgument >= 0 && !process.argv[projectsArgument + 1]) throw new Error('Indica la directory dopo --projects.');
  workspace = await projectStore.load();
  if (!workspace) {
    workspace = { schemaVersion: 1, initialized: true, projects: [], tabs: [], positions: {}, links: [], scene: {} };
    const project = await projectStore.ensureProject({ id: 'project', title: 'ORBIT', x: 0, y: 0 });
    workspace.projects.push(project);
    workspace.scene.currentProjectId = project.id;
    await save();
  }
  for (const tab of workspace.tabs) tabs.set(tab.id, { ...tab, state: tab.state || {} });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  origin = `http://127.0.0.1:${server.address().port}`;
  cookieName = `orbit_${server.address().port}`;
  publish({ type: 'ready', workspace: snapshot(), origin, token });
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  input.on('line', line => {
    let request;
    try { request = JSON.parse(line); } catch { publish({ type: 'response', id: null, error: 'JSON non valido.' }); return; }
    if (!Number.isSafeInteger(request.id) || typeof request.method !== 'string') { publish({ type: 'response', id: request.id ?? null, error: 'Richiesta non valida.' }); return; }
    if (stopping && request.method !== 'shutdown') { publish({ type: 'response', id: request.id, error: 'Il servizio si sta chiudendo.' }); return; }
    Promise.resolve(handle(request.method, request.params)).then(result => {
      publish({ type: 'response', id: request.id, result });
      if (request.method === 'shutdown') { input.close(); process.exit(0); }
    }).catch(error => {
      publish({ type: 'response', id: request.id, error: diagnosticMessage(error) });
      if (request.method === 'shutdown') { input.close(); process.exit(1); }
    });
  });
  input.on('close', () => { if (!stopping) stop().then(() => process.exit(0)).catch(error => { report(error); process.exit(1); }); });
}

function fail(error) {
  const message = diagnosticMessage(error);
  report(message);
  publish({ type: 'error', message });
  void stop().catch(report).finally(() => process.exit(1));
}

process.stdout.on('error', () => { protocolOpen = false; void stop().catch(report).finally(() => process.exit(1)); });
process.on('SIGINT', () => stop().then(() => process.exit(0)).catch(fail));
process.on('SIGTERM', () => stop().then(() => process.exit(0)).catch(fail));
process.on('uncaughtException', fail);
main().catch(fail);
