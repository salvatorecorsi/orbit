const marker = new URLSearchParams(location.search).get('marker');
if (window.opener) {
  window.opener.postMessage({ type: 'orbit-smoke-auth', marker }, location.origin);
  window.close();
}
