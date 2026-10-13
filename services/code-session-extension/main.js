const vscode = require('vscode');
const fs = require('node:fs/promises');
const path = require('node:path');
const { randomUUID } = require('node:crypto');

let finishSession;

function serializePosition(position) {
  return { line: position.line, character: position.character };
}

function serializeRange(range) {
  return { start: serializePosition(range.start), end: serializePosition(range.end) };
}

function documentPosition(document, position) {
  const line = Math.max(0, Math.min(document.lineCount - 1, Math.floor(Number(position?.line) || 0)));
  const character = Math.max(0, Math.min(document.lineAt(line).text.length, Math.floor(Number(position?.character) || 0)));
  return new vscode.Position(line, character);
}

function sameFile(first, second) {
  return first.replace(/\\/g, '/').toLowerCase() === second.replace(/\\/g, '/').toLowerCase();
}

function sessionLocation() {
  const configuration = vscode.workspace.getConfiguration('orbit');
  const projectId = configuration.get('projectId');
  const elementId = configuration.get('elementId');
  const stateFile = configuration.get('stateFile');
  const root = vscode.workspace.workspaceFolders?.[0]?.uri;
  const workspaceFile = vscode.workspace.workspaceFile;
  if (!root || !workspaceFile || typeof projectId !== 'string' || !projectId || typeof elementId !== 'string' || !/^[a-zA-Z0-9][a-zA-Z0-9_-]{0,119}$/.test(elementId) || typeof stateFile !== 'string' || !stateFile) return;
  const statePath = path.resolve(path.dirname(workspaceFile.fsPath), stateFile);
  if (!sameFile(statePath, path.join(root.fsPath, '.orbit', 'editor', `${elementId}.json`))) return;
  return { root, projectId, elementId, statePath };
}

function documentReference(uri, root) {
  const reference = { uri: uri.toString() };
  if (uri.scheme !== 'file' && uri.scheme !== 'vscode-remote') return reference;
  const relativePath = path.relative(root.fsPath, uri.fsPath);
  if (relativePath !== '..' && !relativePath.startsWith(`..${path.sep}`) && !path.isAbsolute(relativePath)) reference.relativePath = relativePath.split(path.sep).join('/');
  return reference;
}

function restoreUri(reference, root) {
  if (typeof reference.relativePath === 'string' && !path.posix.isAbsolute(reference.relativePath) && !reference.relativePath.split('/').includes('..')) {
    return vscode.Uri.joinPath(root, ...reference.relativePath.split('/'));
  }
  const uri = vscode.Uri.parse(reference.sourceUri || reference.uri);
  if (uri.scheme === 'file' || uri.scheme === 'vscode-remote') return uri.with({ scheme: root.scheme, authority: root.authority });
  return uri;
}

function previousEditor(snapshot, uri, viewColumn, root) {
  const group = snapshot?.groups?.find(candidate => candidate.viewColumn === viewColumn);
  return group?.editors?.find(editor => {
    if (editor.uri === uri) return true;
    try { return restoreUri(editor, root).toString() === uri; } catch { return false; }
  });
}

function captureSession(location, previousSnapshot, recoveredDocuments, activationId) {
  const documents = new Map(vscode.workspace.textDocuments.map(document => [document.uri.toString(), document]));
  const groups = vscode.window.tabGroups.all.map(group => ({
    viewColumn: group.viewColumn,
    active: group.isActive,
    editors: group.tabs.filter(tab => tab.input instanceof vscode.TabInputText).map(tab => {
      const uri = tab.input.uri.toString();
      const document = documents.get(uri);
      const editor = vscode.window.visibleTextEditors.find(candidate => candidate.viewColumn === group.viewColumn && candidate.document.uri.toString() === uri);
      const recovered = recoveredDocuments.get(uri);
      const savedEditor = previousEditor(previousSnapshot, recovered?.sourceUri || uri, group.viewColumn, location.root);
      return {
        ...documentReference(tab.input.uri, location.root),
        ...(recovered || {}),
        active: tab.isActive,
        preview: tab.isPreview,
        pinned: tab.isPinned,
        languageId: document?.languageId || savedEditor?.languageId,
        dirty: Boolean(document?.isDirty || tab.isDirty),
        ...(document?.isDirty ? { text: document.getText() } : {}),
        ...(editor ? {
          selections: editor.selections.map(selection => ({ anchor: serializePosition(selection.anchor), active: serializePosition(selection.active) })),
          visibleRanges: editor.visibleRanges.map(serializeRange)
        } : {
          ...(savedEditor?.selections ? { selections: savedEditor.selections } : {}),
          ...(savedEditor?.visibleRanges ? { visibleRanges: savedEditor.visibleRanges } : {})
        })
      };
    })
  }));
  const activeEditor = vscode.window.activeTextEditor;
  return {
    version: 1,
    adapter: 'vscode-extension',
    projectId: location.projectId,
    elementId: location.elementId,
    activationId,
    capturedAt: new Date().toISOString(),
    groups,
    active: activeEditor ? { ...documentReference(activeEditor.document.uri, location.root), viewColumn: activeEditor.viewColumn } : null
  };
}

async function atomicWrite(statePath, snapshot) {
  const stagedPath = `${statePath}.${randomUUID()}.pending`;
  try {
    await fs.writeFile(stagedPath, `${JSON.stringify(snapshot, null, 2)}\n`, { mode: 0o600 });
    await fs.rename(stagedPath, statePath);
  } finally {
    await fs.rm(stagedPath, { force: true });
  }
}

async function restoreDocument(reference, root, recoveredDocuments) {
  const uri = restoreUri(reference, root);
  const savedUri = vscode.Uri.parse(reference.uri);
  const restoredUntitled = savedUri.scheme === 'untitled' && vscode.workspace.textDocuments.find(document => document.uri.toString() === savedUri.toString());
  if (restoredUntitled) {
    if (reference.sourceUri) recoveredDocuments.set(restoredUntitled.uri.toString(), { sourceUri: uri.toString(), ...(reference.relativePath ? { relativePath: reference.relativePath } : {}) });
    return restoredUntitled;
  }
  if (uri.scheme === 'untitled') {
    const document = await vscode.workspace.openTextDocument({ language: reference.languageId || 'plaintext', content: reference.text || '' });
    if (reference.sourceUri) recoveredDocuments.set(document.uri.toString(), { sourceUri: reference.sourceUri, ...(reference.relativePath ? { relativePath: reference.relativePath } : {}) });
    return document;
  }
  const existing = vscode.workspace.textDocuments.find(document => document.uri.toString() === uri.toString());
  if (existing?.isDirty) return existing;
  if (reference.dirty && typeof reference.text === 'string') {
    const document = await vscode.workspace.openTextDocument({ language: reference.languageId || 'plaintext', content: reference.text });
    recoveredDocuments.set(document.uri.toString(), { sourceUri: uri.toString(), ...(reference.relativePath ? { relativePath: reference.relativePath } : {}) });
    return document;
  }
  return vscode.workspace.openTextDocument(uri);
}

function restoreEditorPosition(editor, reference) {
  if (Array.isArray(reference.selections) && reference.selections.length) {
    editor.selections = reference.selections.map(selection => new vscode.Selection(
      documentPosition(editor.document, selection.anchor),
      documentPosition(editor.document, selection.active)
    ));
  }
  const visibleRange = reference.visibleRanges?.[0];
  if (visibleRange) editor.revealRange(new vscode.Range(
    documentPosition(editor.document, visibleRange.start),
    documentPosition(editor.document, visibleRange.end)
  ), vscode.TextEditorRevealType.AtTop);
}

async function restoreSession(snapshot, location, recoveredDocuments, output) {
  if (!Array.isArray(snapshot?.groups)) return;
  const restored = [];
  const retained = new Set(snapshot.groups.flatMap(group => Array.isArray(group.editors) ? group.editors : []).map(reference => {
    try { return restoreUri(reference, location.root).toString(); } catch { return ''; }
  }));
  const stale = vscode.window.tabGroups.all.flatMap(group => group.tabs).filter(tab => tab.input instanceof vscode.TabInputText && !tab.isDirty && !retained.has(tab.input.uri.toString()));
  if (stale.length) await vscode.window.tabGroups.close(stale, true);
  for (const group of snapshot.groups) {
    const viewColumn = Math.max(1, Math.min(9, Math.floor(Number(group.viewColumn) || 1)));
    for (const reference of Array.isArray(group.editors) ? group.editors : []) {
      if (typeof reference?.uri !== 'string') continue;
      try {
        const document = await restoreDocument(reference, location.root, recoveredDocuments);
        const editor = await vscode.window.showTextDocument(document, { viewColumn, preserveFocus: true, preview: false });
        restoreEditorPosition(editor, reference);
        restored.push({ document, reference, viewColumn, activeGroup: group.active });
      } catch (error) {
        output.appendLine(`Ripristino ${reference.relativePath || reference.uri}: ${error.message}`);
      }
    }
  }
  const restoredUris = new Set(restored.map(editor => editor.document.uri.toString()));
  const replaced = vscode.window.tabGroups.all.flatMap(group => group.tabs).filter(tab => tab.input instanceof vscode.TabInputText && !tab.isDirty && !restoredUris.has(tab.input.uri.toString()));
  if (replaced.length) await vscode.window.tabGroups.close(replaced, true);
  for (const group of snapshot.groups) {
    const active = restored.find(editor => editor.viewColumn === group.viewColumn && editor.reference.active);
    if (!active) continue;
    const editor = await vscode.window.showTextDocument(active.document, { viewColumn: active.viewColumn, preserveFocus: true, preview: Boolean(active.reference.preview) });
    restoreEditorPosition(editor, active.reference);
  }
  const active = restored.find(editor => editor.activeGroup && editor.reference.active);
  if (active) {
    const editor = await vscode.window.showTextDocument(active.document, { viewColumn: active.viewColumn, preserveFocus: false, preview: Boolean(active.reference.preview) });
    restoreEditorPosition(editor, active.reference);
  }
}

async function activate(context) {
  const location = sessionLocation();
  if (!location) return;
  const output = vscode.window.createOutputChannel('ORBIT Sessions');
  context.subscriptions.push(output);
  const recoveredDocuments = new Map();
  const activationId = randomUUID();
  let snapshot;
  let bootstrapping = true;
  let stopped = false;
  let captureTimer;
  let writeQueue = Promise.resolve();

  function capture() {
    clearTimeout(captureTimer);
    if (bootstrapping || stopped) return writeQueue;
    snapshot = captureSession(location, snapshot, recoveredDocuments, activationId);
    const captured = snapshot;
    writeQueue = writeQueue.catch(() => {}).then(() => atomicWrite(location.statePath, captured)).catch(error => output.appendLine(`Salvataggio sessione: ${error.message}`));
    return writeQueue;
  }

  function scheduleCapture() {
    if (bootstrapping || stopped) return;
    clearTimeout(captureTimer);
    captureTimer = setTimeout(capture, 300);
  }

  const events = [
    vscode.window.tabGroups.onDidChangeTabs,
    vscode.window.tabGroups.onDidChangeTabGroups,
    vscode.window.onDidChangeActiveTextEditor,
    vscode.window.onDidChangeVisibleTextEditors,
    vscode.window.onDidChangeTextEditorSelection,
    vscode.window.onDidChangeTextEditorVisibleRanges,
    vscode.window.onDidChangeTextEditorViewColumn,
    vscode.workspace.onDidChangeTextDocument,
    vscode.workspace.onDidSaveTextDocument,
    vscode.workspace.onDidCloseTextDocument,
    vscode.workspace.onDidOpenTextDocument,
    vscode.workspace.onDidRenameFiles
  ];
  for (const event of events) context.subscriptions.push(event(scheduleCapture));
  context.subscriptions.push(vscode.window.onDidChangeWindowState(() => { void capture(); }));
  finishSession = () => {
    const finished = capture();
    stopped = true;
    clearTimeout(captureTimer);
    return finished;
  };
  try {
    snapshot = JSON.parse(await fs.readFile(location.statePath, 'utf8'));
    if (!snapshot || !Array.isArray(snapshot.groups) || snapshot.groups.some(group => !group || !Number.isFinite(group.viewColumn) || !Array.isArray(group.editors))) throw new Error('Stato editor non valido: il file esistente viene conservato.');
    if (snapshot.projectId && snapshot.projectId !== location.projectId || snapshot.elementId && snapshot.elementId !== location.elementId) throw new Error('Stato editor di un altro elemento: il file esistente viene conservato.');
  } catch (error) {
    if (error.code !== 'ENOENT') {
      output.appendLine(`Lettura sessione: ${error.message}`);
      return;
    }
  }
  try {
    await restoreSession(snapshot, location, recoveredDocuments, output);
  } finally {
    bootstrapping = false;
    output.appendLine(`Sessione attiva: ${location.elementId}`);
    await capture();
  }
}

function deactivate() {
  return finishSession?.();
}

module.exports = { activate, deactivate };
