const terminalElement = document.querySelector('#terminal');
const statusElement = document.querySelector('#status');
const accessToken = new URL(location.href).searchParams.get('token');
const elementId = new URL(location.href).searchParams.get('element');
const terminal = new Terminal({
  cursorBlink: true,
  cursorStyle: 'bar',
  fontFamily: 'Cascadia Code, Consolas, monospace',
  fontSize: 14,
  lineHeight: 1.18,
  scrollback: 5000,
  allowProposedApi: false,
  theme: {
    background: '#0d111b',
    foreground: '#dbe6f4',
    cursor: '#a3c4ed',
    selectionBackground: '#324a69',
    black: '#141d2e',
    red: '#ef9b9b',
    green: '#9dc7af',
    yellow: '#e4c693',
    blue: '#9fbae7',
    magenta: '#c7a5e5',
    cyan: '#98d0d5',
    white: '#dbe6f4',
    brightBlack: '#78869e'
  }
});
const fitAddon = new FitAddon.FitAddon();
terminal.loadAddon(fitAddon);
terminal.open(terminalElement);
const socketUrl = new URL('/terminal/socket', location.href);
socketUrl.protocol = 'ws:';
socketUrl.searchParams.set('token', accessToken || '');
if (elementId) socketUrl.searchParams.set('element', elementId);
const socket = new WebSocket(socketUrl);
let exited = false;
let fitFrame;
let terminalDimensions;
let shellReady = false;
let restoringTranscript = false;
let viewportTimer;
terminal.options.disableStdin = true;

function sendPacket(packet) {
  if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(packet));
}

function showStatus(message, failed = false) {
  statusElement.textContent = message;
  statusElement.classList.toggle('failed', failed);
  statusElement.hidden = false;
}

function fitTerminal() {
  cancelAnimationFrame(fitFrame);
  fitFrame = requestAnimationFrame(() => {
    const proposed = fitAddon.proposeDimensions();
    if (!proposed || proposed.cols < 2 || proposed.rows < 1) return;
    const columns = Math.min(proposed.cols, 400);
    const rows = Math.min(proposed.rows, 200);
    const dimensions = `${columns}:${rows}`;
    if (dimensions === terminalDimensions) return;
    terminal.resize(columns, rows);
    terminalDimensions = dimensions;
    sendPacket({ type: 'resize', columns, rows });
  });
}

terminal.onData(text => {
  if (!shellReady || restoringTranscript) return;
  for (let offset = 0; offset < text.length; offset += 16384) {
    sendPacket({ type: 'input', text: text.slice(offset, offset + 16384) });
  }
});
terminal.onScroll(() => {
  clearTimeout(viewportTimer);
  if (restoringTranscript) return;
  viewportTimer = setTimeout(() => sendPacket({ type: 'viewport', scrollTop: terminal.buffer.active.viewportY }), 200);
});
socket.addEventListener('open', () => {
  terminalDimensions = undefined;
  fitTerminal();
  terminal.focus();
});
socket.addEventListener('message', event => {
  let packet;
  try {
    packet = JSON.parse(event.data);
  } catch {
    return;
  }
  if (packet.type === 'output') terminal.write(packet.text);
  if (packet.type === 'restore') {
    restoringTranscript = true;
    terminal.options.disableStdin = true;
    terminal.reset();
    terminal.write(typeof packet.state?.transcript === 'string' ? packet.state.transcript : '', () => {
      if (Number.isInteger(packet.state?.scrollTop)) terminal.scrollToLine(packet.state.scrollTop);
      restoringTranscript = false;
      terminal.options.disableStdin = !shellReady;
    });
  }
  if (packet.type === 'ready') {
    shellReady = true;
    statusElement.hidden = true;
    terminal.options.disableStdin = restoringTranscript;
  }
  if (packet.type === 'exit') {
    exited = true;
    shellReady = false;
    terminal.options.disableStdin = true;
    showStatus(`Shell terminata (${packet.exitCode}). Ricarica la pagina per aprirne una nuova.`);
  }
  if (packet.type === 'error') showStatus(packet.message, true);
});
socket.addEventListener('close', () => {
  shellReady = false;
  terminal.options.disableStdin = true;
  if (!exited) showStatus('Connessione chiusa. Ricarica la pagina per aprire una nuova shell.', true);
});
socket.addEventListener('error', () => showStatus('Il server del terminale non è raggiungibile.', true));
const resizeObserver = new ResizeObserver(fitTerminal);
resizeObserver.observe(terminalElement);
window.addEventListener('focus', () => terminal.focus());
window.addEventListener('beforeunload', () => {
  clearTimeout(viewportTimer);
  sendPacket({ type: 'viewport', scrollTop: terminal.buffer.active.viewportY });
  resizeObserver.disconnect();
  socket.close();
});
fitTerminal();
