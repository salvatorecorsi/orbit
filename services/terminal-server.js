const http = require('node:http');
const path = require('node:path');
const fs = require('node:fs');
const crypto = require('node:crypto');
const pty = require('node-pty');
const { WebSocket, WebSocketServer } = require('ws');

const accessToken = crypto.randomBytes(32).toString('hex');
const terminalDirectory = path.join(__dirname, 'terminal');
const workspaceDirectory = path.join(__dirname, 'workspace');
const xtermDirectory = path.dirname(require.resolve('@xterm/xterm'));
const requestedPort = Number(process.env.ORBIT_TERMINAL_PORT || 0);
const routes = new Map([
  ['/terminal/', { file: path.join(terminalDirectory, 'index.html'), mime: 'text/html; charset=utf-8', authenticated: true }],
  ['/terminal/terminal.js', { file: path.join(terminalDirectory, 'terminal.js'), mime: 'text/javascript; charset=utf-8' }],
  ['/terminal/styles.css', { file: path.join(terminalDirectory, 'styles.css'), mime: 'text/css; charset=utf-8' }],
  ['/vendor/xterm.js', { file: require.resolve('@xterm/xterm'), mime: 'text/javascript; charset=utf-8' }],
  ['/vendor/xterm.css', { file: path.join(xtermDirectory, '../css/xterm.css'), mime: 'text/css; charset=utf-8' }],
  ['/vendor/addon-fit.js', { file: require.resolve('@xterm/addon-fit'), mime: 'text/javascript; charset=utf-8' }]
]);
const sockets = new WebSocketServer({ noServer: true, maxPayload: 65536, perMessageDeflate: false });
const connections = new Set();
const sessionConfigurations = new Map();
const transcriptLimit = 204800;
let origin;
let shuttingDown = false;

if (!Number.isInteger(requestedPort) || requestedPort < 0 || requestedPort > 65535) {
  throw new Error('ORBIT_TERMINAL_PORT deve essere una porta valida.');
}

fs.mkdirSync(workspaceDirectory, { recursive: true });
if (!process.send) configureSession({ id: 'standalone', cwd: workspaceDirectory });

function boundedTranscript(value) {
  const contents = Buffer.from(typeof value === 'string' ? value : '');
  if (contents.length <= transcriptLimit) return contents.toString();
  let offset = contents.length - transcriptLimit;
  while ((contents[offset] & 192) === 128) offset++;
  const tail = contents.subarray(offset).toString('utf8');
  const lineEnd = tail.indexOf('\n');
  return lineEnd >= 0 && lineEnd < 4096 ? tail.slice(lineEnd + 1) : tail;
}

function terminalState(session) {
  return {
    version: 1,
    cwd: session.cwd,
    shell: session.shell,
    transcript: session.transcript,
    scrollTop: session.scrollTop,
    capturedAt: Date.now()
  };
}

function publishTerminalState(session) {
  clearTimeout(session.stateTimer);
  session.stateTimer = undefined;
  if (process.connected && process.send) process.send({ type: 'terminal-state', id: session.id, state: terminalState(session) }, () => {});
}

function scheduleTerminalState(session) {
  if (session.stateTimer) return;
  session.stateTimer = setTimeout(() => publishTerminalState(session), 500);
}

function configureSession(message) {
  if (typeof message.id !== 'string' || !/^[a-z\d._:-]{1,128}$/i.test(message.id) || typeof message.cwd !== 'string' || !path.isAbsolute(message.cwd)) throw new Error('Configurazione terminale non valida.');
  const cwd = fs.realpathSync(message.cwd);
  if (!fs.statSync(cwd).isDirectory()) throw new Error('La cartella del progetto non è disponibile.');
  const existing = sessionConfigurations.get(message.id);
  if (existing?.cwd === cwd) return existing;
  existing?.closeShell?.();
  const session = {
    id: message.id,
    cwd,
    shell: '',
    transcript: boundedTranscript(message.state?.transcript),
    scrollTop: Number.isInteger(message.state?.scrollTop) ? Math.max(0, Math.min(5000, message.state.scrollTop)) : 0,
    stateTimer: undefined,
    connection: undefined,
    closeShell: undefined
  };
  sessionConfigurations.set(message.id, session);
  return session;
}

function hasAccess(requestUrl) {
  const token = requestUrl.searchParams.get('token') || '';
  const candidate = Buffer.from(token);
  const expected = Buffer.from(accessToken);
  return candidate.length === expected.length && crypto.timingSafeEqual(candidate, expected);
}

function setResponseHeaders(response) {
  response.setHeader('Cache-Control', 'no-store');
  response.setHeader('X-Content-Type-Options', 'nosniff');
  response.setHeader('Referrer-Policy', 'no-referrer');
  response.setHeader('X-Frame-Options', 'DENY');
  response.setHeader('Content-Security-Policy', `default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ${origin.replace('http:', 'ws:')}; font-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`);
}

function rejectRequest(response, status, message) {
  response.writeHead(status, { 'Content-Type': 'text/plain; charset=utf-8' });
  response.end(message);
}

const server = http.createServer((request, response) => {
  setResponseHeaders(response);
  if (request.headers.host !== new URL(origin).host || !['GET', 'HEAD'].includes(request.method)) {
    rejectRequest(response, 403, 'Richiesta non consentita.');
    return;
  }

  const requestUrl = new URL(request.url, origin);
  const route = routes.get(requestUrl.pathname);
  if (!route) {
    rejectRequest(response, 404, 'Pagina non trovata.');
    return;
  }
  if (route.authenticated && !hasAccess(requestUrl)) {
    rejectRequest(response, 403, 'Sessione terminale non autorizzata.');
    return;
  }

  fs.readFile(route.file, (error, contents) => {
    if (error) {
      rejectRequest(response, 500, 'Risorsa non disponibile.');
      return;
    }
    response.writeHead(200, { 'Content-Type': route.mime });
    response.end(request.method === 'HEAD' ? undefined : contents);
  });
});

server.on('upgrade', (request, socket, head) => {
  const requestUrl = new URL(request.url, origin);
  const id = requestUrl.searchParams.get('element') || 'standalone';
  const session = sessionConfigurations.get(id);
  if (shuttingDown || !session || request.headers.host !== new URL(origin).host || request.headers.origin !== origin || requestUrl.pathname !== '/terminal/socket' || !hasAccess(requestUrl)) {
    socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');
    return;
  }
  sockets.handleUpgrade(request, socket, head, connection => sockets.emit('connection', connection, session));
});

function openShell(connection, session) {
  if (session.connection?.readyState === WebSocket.OPEN) session.connection.close(1000, 'Sessione riaperta.');
  session.closeShell?.();
  const shell = process.platform === 'win32'
    ? path.join(process.env.SystemRoot || 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe')
    : process.env.SHELL || '/bin/sh';
  const argumentsList = process.platform === 'win32' ? ['-NoLogo', '-NoProfile'] : [];
  const shellEnvironment = { ...process.env, TERM: 'xterm-256color', COLORTERM: 'truecolor' };
  delete shellEnvironment.ORBIT_TERMINAL_PORT;
  delete shellEnvironment.ELECTRON_RUN_AS_NODE;

  const terminalProcess = pty.spawn(shell, argumentsList, {
    name: 'xterm-256color',
    cols: 100,
    rows: 30,
    cwd: session.cwd,
    env: shellEnvironment,
    windowsHide: true,
    useConptyDll: process.platform === 'win32'
  });
  let ended = false;
  let paused = false;
  let pendingOutput = '';
  session.shell = path.basename(shell);

  function sendPacket(packet) {
    if (connection.readyState === WebSocket.OPEN) connection.send(JSON.stringify(packet));
  }

  function flushOutput() {
    if (connection.bufferedAmount > 8388608 || pendingOutput.length > 2097152) {
      connection.close(1013, 'Il terminale ha prodotto troppo output.');
      closeShell();
      return;
    }
    if (connection.bufferedAmount > 1048576) {
      if (!paused) {
        terminalProcess.pause();
        paused = true;
      }
      return;
    }
    if (paused && connection.bufferedAmount < 131072) {
      terminalProcess.resume();
      paused = false;
    }
    if (!pendingOutput) return;
    sendPacket({ type: 'output', text: pendingOutput });
    pendingOutput = '';
  }

  const flushInterval = setInterval(flushOutput, 12);
  const outputSubscription = terminalProcess.onData(text => {
    pendingOutput += text;
    session.transcript = boundedTranscript(session.transcript + text);
    scheduleTerminalState(session);
    if (pendingOutput.length > 65536) flushOutput();
  });
  const exitSubscription = terminalProcess.onExit(({ exitCode }) => {
    if (ended) return;
    flushOutput();
    sendPacket({ type: 'exit', exitCode });
    ended = true;
    clearInterval(flushInterval);
    connections.delete(closeShell);
    session.closeShell = undefined;
    session.connection = undefined;
    publishTerminalState(session);
    connection.close(1000, 'Shell terminata.');
  });

  function closeShell() {
    if (ended) return;
    ended = true;
    clearInterval(flushInterval);
    outputSubscription.dispose();
    exitSubscription.dispose();
    connections.delete(closeShell);
    if (session.closeShell === closeShell) {
      session.closeShell = undefined;
      session.connection = undefined;
    }
    publishTerminalState(session);
    try {
      terminalProcess.kill();
    } catch (error) {
      if (error.code !== 'ESRCH') process.stderr.write(`Chiusura terminale: ${error.message}\n`);
    }
  }

  connection.on('message', (contents, isBinary) => {
    if (isBinary || ended) {
      connection.close(1003, 'Messaggio non supportato.');
      return;
    }
    let packet;
    try {
      packet = JSON.parse(contents.toString());
    } catch {
      connection.close(1007, 'Messaggio non valido.');
      return;
    }
    if (packet?.type === 'input' && typeof packet.text === 'string' && packet.text.length <= 16384) {
      terminalProcess.write(packet.text);
      return;
    }
    if (packet?.type === 'resize' && Number.isInteger(packet.columns) && Number.isInteger(packet.rows) && packet.columns >= 2 && packet.columns <= 400 && packet.rows >= 1 && packet.rows <= 200) {
      terminalProcess.resize(packet.columns, packet.rows);
      return;
    }
    if (packet?.type === 'viewport' && Number.isInteger(packet.scrollTop) && packet.scrollTop >= 0 && packet.scrollTop <= 5000) {
      session.scrollTop = packet.scrollTop;
      scheduleTerminalState(session);
      return;
    }
    connection.close(1008, 'Messaggio non consentito.');
  });
  connection.on('close', closeShell);
  connection.on('error', closeShell);
  connections.add(closeShell);
  session.closeShell = closeShell;
  session.connection = connection;
  sendPacket({ type: 'restore', state: terminalState(session) });
  sendPacket({ type: 'ready', shell: path.basename(shell), workspace: session.cwd });
  publishTerminalState(session);
}

sockets.on('connection', (connection, session) => {
  try {
    openShell(connection, session);
  } catch (error) {
    connection.send(JSON.stringify({ type: 'error', message: `Impossibile avviare la shell: ${error.message}` }));
    connection.close(1011, 'Avvio shell fallito.');
  }
});

function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  for (const closeShell of connections) closeShell();
  for (const session of sessionConfigurations.values()) publishTerminalState(session);
  for (const connection of sockets.clients) connection.terminate();
  sockets.close();
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 1500).unref();
}

process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
process.on('disconnect', shutdown);
process.on('message', message => {
  if (message?.type === 'shutdown') shutdown();
  if (message?.type === 'configure-session') {
    try {
      const session = configureSession(message);
      if (process.connected && process.send) process.send({ type: 'session-configured', id: session.id }, () => {});
    } catch (error) {
      if (process.connected && process.send) process.send({ type: 'session-error', id: message.id, message: error.message }, () => {});
    }
  }
  if (message?.type === 'get-session-states') {
    for (const session of sessionConfigurations.values()) publishTerminalState(session);
    if (process.connected && process.send) process.send({ type: 'session-states-ready', count: sessionConfigurations.size }, () => {});
  }
  if (message?.type === 'close-session') {
    const session = sessionConfigurations.get(message.id);
    if (session) {
      session.closeShell?.();
      clearTimeout(session.stateTimer);
      sessionConfigurations.delete(message.id);
    }
  }
});
server.on('error', error => {
  const message = `Server terminale: ${error.message}`;
  if (process.send) process.send({ type: 'error', message });
  else process.stderr.write(`${message}\n`);
  shutdown();
});
server.listen(requestedPort, '127.0.0.1', () => {
  origin = `http://127.0.0.1:${server.address().port}`;
  const url = `${origin}/terminal/?token=${accessToken}`;
  if (process.send) process.send({ type: 'ready', url, origin });
  else process.stdout.write(`${url}\n`);
});
