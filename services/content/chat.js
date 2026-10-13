const parameters = new URLSearchParams(window.location.search);
const conversationKey = `orbit-chat-messages:${parameters.get('tab') || 'standalone'}`;
const conversation = document.querySelector('#conversation');
const messageList = document.querySelector('#messages');
const emptyChat = document.querySelector('#empty-chat');
const responseStatus = document.querySelector('#response-status');
const composer = document.querySelector('#composer');
const messageInput = document.querySelector('#message-input');
const sendButton = document.querySelector('#send-message');
const stopButton = document.querySelector('#stop-message');
const retryButton = document.querySelector('#retry-message');
const providerInput = document.querySelector('#chat-provider');
const modelInput = document.querySelector('#chat-model');
const modelOptions = document.querySelector('#chat-models');
const engineStatus = document.querySelector('#engine-status');
const indexStatus = document.querySelector('#index-status');
const contextElements = document.querySelector('#context-elements');
const contextHint = document.querySelector('#context-hint');
const contextCount = document.querySelector('#context-count');
const kindLabels = { browser: 'Browser', figma: 'Figma', code: 'Code', terminal: 'Terminale', image: 'Immagine', pdf: 'PDF', file: 'File', chat: 'Chat', draft: 'Elemento' };
const terminalStatuses = new Set(['completed', 'error', 'cancelled', 'interrupted']);
const messages = [];
const messageElements = new Map();
const completedRuns = new Set();
const renderedMessages = new WeakMap();
let conversationReady = false;
let runtimeReady = false;
let sendingMessage = false;
let updatingSettings = false;
let runtime = { providers: [], settings: { provider: 'codex', model: '' }, run: null };
let activeRun = null;
let lastConversationAt = 0;
let turnRevision = 0;
let draftSaveTimer;
let streamFrame;
let conversationSaves = Promise.resolve();

function restoreMessages() {
  try {
    const stored = JSON.parse(localStorage.getItem(conversationKey) || '[]');
    return Array.isArray(stored) ? stored.filter(message => typeof message?.text === 'string' && message.text.trim()) : [];
  } catch {
    return [];
  }
}

function runIsActive() {
  return !!activeRun && !terminalStatuses.has(activeRun.status);
}

function selectedProvider() {
  return runtime.providers.find(provider => provider.id === runtime.settings.provider);
}

function showStatus(text = '', error = false) {
  responseStatus.textContent = text;
  responseStatus.hidden = !text;
  responseStatus.dataset.error = String(error);
}

function showError(error) {
  showStatus(error?.message || String(error) || 'Non è stato possibile completare la richiesta.', true);
}

function nearConversationEnd() {
  return conversation.scrollHeight - conversation.scrollTop - conversation.clientHeight < 120;
}

function scrollConversation(force = false) {
  if (force || nearConversationEnd()) conversation.scrollTop = conversation.scrollHeight;
}

function sourceTitle(source) {
  const title = source?.title || source?.name || source?.file || source?.path || source?.url || 'Fonte';
  const page = source?.page || source?.pageNumber;
  const line = source?.line || source?.startLine || source?.lineStart;
  return `${title}${page ? ` · p. ${page}` : line ? ` · r. ${line}` : ''}`;
}

async function openSource(source, messageId) {
  try {
    await window.orbitChat.openSource(messageId ? { ...source, messageId } : source);
  } catch (error) {
    showError(error);
  }
}

function appendInline(parent, text, messageId) {
  const expressions = /\*\*([^*\n]+)\*\*|`([^`\n]+)`|\[([^\]\n]+)\]\(([^\s)]+)\)/g;
  let previousEnd = 0;
  for (const match of text.matchAll(expressions)) {
    parent.append(document.createTextNode(text.slice(previousEnd, match.index)));
    if (match[1]) {
      const strong = document.createElement('strong');
      strong.textContent = match[1];
      parent.append(strong);
    } else if (match[2]) {
      const code = document.createElement('code');
      code.textContent = match[2];
      parent.append(code);
    } else {
      let address;
      try { address = new URL(match[4]); } catch {}
      if (address && ['https:', 'http:'].includes(address.protocol)) {
        const link = document.createElement('button');
        link.type = 'button';
        link.className = 'message-link';
        link.textContent = match[3];
        link.title = address.href;
        link.addEventListener('click', () => openSource({ url: address.href }, messageId));
        parent.append(link);
      } else {
        parent.append(document.createTextNode(match[3]));
      }
    }
    previousEnd = match.index + match[0].length;
  }
  parent.append(document.createTextNode(text.slice(previousEnd)));
}

function renderMarkdown(parent, text, messageId) {
  parent.replaceChildren();
  const segments = String(text || '').split(/```([^\n]*)\n([\s\S]*?)(?:```|$)/g);
  for (let index = 0; index < segments.length; index += 3) {
    for (const paragraphText of segments[index].split(/\n{2,}/).filter(Boolean)) {
      const heading = paragraphText.match(/^(#{1,3})\s+([^\n]+)$/);
      const paragraph = document.createElement(heading ? `h${heading[1].length + 1}` : 'p');
      const lines = (heading ? heading[2] : paragraphText).split('\n');
      lines.forEach((line, lineIndex) => {
        if (lineIndex) paragraph.append(document.createElement('br'));
        appendInline(paragraph, line, messageId);
      });
      parent.append(paragraph);
    }
    if (segments[index + 2] === undefined) continue;
    const pre = document.createElement('pre');
    const code = document.createElement('code');
    code.textContent = segments[index + 2];
    if (segments[index + 1].trim()) code.dataset.language = segments[index + 1].trim().slice(0, 40);
    pre.append(code);
    parent.append(pre);
  }
}

function renderSources(parent, sources = [], messageId) {
  parent.replaceChildren();
  parent.hidden = !sources.length;
  for (const source of sources) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'message-source';
    button.textContent = sourceTitle(source);
    button.title = source?.path || source?.url || sourceTitle(source);
    button.addEventListener('click', () => openSource(source, messageId));
    parent.append(button);
  }
}

function renderMessage(message) {
  const id = message.id || `${message.role}:${messages.indexOf(message)}`;
  let element = messageElements.get(id);
  if (!element) {
    element = document.createElement('article');
    element.className = 'chat-message';
    element.dataset.role = message.role || 'user';
    element.dataset.messageId = id;
    element.setAttribute('aria-label', message.role === 'assistant' ? 'Risposta' : 'Il tuo messaggio');
    const content = document.createElement('div');
    const sources = document.createElement('div');
    const state = document.createElement('small');
    content.className = 'message-content';
    sources.className = 'message-sources';
    state.className = 'message-state';
    element.append(content, sources, state);
    messageElements.set(id, element);
    messageList.append(element);
  }
  const previous = renderedMessages.get(element);
  if (previous && previous.text === message.text && previous.status === message.status && previous.sources === message.sources) return element;
  renderedMessages.set(element, { text: message.text, status: message.status, sources: message.sources });
  const content = element.querySelector('.message-content');
  const state = element.querySelector('.message-state');
  element.dataset.status = message.status || '';
  if (message.role === 'assistant') {
    renderMarkdown(content, message.text, id);
    if (!message.text && !terminalStatuses.has(message.status)) {
      const waiting = document.createElement('span');
      waiting.className = 'thinking';
      waiting.textContent = '…';
      waiting.setAttribute('aria-label', 'Risposta in preparazione');
      content.append(waiting);
    }
    renderSources(element.querySelector('.message-sources'), Array.isArray(message.sources) ? message.sources : [], id);
    const states = { cancelled: 'Risposta interrotta', interrupted: 'Sessione interrotta', error: 'Risposta non completata' };
    state.textContent = states[message.status] || '';
    state.hidden = !state.textContent;
  } else {
    content.textContent = message.text;
    element.querySelector('.message-sources').hidden = true;
    state.hidden = true;
  }
  return element;
}

function renderConversation() {
  const follow = nearConversationEnd();
  const visibleIds = new Set();
  for (const message of messages) {
    const element = renderMessage(message);
    visibleIds.add(element.dataset.messageId);
    messageList.append(element);
  }
  if (runIsActive()) {
    const pending = renderMessage({ id: activeRun.messageId || activeRun.runId, role: 'assistant', text: activeRun.text || '', status: activeRun.status, sources: activeRun.sources || [] });
    visibleIds.add(pending.dataset.messageId);
    messageList.append(pending);
  }
  for (const [id, element] of messageElements) {
    if (visibleIds.has(id)) continue;
    element.remove();
    messageElements.delete(id);
  }
  emptyChat.hidden = messages.length > 0 || runIsActive();
  if (follow) scrollConversation(true);
  updateControls();
}

function applyConversation(stored, restoreDraft = false) {
  if (!stored || !Array.isArray(stored.messages)) return;
  const updatedAt = Date.parse(stored.updatedAt) || 0;
  if (updatedAt && updatedAt < lastConversationAt) return;
  lastConversationAt = Math.max(lastConversationAt, updatedAt);
  messages.splice(0, messages.length, ...stored.messages.filter(message => typeof message?.text === 'string'));
  if (restoreDraft) messageInput.value = typeof stored.draft === 'string' ? stored.draft : '';
  try { localStorage.setItem(conversationKey, JSON.stringify(messages)); } catch {}
  renderConversation();
}

function updateControls() {
  const busy = runIsActive();
  const provider = selectedProvider();
  sendButton.disabled = !conversationReady || !runtimeReady || sendingMessage || updatingSettings || busy || provider?.available !== true || !messageInput.value.trim();
  stopButton.hidden = !busy;
  providerInput.disabled = !runtimeReady || busy || sendingMessage || updatingSettings;
  modelInput.disabled = providerInput.disabled;
  const latestAssistant = [...messages].reverse().find(message => message.role === 'assistant');
  retryButton.hidden = busy || sendingMessage || !runtimeReady || provider?.available !== true || !latestAssistant || !['error', 'cancelled', 'interrupted'].includes(latestAssistant.status);
  messageInput.disabled = !conversationReady || sendingMessage;
}

function resizeComposer() {
  messageInput.style.height = 'auto';
  messageInput.style.height = `${Math.min(140, Math.max(44, innerHeight * .22), messageInput.scrollHeight)}px`;
  updateControls();
}

function persistConversation(record) {
  conversationSaves = conversationSaves.catch(() => {}).then(() => {
    if (!window.orbitChat?.saveConversation) throw new Error('Il salvataggio della conversazione non è disponibile.');
    return window.orbitChat.saveConversation(record);
  });
  return conversationSaves;
}

function scheduleDraftSave() {
  clearTimeout(draftSaveTimer);
  if (!conversationReady || sendingMessage) return;
  draftSaveTimer = setTimeout(() => persistConversation({ draft: messageInput.value }).catch(showError), 400);
}

function refreshEngineControls() {
  providerInput.replaceChildren();
  for (const provider of runtime.providers) {
    const option = document.createElement('option');
    option.value = provider.id;
    option.textContent = provider.id === 'claude' ? 'Claude' : 'Codex';
    option.title = provider.error || '';
    providerInput.append(option);
  }
  providerInput.value = runtime.settings.provider;
  modelInput.value = runtime.settings.model || '';
  modelOptions.replaceChildren();
  for (const model of selectedProvider()?.models || []) {
    const option = document.createElement('option');
    option.value = typeof model === 'string' ? model : model.id;
    option.textContent = typeof model === 'string' ? model : model.label || model.id;
    modelOptions.append(option);
  }
  const provider = selectedProvider();
  engineStatus.textContent = provider?.available ? `${provider.id === 'claude' ? 'Claude' : 'Codex'} collegato` : provider?.error || 'Il motore selezionato non è disponibile.';
  engineStatus.dataset.error = String(!provider?.available);
  updateControls();
}

function applyRuntime(record, restoreRun = true) {
  if (!record) return;
  runtime = { ...runtime, ...record, settings: { ...runtime.settings, ...record.settings } };
  runtimeReady = true;
  if (restoreRun && record.run && !terminalStatuses.has(record.run.status)) activeRun = { ...record.run };
  if (record.conversation) applyConversation(record.conversation);
  refreshEngineControls();
  renderConversation();
  if (runIsActive()) showStatus(activeRun.activity || 'Preparazione della risposta…');
}

async function updateSettings() {
  if (runIsActive() || updatingSettings || !runtimeReady) return;
  updatingSettings = true;
  updateControls();
  try {
    applyRuntime(await window.orbitChat.updateSettings({ provider: providerInput.value, model: modelInput.value.trim() }), false);
  } catch (error) {
    showError(error);
    refreshEngineControls();
  } finally {
    updatingSettings = false;
    updateControls();
  }
}

async function sendMessage(retry = false) {
  const text = messageInput.value.trim();
  if ((!text && !retry) || !conversationReady || !runtimeReady || sendingMessage || runIsActive() || selectedProvider()?.available !== true) return;
  clearTimeout(draftSaveTimer);
  sendingMessage = true;
  showStatus('Recupero del contesto…');
  updateControls();
  try {
    await conversationSaves.catch(showError);
    if (retry) await persistConversation({ draft: messageInput.value });
    const result = await window.orbitChat.sendMessage({ text, id: crypto.randomUUID(), retry });
    if (!retry) messageInput.value = '';
    if (!completedRuns.has(result.runId) && (!activeRun || activeRun.runId !== result.runId)) activeRun = { runId: result.runId, status: 'starting', text: '', sources: [] };
    applyConversation(result.conversation);
    scrollConversation(true);
  } catch (error) {
    showError(error);
  } finally {
    sendingMessage = false;
    resizeComposer();
    messageInput.focus();
  }
}

function scheduleStreamRender() {
  if (streamFrame) return;
  streamFrame = requestAnimationFrame(() => {
    streamFrame = null;
    renderConversation();
  });
}

function updateIndexStatus(turn) {
  const summary = turn.summary;
  if (!summary || typeof summary !== 'object') return;
  const sources = Array.isArray(turn.sources) ? turn.sources : [];
  const documents = Array.isArray(turn.documents) ? turn.documents : [];
  const parts = [summary.mode === 'hybrid' ? 'Ricerca semantica e testuale' : 'Ricerca testuale'];
  parts.push(sources.length === 1 ? '1 fonte' : `${sources.length} fonti`);
  if (summary.mode === 'lexical' && summary.fallbackReason) parts.push('semantica non disponibile');
  const scanned = documents.filter(document => document.status === 'needs-ocr');
  const images = documents.filter(document => document.status === 'metadata-only');
  const excluded = documents.filter(document => ['unsupported', 'too-large', 'error'].includes(document.status));
  if (scanned.length) parts.push(`${scanned.length} PDF ${scanned.length === 1 ? 'richiede' : 'richiedono'} OCR`);
  if (images.length) parts.push(images.length === 1 ? '1 immagine: ricerca per nome' : `${images.length} immagini: ricerca per nome`);
  if (excluded.length) parts.push(excluded.length === 1 ? '1 file non leggibile' : `${excluded.length} file non leggibili`);
  if (documents.some(document => document.kind === 'figma')) parts.push('Figma: testi della pagina, canvas escluso');
  indexStatus.textContent = parts.join(' · ');
  indexStatus.title = [...scanned, ...images, ...excluded].map(document => `${document.title}: ${document.error || document.status}`).join('\n');
  indexStatus.hidden = false;
}

function onTurn(turn) {
  if (!turn || typeof turn.runId !== 'string') return;
  turnRevision++;
  if (turn.type === 'started') {
    activeRun = { runId: turn.runId, messageId: turn.messageId, status: 'starting', text: '', sources: [] };
    if (turn.conversation) applyConversation(turn.conversation);
    showStatus('Recupero del contesto…');
    renderConversation();
    return;
  }
  if (!activeRun || activeRun.runId !== turn.runId) return;
  if (turn.type === 'context') {
    updateIndexStatus(turn);
    activeRun.sources = Array.isArray(turn.sources) ? turn.sources : [];
    activeRun.status = 'running';
    showStatus(typeof turn.summary === 'string' ? turn.summary : activeRun.sources.length ? `${activeRun.sources.length} fonti recuperate · risposta in preparazione…` : 'Risposta in preparazione…');
    renderConversation();
  } else if (turn.type === 'delta') {
    activeRun.status = 'running';
    activeRun.text = `${activeRun.text || ''}${turn.text || ''}`;
    showStatus('');
    scheduleStreamRender();
  } else if (turn.type === 'activity') {
    activeRun.activity = typeof turn.activity === 'string' ? turn.activity : '';
    showStatus(activeRun.activity);
  } else if (['completed', 'error', 'cancelled'].includes(turn.type)) {
    completedRuns.add(turn.runId);
    if (completedRuns.size > 200) completedRuns.delete(completedRuns.values().next().value);
    activeRun.status = turn.type;
    if (turn.conversation) applyConversation(turn.conversation);
    else if (activeRun.text) {
      const pendingId = activeRun.messageId || activeRun.runId;
      const pendingMessage = { id: pendingId, role: 'assistant', text: activeRun.text, status: turn.type, sources: activeRun.sources || [] };
      const existing = messages.findIndex(message => message.id === pendingId);
      if (existing === -1) messages.push(pendingMessage);
      else messages.splice(existing, 1, pendingMessage);
    }
    showStatus(turn.type === 'error' ? turn.error?.message || turn.error || 'Il motore non ha completato la risposta.' : turn.type === 'cancelled' ? 'Risposta interrotta.' : '', turn.type === 'error');
    activeRun = null;
    renderConversation();
  }
}

function updateContext(context = {}) {
  const elements = Array.isArray(context.elements) ? context.elements : [];
  document.querySelector('#project-title').textContent = context.project?.title || '';
  contextElements.replaceChildren();
  for (const element of elements) {
    const badge = document.createElement('div');
    const title = document.createElement('span');
    const kind = document.createElement('small');
    badge.className = 'context-element';
    badge.dataset.elementId = element.id;
    title.textContent = element.title || 'Elemento';
    kind.textContent = kindLabels[element.kind] || 'Elemento';
    badge.append(title, kind);
    contextElements.append(badge);
  }
  contextElements.hidden = elements.length === 0;
  contextHint.hidden = elements.length > 0;
  contextCount.textContent = elements.length === 1 ? '1 elemento collegato' : elements.length ? `${elements.length} elementi collegati` : 'Contesto del progetto';
}

composer.addEventListener('submit', event => {
  event.preventDefault();
  sendMessage();
});
messageInput.addEventListener('input', () => {
  resizeComposer();
  scheduleDraftSave();
});
window.addEventListener('resize', resizeComposer);
messageInput.addEventListener('keydown', event => {
  if (event.key !== 'Enter' || event.shiftKey || event.isComposing) return;
  event.preventDefault();
  sendMessage();
});
stopButton.addEventListener('click', async () => {
  stopButton.disabled = true;
  try { await window.orbitChat.cancelTurn(); } catch (error) { showError(error); }
  finally { stopButton.disabled = false; }
});
retryButton.addEventListener('click', () => sendMessage(true));
providerInput.addEventListener('change', () => {
  modelInput.value = '';
  updateSettings();
});
modelInput.addEventListener('change', updateSettings);
modelInput.addEventListener('keydown', event => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  updateSettings();
  messageInput.focus();
});
document.querySelectorAll('[data-prompt]').forEach(button => button.addEventListener('click', () => {
  messageInput.value = button.dataset.prompt;
  resizeComposer();
  scheduleDraftSave();
  messageInput.focus();
}));

async function initializeConversation() {
  messageInput.disabled = true;
  try {
    const stored = await window.orbitChat.getConversation(restoreMessages());
    applyConversation(stored, true);
    conversationReady = true;
    renderConversation();
  } catch (error) {
    showError(error);
  } finally {
    resizeComposer();
  }
}

async function initializeRuntime() {
  const revision = turnRevision;
  try {
    applyRuntime(await window.orbitChat.getRuntime(), revision === turnRevision);
  } catch (error) {
    engineStatus.textContent = error?.message || 'Collegamento al motore non riuscito.';
    engineStatus.dataset.error = 'true';
    updateControls();
  }
}

if (window.orbitChat) {
  const unsubscribeContext = window.orbitChat.onContext(updateContext);
  const unsubscribeTurn = window.orbitChat.onTurn(onTurn);
  window.orbitChat.getContext().then(updateContext).catch(() => updateContext());
  window.addEventListener('pagehide', () => {
    unsubscribeContext();
    unsubscribeTurn();
    cancelAnimationFrame(streamFrame);
  }, { once: true });
  window.orbitChat.onBeforeClose?.(async () => {
    clearTimeout(draftSaveTimer);
    try {
      await conversationSaves.catch(showError);
      if (conversationReady) await persistConversation({ draft: messageInput.value });
    } catch (error) {
      showError(error);
    } finally {
      window.orbitChat.readyToClose();
    }
  });
  initializeConversation();
  initializeRuntime();
} else {
  engineStatus.textContent = 'Apri questa chat dentro ORBIT per collegare il motore.';
}
window.addEventListener('pagehide', () => {
  clearTimeout(draftSaveTimer);
  if (conversationReady && !sendingMessage) persistConversation({ draft: messageInput.value }).catch(() => {});
});
