const figmaHostname = hostname => /(?:^|\.)figma\.com$/i.test(hostname);

function address(value) {
  try {
    const parsed = new URL(value);
    return ['http:', 'https:'].includes(parsed.protocol) ? parsed : null;
  } catch { return null; }
}

function isFigmaAddress(value) {
  const parsed = address(value);
  return Boolean(parsed && figmaHostname(parsed.hostname));
}

function hasFigmaContext(tab = {}) {
  return tab.kind === 'figma' || isFigmaAddress(tab.state?.restoreUrl) || isFigmaAddress(tab.state?.figma?.url);
}

function isTransientAuthenticationAddress(value, tab = {}) {
  const parsed = address(value);
  if (!parsed) return false;
  if (hasFigmaContext(tab) && parsed.hostname === 'accounts.google.com') return true;
  if (!figmaHostname(parsed.hostname)) return false;
  if (/(?:^|[/_-])(?:finish[^/]*(?:sso|oauth)|callback|oauth2?|sso)(?:[/_-]|$)/i.test(parsed.pathname)) return true;
  const parameters = new URLSearchParams(parsed.search);
  for (const [key, value] of new URLSearchParams(parsed.hash.slice(1))) parameters.append(key, value);
  return ['access_token', 'id_token', 'refresh_token', 'client_secret'].some(key => parameters.has(key)) || parameters.has('code') && ['state', 'iss', 'scope', 'authuser'].some(key => parameters.has(key));
}

function restoreAddress(tab) {
  if (!isTransientAuthenticationAddress(tab.url, tab)) return String(tab.url || '');
  const entries = tab.state?.navigation?.entries;
  const history = Array.isArray(entries) ? [...entries].reverse().map(entry => entry?.url) : [];
  const candidates = [tab.state?.restoreUrl, tab.state?.figma?.url, ...history, tab.state?.surface?.url];
  return candidates.find(value => isFigmaAddress(value) && !isTransientAuthenticationAddress(value, tab)) || 'https://www.figma.com/login';
}

function navigationStateForDisk(tab) {
  const restoredUrl = restoreAddress(tab);
  function sanitize(value) {
    if (typeof value === 'string') return isTransientAuthenticationAddress(value, tab) ? restoredUrl : value;
    if (Array.isArray(value)) return value.map(sanitize);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, sanitize(item)]));
    return value;
  }
  const state = sanitize(tab.state || {});
  if (isFigmaAddress(restoredUrl) && !isTransientAuthenticationAddress(restoredUrl, tab)) state.restoreUrl = restoredUrl;
  return state;
}

function restoreTabNavigation(tab) {
  return { ...tab, url: restoreAddress(tab), ...(tab.state ? { state: navigationStateForDisk(tab) } : {}) };
}

module.exports = { isFigmaAddress, isTransientAuthenticationAddress, restoreAddress, navigationStateForDisk, restoreTabNavigation };
