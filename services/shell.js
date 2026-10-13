const panel = document.querySelector('#panel');
const launcherPanel = document.querySelector('#launcher-panel');
const address = document.querySelector('#address');
const launcherProject = document.querySelector('#launcher-project');
const launcherUrl = document.querySelector('#launcher-url');
const launcherForm = document.querySelector('#launcher-form');
const launcherSubmit = launcherForm.querySelector('.submit-control');
const newProjectForm = document.querySelector('#new-project-form');
const newProjectTitle = document.querySelector('#new-project-title');
const newControl = document.querySelector('#new-window');
const projectNames = new Map();
let workspace = { projects: [], tabs: [], selected: null, selectedVisible: false, expanded: false, selectedColor: '#b4b8df', currentProject: null, status: '' };
let selectedKind = 'browser';
let activePanel = null;
let creationMode = 'window';
let shellHeight = 64;
let shellOverlay = false;
let shellPaletteBounds = '';

function send(action, properties = {}) {
  window.ipc.postMessage(JSON.stringify({ action, ...properties }));
}

function resizeShell(height, overlay = false) {
  let palette = null;
  if (overlay) {
    const bounds = launcherPanel.getBoundingClientRect();
    palette = { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height, radius: parseFloat(getComputedStyle(launcherPanel).borderTopLeftRadius) || 0 };
  }
  const paletteBounds = palette ? JSON.stringify(palette) : '';
  if (height === shellHeight && overlay === shellOverlay && paletteBounds === shellPaletteBounds) return;
  shellHeight = height;
  shellOverlay = overlay;
  shellPaletteBounds = paletteBounds;
  send('shellResize', { height, overlay, ...(palette ? { palette } : {}) });
}

function resizePalette() {
  if (activePanel === 'launcher' && launcherPanel.open) resizeShell(64, true);
}

function closePanel() {
  const closingLauncher = activePanel === 'launcher';
  activePanel = null;
  if (launcherPanel.open) launcherPanel.close();
  panel.hidden = true;
  document.querySelector('#project-menu').setAttribute('aria-expanded', 'false');
  document.querySelector('#windows-menu').setAttribute('aria-expanded', 'false');
  newControl.setAttribute('aria-expanded', 'false');
  resizeShell(64);
  if (closingLauncher) newControl.focus({ preventScroll: true });
}

function showPanel(name, toggle = true) {
  if (activePanel === name && toggle) return closePanel();
  if (name !== 'launcher' && launcherPanel.open) launcherPanel.close();
  activePanel = name;
  panel.hidden = name === 'launcher';
  for (const section of panel.querySelectorAll('section')) section.hidden = section.id !== `${name}-panel`;
  document.querySelector('#project-menu').setAttribute('aria-expanded', String(name === 'projects'));
  document.querySelector('#windows-menu').setAttribute('aria-expanded', String(name === 'windows'));
  newControl.setAttribute('aria-expanded', String(name === 'launcher'));
  if (name === 'launcher' && !launcherPanel.open) launcherPanel.showModal();
  resizeShell(name === 'launcher' ? 64 : 390, name === 'launcher');
}

function focusCreation() {
  if (creationMode === 'project') return newProjectTitle.focus({ preventScroll: true });
  if (!launcherUrl.parentElement.hidden) return launcherUrl.focus({ preventScroll: true });
  const selectedOption = document.querySelector('[data-kind].is-selected');
  (selectedOption || launcherSubmit).focus({ preventScroll: true });
}

function setCreationMode(mode) {
  creationMode = mode === 'window' && !workspace.projects.length ? 'project' : mode;
  launcherForm.hidden = creationMode !== 'window';
  newProjectForm.hidden = creationMode !== 'project';
  for (const option of document.querySelectorAll('[data-create-mode]')) option.setAttribute('aria-pressed', String(option.dataset.createMode === creationMode));
  if (activePanel !== 'launcher') return;
  resizePalette();
  focusCreation();
}

function selectLauncherProject() {
  const choices = [launcherProject.value, workspace.currentProject, workspace.projects[0]?.id];
  launcherProject.value = choices.find(id => projectNames.has(id)) || '';
}

function openLauncher(mode = 'window', toggle = false) {
  if (activePanel === 'launcher' && toggle) return closePanel();
  showPanel('launcher', false);
  selectLauncherProject();
  setCreationMode(mode);
}

function makeButton(label, action, className) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = className;
  button.textContent = label;
  button.addEventListener('click', action);
  return button;
}

function renderProjects() {
  projectNames.clear();
  const list = document.querySelector('#project-list');
  list.replaceChildren();
  const selectedProject = launcherProject.value;
  launcherProject.replaceChildren();
  for (const project of workspace.projects) {
    const title = project.title || project.name || project.id;
    projectNames.set(project.id, title);
    const count = workspace.tabs.filter(tab => tab.projectId === project.id).length;
    const button = makeButton(title, () => { closePanel(); send('project', { id: project.id, projectId: project.id }); }, 'project-item');
    const counter = document.createElement('small');
    counter.textContent = `${count} finestre`;
    button.append(counter);
    list.append(button);
    const option = document.createElement('option');
    option.value = project.id;
    option.textContent = title;
    launcherProject.append(option);
  }
  launcherProject.value = projectNames.has(selectedProject) ? selectedProject : '';
  selectLauncherProject();
  const hasProjects = workspace.projects.length > 0;
  launcherProject.disabled = !hasProjects;
  launcherSubmit.disabled = !hasProjects;
  document.querySelector('[data-create-mode="window"]').disabled = !hasProjects;
  document.querySelector('#launcher-empty').hidden = hasProjects;
  if (!hasProjects && activePanel === 'launcher' && creationMode === 'window') setCreationMode('project');
  resizePalette();
  document.querySelector('#project-name').textContent = projectNames.get(workspace.currentProject) || 'Galassia';
}

function renderWindows() {
  const list = document.querySelector('#window-list');
  list.replaceChildren();
  const selectedId = typeof workspace.selected === 'object' ? workspace.selected?.id : workspace.selected;
  const visibleTabs = workspace.currentProject ? workspace.tabs.filter(tab => tab.projectId === workspace.currentProject) : workspace.tabs;
  for (const tab of visibleTabs) {
    const row = document.createElement('div');
    row.className = `window-row${tab.id === selectedId ? ' is-active' : ''}`;
    const open = makeButton('', () => { closePanel(); send('select', { id: tab.id }); }, 'window-open');
    open.disabled = !workspace.currentProject;
    if (open.disabled) open.title = 'Entra nel pianeta per interagire con questa finestra';
    const title = document.createElement('strong');
    title.textContent = tab.title || tab.kind || 'Pagina';
    const detail = document.createElement('span');
    detail.textContent = `${projectNames.get(tab.projectId) || 'Progetto'} · ${tab.kind || 'browser'}`;
    open.append(title, detail);
    const close = makeButton('×', () => send('close', { id: tab.id }), 'window-close');
    close.setAttribute('aria-label', `Chiudi ${title.textContent}`);
    row.append(open, close);
    list.append(row);
  }
  if (!visibleTabs.length) {
    const empty = document.createElement('p');
    empty.className = 'empty';
    empty.textContent = 'Nessuna finestra aperta su questo pianeta.';
    list.append(empty);
  }
  document.querySelector('#window-count').textContent = visibleTabs.length;
  document.querySelector('#windows-menu').title = workspace.currentProject ? 'Cambia finestra · Alt+Tab · Alt+Shift+Tab per tornare indietro' : 'Finestre aperte';
  document.querySelector('#scene-hint').textContent = workspace.currentProject && visibleTabs.length > 1 ? 'Alt+Tab per cambiare finestra · trascina lo spazio per ruotare' : 'Trascina nello spazio per ruotare · rotella per avvicinarti';
  const selected = workspace.tabs.find(tab => tab.id === selectedId);
  const expanded = Boolean(selected && workspace.expanded);
  document.querySelector('#navigation').hidden = !selected;
  document.querySelector('#scene-hint').hidden = expanded;
  if (selected && document.activeElement !== address) address.value = selected.url || '';
  address.readOnly = selected ? selected.kind !== 'browser' && selected.kind !== 'figma' : false;
  document.querySelector('#selection').hidden = !selected;
  document.querySelector('#selection-title').textContent = selected ? `Attiva: ${selected.title || selected.kind || 'Pagina'}` : '';
  document.querySelector('#selection-title').title = selected?.title || selected?.kind || '';
  const color = /^#[a-f\d]{6}$/i.test(workspace.selectedColor || '') ? workspace.selectedColor : '#b4b8df';
  document.documentElement.style.setProperty('--selection-color', color);
  document.body.classList.toggle('has-selection', Boolean(selected));
  document.body.classList.toggle('is-expanded', expanded);
  const expand = document.querySelector('#expand');
  expand.hidden = !selected;
  expand.disabled = !expanded && !workspace.selectedVisible;
  expand.textContent = expanded ? 'Riduci' : 'Ingrandisci';
  expand.title = expanded ? 'Torna alla finestra nell’orbita · Esc o Ctrl+Invio' : expand.disabled ? 'Ruota il pianeta per vedere questa finestra' : 'Ingrandisci la finestra · Ctrl+Invio';
  expand.setAttribute('aria-pressed', String(expanded));
}

function toggleExpansion() {
  if (activePanel === 'launcher' || !workspace.selected || !workspace.expanded && !workspace.selectedVisible) return;
  closePanel();
  send(workspace.expanded ? 'collapse' : 'expand');
}

function selectKind(kind) {
  selectedKind = kind;
  for (const option of document.querySelectorAll('[data-kind]')) {
    const selected = option.dataset.kind === kind;
    option.classList.toggle('is-selected', selected);
    option.setAttribute('aria-pressed', String(selected));
  }
  const names = { browser: 'browser', figma: 'Figma', code: 'VS Code', terminal: 'PowerShell', chat: 'chat', pdf: 'PDF', image: 'immagine', file: 'file' };
  launcherSubmit.textContent = `Apri ${names[kind]}`;
  const localFile = kind === 'pdf' || kind === 'file';
  launcherUrl.parentElement.hidden = kind === 'code' || kind === 'terminal' || kind === 'chat';
  document.querySelector('#launcher-location-label').textContent = localFile ? 'Percorso del file' : kind === 'image' ? 'Percorso o indirizzo dell’immagine' : 'Indirizzo';
  launcherUrl.placeholder = localFile ? 'C:\\…' : kind === 'figma' ? 'https://www.figma.com/…' : kind === 'image' ? 'C:\\… oppure https://…' : 'https://…';
  launcherUrl.required = localFile || kind === 'image';
  resizePalette();
}

window.orbitReceive = next => {
  workspace = { ...workspace, ...next };
  renderProjects();
  renderWindows();
  const status = document.querySelector('#status');
  status.title = workspace.status || 'Servizi pronti';
  status.classList.toggle('has-error', Boolean(workspace.status?.startsWith('Errore')));
};
window.orbitOpenLauncher = () => activePanel === 'launcher' ? focusCreation() : openLauncher();
window.orbitClosePanel = closePanel;
window.orbitPopupReceive = popup => {
  const close = document.querySelector('#close-popup');
  close.hidden = !popup.open;
  close.title = popup.open ? `Chiudi ${popup.title || 'la finestra del browser'} e torna alla pagina` : 'Chiudi la finestra del browser';
};

document.querySelector('#galaxy').addEventListener('click', () => { closePanel(); send('home'); });
document.querySelector('#expand').addEventListener('click', toggleExpansion);
document.querySelector('#deselect').addEventListener('click', () => { closePanel(); send('deselect'); });
document.querySelector('#project-menu').addEventListener('click', () => showPanel('projects'));
document.querySelector('#windows-menu').addEventListener('click', () => showPanel('windows'));
newControl.addEventListener('click', () => openLauncher('window', true));
document.querySelector('#create-project').addEventListener('click', () => openLauncher('project'));
document.querySelector('#back').addEventListener('click', () => send('back'));
document.querySelector('#forward').addEventListener('click', () => send('forward'));
document.querySelector('#close-popup').addEventListener('click', () => send('closePopup'));
for (const close of document.querySelectorAll('.close-panel')) close.addEventListener('click', closePanel);
launcherPanel.addEventListener('cancel', event => { event.preventDefault(); closePanel(); });
launcherPanel.addEventListener('close', () => { if (activePanel === 'launcher' && !launcherPanel.open) closePanel(); });
launcherPanel.addEventListener('click', event => {
  if (event.target !== launcherPanel) return;
  const bounds = launcherPanel.getBoundingClientRect();
  if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) closePanel();
});
for (const option of document.querySelectorAll('[data-create-mode]')) option.addEventListener('click', () => setCreationMode(option.dataset.createMode));
for (const option of document.querySelectorAll('[data-kind]')) option.addEventListener('click', () => selectKind(option.dataset.kind));
document.querySelector('#address-form').addEventListener('submit', event => { event.preventDefault(); send('navigate', { url: address.value }); });
launcherForm.addEventListener('submit', event => {
  event.preventDefault();
  if (!projectNames.has(launcherProject.value)) return;
  const location = launcherUrl.value.trim();
  const localFile = selectedKind === 'pdf' || selectedKind === 'file' || selectedKind === 'image' && (/^[a-z]:[\\/]/i.test(location) || location.startsWith('\\\\'));
  const properties = localFile ? { filePath: location } : { url: location || (selectedKind === 'figma' ? 'https://www.figma.com/' : 'https://example.com/') };
  send('new', { kind: selectedKind, ...properties, projectId: launcherProject.value });
  closePanel();
});
newProjectForm.addEventListener('submit', event => {
  event.preventDefault();
  const title = newProjectTitle.value.trim();
  if (!title) return;
  send('newProject', { title });
  newProjectTitle.value = '';
  closePanel();
});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && activePanel === 'launcher') return;
  if (event.key === 'Escape' && activePanel) { event.preventDefault(); closePanel(); }
  else if (event.key === 'Escape' && workspace.expanded) { event.preventDefault(); send('collapse'); }
  if (event.ctrlKey && event.key === 'Enter' && workspace.selected) { event.preventDefault(); toggleExpansion(); }
  if ((event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === 't') {
    event.preventDefault();
    window.orbitOpenLauncher();
  }
});
window.addEventListener('resize', resizePalette);
document.fonts.ready.then(resizePalette);
document.fonts.addEventListener('loadingdone', resizePalette);
selectKind('browser');
send('shellReady');
