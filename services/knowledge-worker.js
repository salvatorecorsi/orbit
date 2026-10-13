const { createKnowledgeIndex } = require('./knowledge-index');

let knowledge;
let pending = Promise.resolve();
let stopping = false;

function send(message) {
  if (process.connected) process.send(message);
}

async function handle(message) {
  if (message.method === 'initialize') {
    knowledge = createKnowledgeIndex({ ...message.options, onStatus: status => send({ type: 'status', status }) });
    return { ready: true };
  }
  if (!knowledge) throw new Error('Indice non inizializzato.');
  if (message.method === 'stop') {
    stopping = true;
    await knowledge.stop();
    return { stopped: true };
  }
  if (!['refresh', 'retrieve'].includes(message.method)) throw new Error('Operazione indice non valida.');
  return knowledge[message.method](message.request);
}

process.on('message', message => {
  if (!message?.id || stopping) return;
  pending = pending.then(async () => {
    try {
      const result = await handle(message);
      send({ id: message.id, result });
      if (stopping) process.disconnect();
    } catch (error) {
      send({ id: message.id, error: error.message });
    }
  });
});

process.on('disconnect', () => {
  if (!stopping) knowledge?.stop().finally(() => process.exit(0));
  else process.exit(0);
});
