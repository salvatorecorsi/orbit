const { spawn } = require('node:child_process');
const { randomBytes } = require('node:crypto');
const fs = require('node:fs/promises');
const http = require('node:http');
const net = require('node:net');
const path = require('node:path');
const { setTimeout: delay } = require('node:timers/promises');
const { pathToFileURL } = require('node:url');

function elementFileName(tabId) {
  if (typeof tabId !== 'string' || !/^[a-zA-Z0-9][a-zA-Z0-9_-]{0,119}$/.test(tabId)) throw new Error('Identificatore editor non valido.');
  return tabId;
}

async function writeJsonFile(filePath, contents) {
  const stagedPath = `${filePath}.${randomBytes(6).toString('hex')}.pending`;
  try {
    await fs.writeFile(stagedPath, `${JSON.stringify(contents, null, 2)}\n`, { mode: 0o600 });
    await fs.rename(stagedPath, filePath);
  } finally {
    await fs.rm(stagedPath, { force: true });
  }
}

function editorCaptureTime(state) {
  const capturedAt = typeof state?.capturedAt === 'number' ? state.capturedAt : Date.parse(state?.capturedAt || '');
  return Number.isFinite(capturedAt) ? capturedAt : 0;
}

async function readCodeFile(filePath, description) {
  try {
    const contents = JSON.parse(await fs.readFile(filePath, 'utf8'));
    if (!contents || typeof contents !== 'object' || Array.isArray(contents)) throw new Error('Il contenuto deve essere un oggetto JSON.');
    return contents;
  } catch (error) {
    if (error.code === 'ENOENT') return;
    throw new Error(`${description} non disponibile: ${filePath}. Il file esistente viene conservato. ${error.message}`, { cause: error });
  }
}

async function prepareCodeWorkspace({ projectDirectory, projectId, tabId, state } = {}) {
  const projectPath = path.resolve(projectDirectory);
  const elementId = elementFileName(tabId);
  if (typeof projectId !== 'string' || !projectId) throw new Error('Identificatore progetto non valido.');
  const workspaceFile = path.join(projectPath, '.orbit', 'workspaces', `${elementId}.code-workspace`);
  const stateFile = path.join(projectPath, '.orbit', 'editor', `${elementId}.json`);
  const [savedWorkspace, savedState] = await Promise.all([
    readCodeFile(workspaceFile, 'Workspace VS Code'),
    readCodeFile(stateFile, 'Stato editor')
  ]);
  if (savedState && (savedState.projectId !== projectId || savedState.elementId !== elementId)) throw new Error(`Stato editor di un altro elemento: ${stateFile}. Il file esistente viene conservato.`);
  await Promise.all([
    fs.mkdir(path.dirname(workspaceFile), { recursive: true }),
    fs.mkdir(path.dirname(stateFile), { recursive: true })
  ]);
  const folders = Array.isArray(savedWorkspace?.folders) ? savedWorkspace.folders : [];
  const primaryFolder = folders.find(folder => typeof folder?.path === 'string' && path.resolve(path.dirname(workspaceFile), folder.path).toLowerCase() === projectPath.toLowerCase());
  await writeJsonFile(workspaceFile, {
    ...savedWorkspace,
    folders: [{ name: path.basename(projectPath), ...primaryFolder, path: '../..' }, ...folders.filter(folder => folder !== primaryFolder)],
    settings: {
      ...savedWorkspace?.settings,
      'orbit.projectId': projectId,
      'orbit.elementId': elementId,
      'orbit.stateFile': `../editor/${elementId}.json`
    }
  });
  if (state && typeof state === 'object' && !Array.isArray(state)) {
    const newerSavedState = savedState && Array.isArray(savedState.groups) && editorCaptureTime(savedState) > editorCaptureTime(state);
    if (!newerSavedState) await writeJsonFile(stateFile, { ...state, projectId, elementId });
  }
  return { workspaceFile, stateFile };
}

function buildCodeAddress(serverUrl, workspaceFile) {
  const address = new URL(serverUrl);
  address.searchParams.delete('folder');
  address.searchParams.set('workspace', decodeURIComponent(pathToFileURL(path.resolve(workspaceFile)).pathname));
  return address.href;
}

async function installSessionExtension(runtimeDirectory) {
  const extensionsDirectory = path.join(runtimeDirectory, 'server', 'extensions');
  const extensionDirectory = path.join(extensionsDirectory, 'ORBIT');
  await fs.mkdir(extensionDirectory, { recursive: true });
  await Promise.all(['package.json', 'main.js'].map(fileName => fs.copyFile(
    path.join(__dirname, 'code-session-extension', fileName),
    path.join(extensionDirectory, fileName)
  )));
  return extensionsDirectory;
}

async function findCodeExecutable() {
  const installDirectories = [
    process.env.LOCALAPPDATA && path.join(process.env.LOCALAPPDATA, 'Programs', 'Microsoft VS Code'),
    process.env.ProgramFiles && path.join(process.env.ProgramFiles, 'Microsoft VS Code'),
    process.env['ProgramFiles(x86)'] && path.join(process.env['ProgramFiles(x86)'], 'Microsoft VS Code'),
    ...(process.env.PATH || '').split(path.delimiter).filter(Boolean).map(directory => path.resolve(directory, '..'))
  ].filter(Boolean);

  for (const installDirectory of new Set(installDirectories)) {
    const executable = path.join(installDirectory, 'bin', 'code-tunnel.exe');
    try {
      const executableStats = await fs.stat(executable);
      if (executableStats.isFile()) return executable;
    } catch {}
  }

  throw new Error('VS Code locale non trovato: serve il comando code-tunnel.exe della sua installazione.');
}

function reserveAvailablePort() {
  return new Promise((resolve, reject) => {
    const listener = net.createServer();
    listener.once('error', reject);
    listener.listen(0, '127.0.0.1', () => {
      const port = listener.address().port;
      listener.close(error => error ? reject(error) : resolve(port));
    });
  });
}

function requestEditor(url, signal, cookie = '', redirectCount = 0) {
  return new Promise(resolve => {
    const request = http.get(url, { signal, headers: { Accept: 'text/html', Cookie: cookie } }, response => {
      if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location && redirectCount < 3) {
        const redirectUrl = new URL(response.headers.location, url);
        const redirectedCookie = (response.headers['set-cookie'] || []).map(value => value.split(';')[0]).join('; ') || cookie;
        response.resume();
        if (redirectUrl.origin !== new URL(url).origin) return resolve(false);
        return resolve(requestEditor(redirectUrl, signal, redirectedCookie, redirectCount + 1));
      }
      let document = '';
      response.setEncoding('utf8');
      response.on('data', chunk => {
        if (document.length < 500000) document += chunk;
      });
      response.once('end', () => resolve(
        response.statusCode === 200 && /(?:workbench|vscode-workbench-web-configuration)/i.test(document)
      ));
      response.once('error', () => resolve(false));
    });
    request.setTimeout(10000, () => request.destroy());
    request.once('error', () => resolve(false));
  });
}

function waitForExit(child, duration = 2500) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise(resolve => {
    const timer = setTimeout(() => {
      child.removeListener('exit', finished);
      resolve();
    }, duration);
    function finished() {
      clearTimeout(timer);
      resolve();
    }
    child.once('exit', finished);
  });
}

async function stopOwnedProcess(child) {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;

  if (process.platform === 'win32') {
    await new Promise(resolve => {
      const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], {
        windowsHide: true,
        stdio: 'ignore'
      });
      killer.once('error', resolve);
      killer.once('exit', resolve);
    });
    await waitForExit(child);
    return;
  }

  child.kill('SIGTERM');
  await waitForExit(child);
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
}

function abortedStartup() {
  const error = new Error('Avvio del server VS Code annullato.');
  error.name = 'AbortError';
  return error;
}

async function startCodeServer({ workspaceDirectory, runtimeDirectory, signal }) {
  if (signal?.aborted) throw abortedStartup();
  if (process.platform !== 'win32') {
    throw new Error('Questa demo cerca VS Code locale su Windows.');
  }

  const workspacePath = path.resolve(workspaceDirectory);
  const runtimePath = path.resolve(runtimeDirectory);
  const executable = await findCodeExecutable();
  const port = await reserveAvailablePort();
  const token = randomBytes(32).toString('hex').toUpperCase();
  const tokenPath = path.join(runtimePath, 'connection-token');
  await fs.mkdir(workspacePath, { recursive: true });
  await fs.mkdir(runtimePath, { recursive: true });
  const extensionsDirectory = await installSessionExtension(runtimePath);
  if (signal?.aborted) throw abortedStartup();
  await fs.writeFile(tokenPath, token, { mode: 0o600 });
  if (signal?.aborted) {
    await fs.rm(tokenPath, { force: true });
    throw abortedStartup();
  }

  const serverArguments = [
    '--cli-data-dir', path.join(runtimePath, 'cli'),
    'serve-web',
    '--host', '127.0.0.1',
    '--port', String(port),
    '--connection-token-file', tokenPath,
    '--server-data-dir', path.join(runtimePath, 'server'),
    '--default-folder', workspacePath,
    '--disable-telemetry',
    '--accept-server-license-terms'
  ];
  const child = spawn(executable, serverArguments, {
    cwd: workspacePath,
    env: { ...process.env, VSCODE_EXTENSIONS: extensionsDirectory },
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe']
  });
  const url = `http://127.0.0.1:${port}/?tkn=${token}`;
  let processFailure = null;
  let recentOutput = '';
  let announcedDownload = false;
  let stopping = false;
  let stopPromise;

  function recordOutput(chunk) {
    const output = chunk.toString().split(token).join('[token]');
    recentOutput = (recentOutput + output).slice(-5000);
    if (!announcedDownload && /download|install|extract/i.test(output)) {
      announcedDownload = true;
      process.stderr.write('VS Code: preparazione del server web locale al primo avvio.\n');
    }
  }
  child.stdout.on('data', recordOutput);
  child.stderr.on('data', recordOutput);
  child.once('error', error => { processFailure = error; });
  child.once('exit', (code, exitSignal) => {
    if (!stopping) processFailure = new Error(`VS Code serve-web terminato (${exitSignal || code}).`);
  });

  function stop() {
    if (!stopPromise) {
      stopping = true;
      signal?.removeEventListener('abort', onAbort);
      stopPromise = stopOwnedProcess(child).finally(() => fs.rm(tokenPath, { force: true }));
    }
    return stopPromise;
  }

  function onAbort() {
    void stop().catch(() => {});
  }
  signal?.addEventListener('abort', onAbort, { once: true });
  if (signal?.aborted) onAbort();

  try {
    const deadline = Date.now() + 120000;
    while (Date.now() < deadline) {
      if (signal?.aborted) throw abortedStartup();
      if (processFailure) throw processFailure;
      if (await requestEditor(url, signal)) {
        if (signal?.aborted) throw abortedStartup();
        return { url, stop };
      }
      await delay(500, undefined, { signal });
    }
    throw new Error('Il server web VS Code non è diventato disponibile entro due minuti.');
  } catch (error) {
    await stop();
    if (signal?.aborted || error.name === 'AbortError') throw abortedStartup();
    const lastOutput = recentOutput.trim().split(/\r?\n/).slice(-6).join(' ').slice(0, 1000);
    throw new Error(`${error.message}${lastOutput ? ` ${lastOutput}` : ''}`);
  }
}

module.exports = { startCodeServer, prepareCodeWorkspace, buildCodeAddress };
