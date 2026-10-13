const input = document.querySelector('#qa-input');
const output = document.querySelector('#qa-value');
const counter = document.querySelector('#qa-counter');
let count = 0;
input.addEventListener('input', () => { output.textContent = input.value; });
counter.addEventListener('click', event => {
  window.orbitQaLastClick = { trusted: event.isTrusted, activation: navigator.userActivation.isActive };
  counter.textContent = String(++count);
});
document.querySelector('#qa-auth-popup').addEventListener('click', event => {
  window.orbitQaAuthClick = { trusted: event.isTrusted, activation: navigator.userActivation.isActive };
  const marker = window.orbitSmokeAuthMarker || 'ORBIT_AUTH_QA';
  window.orbitSmokePopup = window.open('/content/popup.html?marker=' + encodeURIComponent(marker), 'orbit-smoke-auth', 'popup,width=640,height=480');
});
