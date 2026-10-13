const fs = require('node:fs/promises');
const path = require('node:path');
const { createHash, randomUUID } = require('node:crypto');
const { restoreAddress, navigationStateForDisk, restoreTabNavigation } = require('./navigation-addresses');

const mimeTypes = {
  '.avif': 'image/avif', '.gif': 'image/gif', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg',
  '.png': 'image/png', '.svg': 'image/svg+xml', '.webp': 'image/webp', '.bmp': 'image/bmp',
  '.pdf': 'application/pdf', '.json': 'application/json', '.txt': 'text/plain',
  '.md': 'text/markdown', '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript',
  '.csv': 'text/csv', '.mp3': 'audio/mpeg', '.wav': 'audio/wav', '.mp4': 'video/mp4',
  '.webm': 'video/webm', '.zip': 'application/zip'
};

const copy = value => value === undefined ? undefined : JSON.parse(JSON.stringify(value));
const isObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const coordinate = (value, fallback = 0) => Number.isFinite(value) ? value : fallback;
const slash = value => value.split(path.sep).join('/');

function identifier(value, label) {
  if (typeof value !== 'string' || !value.trim() || value.length > 200) throw new Error(`Invalid ${label}`);
  return value;
}

function safeName(value, fallback = 'Project') {
  const name = String(value || '').normalize('NFC').replace(/[<>:"/\\|?*\u0000-\u001f]/g, '-').trim().replace(/[. ]+$/g, '').slice(0, 100).replace(/[. ]+$/g, '') || fallback;
  return /^(con|prn|aux|nul|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)/i.test(name) ? `_${name}` : name;
}

function safeToken(value) {
  const token = safeName(value, 'element').replace(/[^\p{L}\p{N}_-]/gu, '-');
  return token === value ? token : `${token}-${createHash('sha256').update(value).digest('hex').slice(0, 8)}`;
}

function containedPath(directory, relative) {
  const resolved = path.resolve(directory, relative);
  const difference = path.relative(directory, resolved);
  if (!difference || difference === '..' || difference.startsWith(`..${path.sep}`) || path.isAbsolute(difference)) {
    throw new Error(`Path outside project directory: ${resolved}`);
  }
  return resolved;
}

async function readDocument(filename, validate) {
  let content;
  try {
    content = await fs.readFile(filename, 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') return null;
    throw error;
  }
  try {
    const document = JSON.parse(content);
    if (!validate(document)) throw new Error('Unsupported or incomplete schema');
    return document;
  } catch (error) {
    throw new Error(`Cannot read ${filename}: ${error.message}`, { cause: error });
  }
}

async function writeDocument(filename, document, validate) {
  await readDocument(filename, validate);
  const pendingFilename = `${filename}.${randomUUID()}.tmp`;
  let handle;
  try {
    handle = await fs.open(pendingFilename, 'wx');
    await handle.writeFile(`${JSON.stringify(document, null, 2)}\n`, 'utf8');
    await handle.sync();
    await handle.close();
    handle = null;
    await fs.rename(pendingFilename, filename);
  } catch (error) {
    if (handle) await handle.close().catch(() => {});
    await fs.rm(pendingFilename, { force: true }).catch(() => {});
    throw error;
  }
}

function validWorkspaceShape(document) {
  return isObject(document) && document.schemaVersion === 1 && document.initialized === true &&
    Array.isArray(document.projects) && Array.isArray(document.tabs) && isObject(document.positions) &&
    Array.isArray(document.links) && isObject(document.scene);
}

function validRecord(record) {
  return isObject(record) && typeof record.id === 'string' && !!record.id.trim() &&
    typeof record.title === 'string' && typeof record.directory === 'string' &&
    typeof record.description === 'string' && Array.isArray(record.milestones) && Array.isArray(record.references) &&
    typeof record.createdAt === 'string' && Number.isFinite(Date.parse(record.createdAt)) &&
    typeof record.updatedAt === 'string' && Number.isFinite(Date.parse(record.updatedAt)) &&
    Number.isFinite(record.x) && Number.isFinite(record.y);
}

function validWorkspace(document) {
  return validWorkspaceShape(document) && document.projects.every(validRecord);
}

function validProject(document) {
  return validRecord(document) && document.schemaVersion === 1 && Array.isArray(document.links) &&
    isObject(document.session) && Array.isArray(document.session.elements) && isObject(document.session.positions);
}

function validConversation(document) {
  return isObject(document) && document.schemaVersion === 1 && typeof document.id === 'string' &&
    typeof document.projectId === 'string' && Array.isArray(document.messages) && typeof document.draft === 'string' &&
    typeof document.createdAt === 'string' && Number.isFinite(Date.parse(document.createdAt)) &&
    typeof document.updatedAt === 'string' && Number.isFinite(Date.parse(document.updatedAt));
}

function createProjectStore({ directory }) {
  const projectsDirectory = path.resolve(directory);
  const workspaceFilename = path.join(projectsDirectory, 'workspace.info');
  const records = new Map();
  let workspace = null;
  let writes = Promise.resolve();

  function enqueue(operation) {
    const result = writes.then(operation, operation);
    writes = result;
    return result;
  }

  function referenceFromDisk(reference, projectDirectory) {
    if (!isObject(reference) || typeof reference.file !== 'string') throw new Error(`Invalid file reference in ${projectDirectory}`);
    const filename = containedPath(projectDirectory, reference.file);
    return { ...copy(reference), file: slash(path.relative(projectDirectory, filename)), path: filename };
  }

  function referencesForDisk(references) {
    return references.map(reference => {
      const { path: absolutePath, ...stored } = reference;
      return copy(stored);
    });
  }

  function mergeReferences(current, incoming, projectDirectory) {
    const references = new Map();
    for (const reference of [...current, ...(Array.isArray(incoming) ? incoming : [])]) {
      const restored = referenceFromDisk(reference, projectDirectory);
      const key = restored.id || restored.file;
      if (!references.has(key)) references.set(key, restored);
    }
    return [...references.values()];
  }

  function recordForDisk(record, relativeDirectory) {
    return { ...copy(record), directory: relativeDirectory, references: referencesForDisk(record.references) };
  }

  function sanitizeResource(resource, projectDirectory) {
    if (!isObject(resource)) return copy(resource);
    const result = copy(resource);
    if (typeof result.path === 'string' && path.isAbsolute(result.path)) {
      const filename = containedPath(projectDirectory, result.path);
      result.file = slash(path.relative(projectDirectory, filename));
      delete result.path;
    }
    return result;
  }

  function restoreResource(resource, projectDirectory) {
    const result = copy(resource);
    if (isObject(result) && typeof result.file === 'string') result.path = containedPath(projectDirectory, result.file);
    return result;
  }

  function tabForDisk(tab) {
    const record = records.get(tab.projectId)?.record;
    if (!record) throw new Error(`Unknown project ${tab.projectId}`);
    const stored = { id: identifier(tab.id, 'element id'), projectId: tab.projectId, title: String(tab.title || ''), kind: String(tab.kind || 'browser'), url: restoreAddress(tab) };
    if (tab.resource !== undefined) stored.resource = sanitizeResource(tab.resource, record.directory);
    if (tab.state !== undefined) stored.state = navigationStateForDisk(tab);
    return stored;
  }

  function positionForDisk(position) {
    const stored = { x: coordinate(position?.x), y: coordinate(position?.y) };
    if (Number.isFinite(position?.width)) stored.width = position.width;
    if (Number.isFinite(position?.height)) stored.height = position.height;
    return stored;
  }

  function sceneForDisk(scene = {}) {
    return {
      camera: copy(scene.camera || { x: 0, y: 0, scale: 1 }),
      currentProjectId: scene.currentProjectId || null,
      order: copy(scene.order ?? []),
      activeTabId: scene.activeTabId || null,
      expandedId: scene.expandedId || null,
      returnCamera: copy(scene.returnCamera || null),
      orbit: copy(scene.orbit || null)
    };
  }

  function projectDocument(entry) {
    return { schemaVersion: 1, ...recordForDisk(entry.record, '.'), links: copy(entry.links), session: { ...copy(entry.session), elements: entry.session.elements.map(restoreTabNavigation) } };
  }

  async function writeProject(entry) {
    await writeDocument(path.join(entry.record.directory, 'project.info'), projectDocument(entry), validProject);
  }

  async function writeWorkspace() {
    if (!workspace) return;
    await fs.mkdir(projectsDirectory, { recursive: true });
    const stored = {
      ...copy(workspace),
      projects: workspace.projects.map(project => recordForDisk(records.get(project.id).record, slash(path.relative(projectsDirectory, records.get(project.id).record.directory)))),
      tabs: workspace.tabs.map(tabForDisk)
    };
    await writeDocument(workspaceFilename, stored, validWorkspace);
  }

  async function syncRegistry(record) {
    if (!workspace) return;
    const index = workspace.projects.findIndex(project => project.id === record.id);
    if (index < 0) workspace.projects.push(copy(record));
    else workspace.projects[index] = copy(record);
    await writeWorkspace();
  }

  async function allocateDirectory(project) {
    await fs.mkdir(projectsDirectory, { recursive: true });
    const existing = new Set((await fs.readdir(projectsDirectory)).map(name => name.toLowerCase()));
    const title = safeName(project.title);
    const suffix = createHash('sha256').update(project.id).digest('hex').slice(0, 8);
    for (let collision = 0; ; collision += 1) {
      const name = collision === 0 ? title : `${title}-${suffix}${collision > 1 ? `-${collision}` : ''}`;
      if (existing.has(name.toLowerCase())) continue;
      const projectDirectory = containedPath(projectsDirectory, name);
      try {
        await fs.mkdir(projectDirectory);
        return projectDirectory;
      } catch (error) {
        if (error.code !== 'EEXIST') throw error;
        existing.add(name.toLowerCase());
      }
    }
  }

  async function ensureProjectRecord(project, persist = true) {
    identifier(project?.id, 'project id');
    if (project.title !== undefined && typeof project.title !== 'string') throw new Error('Invalid project title');
    let entry = records.get(project.id);
    if (!entry) {
      const projectDirectory = await allocateDirectory(project);
      const createdAt = new Date().toISOString();
      entry = {
        record: { id: project.id, title: project.title?.trim() || 'Project', directory: projectDirectory, createdAt, updatedAt: createdAt, description: '', milestones: [], references: [], x: 0, y: 0 },
        links: [], session: { elements: [], positions: {}, centre: { x: 0, y: 0 } }
      };
      records.set(project.id, entry);
    }
    const record = entry.record;
    if (typeof project.title === 'string' && project.title.trim()) record.title = project.title.trim();
    if (typeof project.description === 'string') record.description = project.description;
    if (Array.isArray(project.milestones)) record.milestones = copy(project.milestones);
    record.references = mergeReferences(record.references, project.references, record.directory);
    record.x = coordinate(project.x, record.x);
    record.y = coordinate(project.y, record.y);
    record.updatedAt = new Date().toISOString();
    entry.session.centre = { x: record.x, y: record.y };
    if (persist) await writeProject(entry);
    return record;
  }

  function requireProject(projectId) {
    const entry = records.get(projectId);
    if (!entry) throw new Error(`Unknown project ${projectId}`);
    return entry;
  }

  function load() {
    return enqueue(async () => {
      const saved = await readDocument(workspaceFilename, validWorkspace);
      if (!saved) return null;
      const restored = new Map();
      let missingProjects = 0;
      for (const project of saved.projects) {
        identifier(project.id, 'project id');
        const projectDirectory = containedPath(projectsDirectory, project.directory);
        try {
          await fs.stat(projectDirectory);
        } catch (error) {
          if (error.code !== 'ENOENT') throw error;
          missingProjects += 1;
          continue;
        }
        const filename = path.join(projectDirectory, 'project.info');
        const document = await readDocument(filename, validProject);
        if (!document || document.id !== project.id) throw new Error(`Missing or mismatched project information: ${filename}`);
        const record = {
          id: document.id, title: document.title, directory: projectDirectory,
          createdAt: document.createdAt, updatedAt: document.updatedAt,
          description: document.description || '', milestones: copy(document.milestones || []),
          references: mergeReferences(document.references, project.references, projectDirectory),
          x: coordinate(document.x), y: coordinate(document.y)
        };
        restored.set(record.id, { record, links: copy(document.links), session: copy(document.session) });
      }
      const tabsById = new Map();
      const restoredPositions = {};
      const restoredLinks = new Map();
      for (const entry of restored.values()) {
        for (const tab of entry.session.elements) {
          if (tab.projectId !== entry.record.id) throw new Error(`Mismatched element project in ${entry.record.directory}`);
          tabsById.set(tab.id, restoreTabNavigation({ ...copy(tab), ...(tab.resource === undefined ? {} : { resource: restoreResource(tab.resource, entry.record.directory) }) }));
          if (entry.session.positions[tab.id]) restoredPositions[tab.id] = positionForDisk(entry.session.positions[tab.id]);
        }
        for (const link of entry.links) restoredLinks.set(`${link.from}\u0000${link.to}`, copy(link));
      }
      const order = [...saved.tabs.map(tab => tab.id), ...tabsById.keys()];
      const seen = new Set();
      const restoredTabs = order.filter(id => tabsById.has(id) && !seen.has(id) && seen.add(id)).map(id => tabsById.get(id));
      const links = [...restoredLinks.values()].filter(link => tabsById.has(link.from) && tabsById.has(link.to));
      const scene = sceneForDisk(saved.scene);
      if (missingProjects) {
        if (!restored.has(scene.currentProjectId)) scene.currentProjectId = null;
        if (!tabsById.has(scene.activeTabId)) scene.activeTabId = null;
        if (!tabsById.has(scene.expandedId)) scene.expandedId = null;
        if (isObject(scene.order)) scene.order = Object.fromEntries(Object.entries(scene.order).filter(([id]) => tabsById.has(id)));
        else if (Array.isArray(scene.order)) scene.order = scene.order.filter(id => tabsById.has(id));
        await fs.copyFile(workspaceFilename, path.join(projectsDirectory, `workspace.recovery-${randomUUID()}.info`), fs.constants.COPYFILE_EXCL);
      }
      records.clear();
      for (const [id, entry] of restored) records.set(id, entry);
      workspace = { schemaVersion: 1, initialized: true, projects: [...records.values()].map(entry => copy(entry.record)), tabs: restoredTabs, positions: restoredPositions, links, scene };
      return copy(workspace);
    });
  }

  function ensureProject(project) {
    return enqueue(async () => {
      const record = await ensureProjectRecord(project);
      await syncRegistry(record);
      return copy(record);
    });
  }

  function save(snapshot) {
    return enqueue(async () => {
      if (!validWorkspaceShape(snapshot)) throw new Error('Invalid workspace schema');
      const projectIds = new Set(snapshot.projects.map(project => project.id));
      const tabIds = new Set(snapshot.tabs.map(tab => tab.id));
      if (projectIds.size !== snapshot.projects.length || tabIds.size !== snapshot.tabs.length) throw new Error('Duplicate project or element id');
      if (snapshot.tabs.some(tab => !projectIds.has(tab.projectId))) throw new Error('Element references a project outside the workspace');
      const savedProjects = [];
      for (const project of snapshot.projects) savedProjects.push(copy(await ensureProjectRecord(project, false)));
      const tabs = snapshot.tabs.map(tabForDisk);
      const elementIds = new Set(tabs.map(tab => tab.id));
      const positions = Object.fromEntries(Object.entries(snapshot.positions).filter(([id]) => elementIds.has(id)).map(([id, position]) => [id, positionForDisk(position)]));
      const links = snapshot.links.filter(link => isObject(link) && elementIds.has(link.from) && elementIds.has(link.to)).map(link => ({ from: link.from, to: link.to }));
      for (const project of savedProjects) {
        const entry = records.get(project.id);
        const elements = tabs.filter(tab => tab.projectId === project.id);
        const ids = new Set(elements.map(tab => tab.id));
        entry.links = links.filter(link => ids.has(link.from) || ids.has(link.to));
        entry.session = { elements, positions: Object.fromEntries(Object.entries(positions).filter(([id]) => ids.has(id))), centre: { x: project.x, y: project.y } };
        await writeProject(entry);
      }
      const runtimeTabs = tabs.map(tab => ({ ...copy(tab), ...(tab.resource === undefined ? {} : { resource: restoreResource(tab.resource, records.get(tab.projectId).record.directory) }) }));
      workspace = { schemaVersion: 1, initialized: true, projects: savedProjects, tabs: runtimeTabs, positions, links, scene: sceneForDisk(snapshot.scene) };
      await writeWorkspace();
      return copy(workspace);
    });
  }

  function importFile({ projectId, sourcePath }) {
    return enqueue(async () => {
      const entry = requireProject(projectId);
      const source = path.resolve(sourcePath);
      const status = await fs.stat(source);
      if (!status.isFile()) throw new Error(`Not a file: ${source}`);
      const filesDirectory = path.join(entry.record.directory, 'files');
      await fs.mkdir(filesDirectory, { recursive: true });
      const sourceName = path.parse(source);
      const original = `${safeName(sourceName.name, 'file')}${safeName(sourceName.ext, '')}`;
      const extension = path.extname(original);
      const stem = original.slice(0, original.length - extension.length);
      const existing = new Set((await fs.readdir(filesDirectory)).map(name => name.toLowerCase()));
      let filename;
      for (let collision = 0; ; collision += 1) {
        const name = collision === 0 ? original : `${stem}-${collision + 1}${extension}`;
        if (existing.has(name.toLowerCase())) continue;
        const destination = containedPath(filesDirectory, name);
        try {
          await fs.copyFile(source, destination, fs.constants.COPYFILE_EXCL);
          filename = destination;
          break;
        } catch (error) {
          if (error.code !== 'EEXIST') throw error;
          existing.add(name.toLowerCase());
        }
      }
      const mimeType = mimeTypes[extension.toLowerCase()] || 'application/octet-stream';
      const reference = {
        id: randomUUID(), kind: mimeType.startsWith('image/') ? 'image' : mimeType === 'application/pdf' ? 'pdf' : 'file',
        title: path.basename(filename), file: slash(path.relative(entry.record.directory, filename)), path: filename,
        mimeType, size: (await fs.stat(filename)).size, createdAt: new Date().toISOString()
      };
      entry.record.references.push(reference);
      entry.record.updatedAt = reference.createdAt;
      await writeProject(entry);
      await syncRegistry(entry.record);
      return copy(reference);
    });
  }

  function readChat({ projectId, tabId }) {
    return enqueue(async () => {
      const entry = requireProject(projectId);
      identifier(tabId, 'chat id');
      const filename = path.join(entry.record.directory, `chat-${safeToken(tabId)}.chat`);
      const conversation = await readDocument(filename, validConversation);
      if (conversation && (conversation.id !== tabId || conversation.projectId !== projectId)) throw new Error(`Mismatched chat information: ${filename}`);
      return copy(conversation);
    });
  }

  function writeChat({ projectId, tabId, conversation = {}, title }) {
    return enqueue(async () => {
      const entry = requireProject(projectId);
      identifier(tabId, 'chat id');
      const file = `chat-${safeToken(tabId)}.chat`;
      const filename = path.join(entry.record.directory, file);
      const previous = await readDocument(filename, validConversation);
      if (previous && (previous.id !== tabId || previous.projectId !== projectId)) throw new Error(`Mismatched chat information: ${filename}`);
      if (!isObject(conversation) || (conversation.messages !== undefined && !Array.isArray(conversation.messages))) throw new Error('Invalid chat conversation');
      const updatedAt = new Date().toISOString();
      const provider = conversation.provider ?? previous?.provider ?? 'codex';
      const model = conversation.model ?? previous?.model ?? '';
      if (!['codex', 'claude'].includes(provider)) throw new Error('Invalid chat provider');
      if (typeof model !== 'string' || model.length > 160 || /[\u0000-\u001f]/.test(model)) throw new Error('Invalid chat model');
      const sessions = copy(previous?.sessions || {});
      if (conversation.sessions !== undefined) {
        if (!isObject(conversation.sessions)) throw new Error('Invalid chat sessions');
        for (const engine of ['codex', 'claude']) {
          if (!Object.hasOwn(conversation.sessions, engine)) continue;
          const session = conversation.sessions[engine];
          if (session === null) {
            delete sessions[engine];
            continue;
          }
          if (!isObject(session)) throw new Error('Invalid chat session');
          for (const [field, limit] of [['sessionId', 200], ['model', 160], ['createdAt', 80], ['updatedAt', 80]]) {
            if (session[field] !== undefined && (typeof session[field] !== 'string' || session[field].length > limit || /[\u0000-\u001f]/.test(session[field]))) throw new Error(`Invalid chat session ${field}`);
          }
          sessions[engine] = { ...sessions[engine], ...copy(session) };
        }
      }
      const turn = Object.hasOwn(conversation, 'turn') ? conversation.turn : previous?.turn ?? null;
      if (turn !== null) {
        if (!isObject(turn) || typeof turn.runId !== 'string' || turn.runId.length > 200 || !['starting', 'running', 'completed', 'error', 'cancelled', 'interrupted'].includes(turn.status)) throw new Error('Invalid chat turn');
        if (JSON.stringify(turn).length > 2000000) throw new Error('Chat turn is too large');
      }
      const stored = {
        schemaVersion: 1, id: tabId, projectId,
        messages: copy(conversation.messages ?? previous?.messages ?? []),
        draft: String(conversation.draft ?? previous?.draft ?? '').slice(0, 20000),
        provider, model, sessions, turn: copy(turn),
        createdAt: previous?.createdAt || updatedAt, updatedAt
      };
      await writeDocument(filename, stored, validConversation);
      let reference = entry.record.references.find(reference => reference.kind === 'chat' && reference.file === file);
      if (!reference) {
        reference = { id: randomUUID(), kind: 'chat', title: title?.trim() || 'Chat', file, path: filename, mimeType: 'application/json', createdAt: stored.createdAt, updatedAt };
        entry.record.references.push(reference);
      } else {
        if (typeof title === 'string' && title.trim()) reference.title = title.trim();
        reference.updatedAt = updatedAt;
      }
      entry.record.updatedAt = updatedAt;
      await writeProject(entry);
      await syncRegistry(entry.record);
      return { conversation: copy(stored), reference: copy(reference), fileRef: copy(reference) };
    });
  }

  return { load, ensureProject, save, importFile, readChat, writeChat, getProject: id => copy(records.get(id)?.record || null), flush: async () => { await writes; } };
}

module.exports = { createProjectStore };
