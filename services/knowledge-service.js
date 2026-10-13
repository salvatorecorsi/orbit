const path = require('node:path');
const { fork } = require('node:child_process');
const { randomUUID } = require('node:crypto');

function createKnowledgeService({ nodeExecutable = process.execPath, cacheDirectory, onStatus = () => {}, embeddings = true, embeddingTimeoutMs } = {}) {
  const requests = new Map();
  let child;
  let ready;
  let stopped = false;
  let lastErrorOutput = '';

  function request(method, payload) {
    return new Promise((resolve, reject) => {
      if (!child?.connected) {
        reject(new Error('Il servizio di indicizzazione non è collegato.'));
        return;
      }
      const id = randomUUID();
      const timeout = setTimeout(() => {
        requests.delete(id);
        reject(new Error('Il servizio di indicizzazione non ha risposto.'));
      }, 15 * 60 * 1000);
      requests.set(id, { resolve, reject, timeout });
      child.send({ id, method, ...payload }, error => {
        if (!error) return;
        clearTimeout(timeout);
        requests.delete(id);
        reject(error);
      });
    });
  }

  function start() {
    if (stopped) return Promise.reject(new Error('Il servizio di indicizzazione è stato chiuso.'));
    if (ready) return ready;
    child = fork(path.join(__dirname, 'knowledge-worker.js'), [], {
      execPath: nodeExecutable, cwd: __dirname, silent: true, windowsHide: true,
      env: { ...process.env }
    });
    child.stdout.on('data', () => {});
    child.stderr.on('data', chunk => { lastErrorOutput = (lastErrorOutput + chunk.toString()).slice(-3000); });
    child.on('message', message => {
      if (message.type === 'status') {
        try { onStatus(message.status); } catch {}
        return;
      }
      const pending = requests.get(message.id);
      if (!pending) return;
      clearTimeout(pending.timeout);
      requests.delete(message.id);
      if (message.error) pending.reject(new Error(message.error));
      else pending.resolve(message.result);
    });
    function rejectPending(error) {
      for (const pending of requests.values()) {
        clearTimeout(pending.timeout);
        pending.reject(error);
      }
      requests.clear();
      ready = null;
      child = null;
    }
    child.on('error', error => rejectPending(error));
    child.on('exit', (code, signal) => {
      if (requests.size) rejectPending(new Error(`Servizio indice terminato (${signal || code}). ${lastErrorOutput.trim()}`));
      else { ready = null; child = null; }
    });
    ready = request('initialize', { options: { cacheDirectory, embeddings, ...(embeddingTimeoutMs ? { embeddingTimeoutMs } : {}) } });
    return ready;
  }

  async function refresh(parameters) {
    await start();
    return request('refresh', { request: parameters });
  }

  async function retrieve(parameters) {
    await start();
    return request('retrieve', { request: parameters });
  }

  async function stop() {
    stopped = true;
    const running = child;
    if (!running) return;
    const timeout = setTimeout(() => running.kill(), 2000);
    try {
      if (running.connected) await request('stop', {});
    } catch {}
    finally { clearTimeout(timeout); if (running.connected) running.disconnect(); }
  }

  return { refresh, retrieve, stop };
}

module.exports = { createKnowledgeService };
