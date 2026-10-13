(() => {
  if (window.top !== window) return;
  const parameters = new URLSearchParams(window.location.search);
  const tabId = parameters.get('tab');
  const token = parameters.get('token');
  if (!tabId || !token) return;

  async function request(method, params) {
    const response = await fetch(`/api/tab/${encodeURIComponent(tabId)}/${method}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
      body: JSON.stringify(params ?? {})
    });
    const result = await response.json();
    if (!response.ok || result.error) throw new Error(result.error || 'Servizio non disponibile.');
    return result.result;
  }

  if (window.location.pathname === '/content/chat.html') {
    const events = new EventSource(`/api/tab/${encodeURIComponent(tabId)}/events?token=${encodeURIComponent(token)}`);
    const subscribe = (event, callback) => {
      const listener = message => callback(JSON.parse(message.data));
      events.addEventListener(event, listener);
      return () => events.removeEventListener(event, listener);
    };
    window.orbitChat = Object.freeze({
      getConversation: legacy => request('conversation', { legacy }),
      saveConversation: conversation => request('saveConversation', conversation),
      getRuntime: () => request('runtime'),
      updateSettings: settings => request('updateSettings', settings),
      sendMessage: message => request('sendMessage', message),
      cancelTurn: () => request('cancelTurn'),
      openSource: source => request('openSource', source),
      getContext: () => request('context'),
      onTurn: callback => subscribe('turn', callback),
      onContext: callback => subscribe('context', callback),
      onBeforeClose: callback => subscribe('beforeClose', callback),
      readyToClose: () => request('readyToClose')
    });
    window.addEventListener('pagehide', () => events.close(), { once: true });
  }

  if (window.location.pathname === '/content/file.html') {
    window.orbitFile = Object.freeze({
      getFile: () => request('file'),
      openFile: () => request('openFile')
    });
  }
})();
