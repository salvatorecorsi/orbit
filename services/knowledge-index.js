const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { createHash, randomUUID } = require('node:crypto');
const { cleanText, extractFile, isExcludedPath, listProjectFiles } = require('./knowledge-extractors');

const embeddingModel = 'Xenova/paraphrase-multilingual-MiniLM-L12-v2';
const indexVersion = 1;
const stopWords = new Set('a ad al alla alle allo ai agli anche che chi ci come con da dal dalla delle del dei di e ed è gli il in io la le lo ma mi ne nel nella o per più quale quali quanto questo questa rispetto se si sono su tra un una uno siamo avete hanno what which how is are the to of and in on for with we it this that'.split(' '));

function hash(value) {
  return createHash('sha256').update(value).digest('hex');
}

function terms(value) {
  return (String(value || '').normalize('NFD').replace(/\p{M}/gu, '').toLowerCase().match(/[\p{L}\p{N}_]{2,}/gu) || []).filter(word => !stopWords.has(word));
}

function insideProject(directory, filename) {
  const relative = path.relative(directory, filename);
  return relative && relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

function chunkPage(page) {
  const lines = cleanText(page.text).split('\n');
  const pieces = [];
  let current = [];
  let lineStart = 1;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (line.length > 700) {
      if (current.length) pieces.push({ text: current.join('\n'), lineStart, lineEnd: index });
      current = [];
      for (let offset = 0; offset < line.length; offset += 600) pieces.push({ text: line.slice(offset, offset + 700), lineStart: index + 1, lineEnd: index + 1 });
      lineStart = index + 2;
      continue;
    }
    if (current.length && current.join('\n').length + line.length > 700) {
      pieces.push({ text: current.join('\n'), lineStart, lineEnd: index });
      const overlap = current.slice(-1);
      current = overlap[0]?.length < 200 ? overlap : [];
      lineStart = current.length ? index : index + 1;
    }
    if (!current.length) lineStart = index + 1;
    current.push(line);
  }
  if (current.join('\n').trim()) pieces.push({ text: current.join('\n'), lineStart, lineEnd: lines.length });
  return pieces.map(piece => ({ ...piece, ...(page.page ? { page: page.page } : {}), ...(page.title ? { section: page.title } : {}) }));
}

function metadataText(project) {
  const milestones = (project.milestones || []).map(milestone => typeof milestone === 'string' ? milestone : JSON.stringify(milestone)).join('\n');
  return cleanText(`Progetto: ${project.title || project.id}\nCreato: ${project.createdAt || ''}\nDescrizione: ${project.description || ''}\nMilestone:\n${milestones}`);
}

function publicDocument(document) {
  return { id: document.id, title: document.title, kind: document.kind, status: document.status, ...(document.path ? { path: document.path } : {}), ...(document.tabId ? { tabId: document.tabId } : {}), ...(document.error ? { error: document.error } : {}) };
}

async function atomicWrite(filename, document) {
  const pending = `${filename}.${randomUUID()}.pending`;
  await fs.mkdir(path.dirname(filename), { recursive: true });
  try {
    const handle = await fs.open(pending, 'wx', 0o600);
    try {
      await handle.writeFile(JSON.stringify(document));
      await handle.sync();
    } finally {
      await handle.close();
    }
    await fs.rename(pending, filename);
  } finally {
    await fs.rm(pending, { force: true }).catch(() => {});
  }
}

function createKnowledgeIndex({ cacheDirectory, onStatus = () => {}, embeddings = true, embeddingTimeoutMs = 180000 } = {}) {
  let extractor;
  let embeddingFailure;
  let embeddingPending;
  const downloadPercentages = new Map();
  const queryVectors = new Map();
  const documentIndexes = new Map();

  function report(status) {
    onStatus(status);
  }

  async function loadExtractor() {
    if (extractor) return extractor;
    if (!embeddings) throw new Error('Embedding disattivati per questo indice.');
    if (embeddingFailure) throw new Error(embeddingFailure);
    if (!embeddingPending) {
      embeddingPending = (async () => {
        report({ phase: 'model-loading', message: 'Preparazione del modello multilingue locale.' });
        const { pipeline, env } = await import('@huggingface/transformers');
        env.cacheDir = cacheDirectory || path.join(os.tmpdir(), 'orbit-model-cache');
        env.allowLocalModels = false;
        const pending = pipeline('feature-extraction', embeddingModel, {
          dtype: 'q8', device: 'cpu',
          session_options: { intraOpNumThreads: Math.max(1, Math.min(4, os.availableParallelism?.() || os.cpus().length)), interOpNumThreads: 1 },
          progress_callback: progress => {
            if (progress.status !== 'progress' || !Number.isFinite(progress.progress)) return;
            const percent = Math.floor(progress.progress / 10) * 10;
            if (percent === downloadPercentages.get(progress.file)) return;
            downloadPercentages.set(progress.file, percent);
            report({ phase: 'model-loading', message: 'Download iniziale del modello locale.', progress: percent, file: progress.file });
          }
        });
        let timer;
        const timed = new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Il modello locale non è disponibile entro il tempo previsto.')), embeddingTimeoutMs); });
        try {
          extractor = await Promise.race([pending, timed]);
          report({ phase: 'model-ready', message: 'Embedding multilingue locali pronti.', model: embeddingModel });
          return extractor;
        } catch (error) {
          pending.then(created => created.dispose()).catch(() => {});
          throw error;
        } finally {
          clearTimeout(timer);
        }
      })().catch(error => {
        embeddingFailure = error.message;
        report({ phase: 'lexical-fallback', message: 'Ricerca testuale attiva; embedding locali non disponibili.', error: error.message });
        throw error;
      });
    }
    return embeddingPending;
  }

  async function vectorize(texts) {
    const model = await loadExtractor();
    const tensor = await model(texts, { pooling: 'mean', normalize: true, truncation: true });
    return tensor.tolist().map(vector => vector.map(number => Math.round(number * 1000000) / 1000000));
  }

  async function loadIndex(directory) {
    if (documentIndexes.has(directory)) return documentIndexes.get(directory);
    try {
      const parsed = JSON.parse(await fs.readFile(path.join(directory, '.orbit', 'index', 'index.json'), 'utf8'));
      if (parsed.version === indexVersion && parsed.model === embeddingModel && Array.isArray(parsed.documents)) {
        documentIndexes.set(directory, parsed);
        return parsed;
      }
    } catch (error) {
      if (error.code !== 'ENOENT') report({ phase: 'index-rebuild', message: 'Ricostruzione dell’indice locale.', error: error.message });
    }
    return { version: indexVersion, model: embeddingModel, documents: [] };
  }

  async function collectDocuments({ project, tabs = [], liveSources = [], chatId }, previousDocuments) {
    if (!project?.directory || !path.isAbsolute(project.directory)) throw new Error('Il progetto deve avere una cartella assoluta.');
    const directory = path.resolve(project.directory);
    const projectTabs = tabs.filter(tab => !tab.projectId || tab.projectId === project.id);
    const referenceByFile = new Map((project.references || []).filter(reference => reference.file || reference.path).map(reference => [path.resolve(directory, reference.path || reference.file), reference]));
    const tabByFile = new Map(projectTabs.filter(tab => tab.resource?.path || tab.resource?.file).map(tab => [path.resolve(directory, tab.resource.path || tab.resource.file), tab]));
    const documents = [{ id: `project:${project.id}`, title: cleanText(project.title || project.id), kind: 'project', path: path.join(directory, 'project.info'), status: 'indexed', capturedAt: project.updatedAt || new Date().toISOString(), pages: [{ text: metadataText(project) }] }];
    const files = await listProjectFiles(directory);
    for (const filename of files) {
      const reference = referenceByFile.get(filename);
      const tab = tabByFile.get(filename);
      if (chatId && (tab?.id === chatId || filename === path.join(directory, `chat-${chatId}.chat`))) continue;
      let statistics;
      try {
        statistics = await fs.stat(filename);
      } catch (error) {
        if (error.code === 'ENOENT') continue;
        throw error;
      }
      const relative = path.relative(directory, filename).split(path.sep).join('/');
      const document = { id: `file:${hash(relative).slice(0, 20)}`, title: cleanText(tab?.title || reference?.title || relative), kind: tab?.kind || reference?.kind || (path.extname(filename).toLowerCase() === '.chat' ? 'chat' : 'file'), path: filename, ...(tab ? { tabId: tab.id } : {}), capturedAt: statistics.mtime.toISOString(), fileSignature: `${statistics.size}:${statistics.mtimeMs}` };
      const previousDocument = previousDocuments.get(document.id);
      if (previousDocument?.fileSignature === document.fileSignature && previousDocument.title === document.title && previousDocument.status !== 'error') {
        if (chatId && previousDocument.chatId === chatId) continue;
        documents.push({ ...previousDocument, ...document });
        continue;
      }
      try {
        Object.assign(document, await extractFile(filename, { chatId }));
        if (document.status === 'excluded') continue;
        if (document.chatId) document.tabId ||= document.chatId;
      } catch (error) {
        Object.assign(document, { status: 'error', error: error.message, pages: [] });
      }
      documents.push(document);
    }
    for (const source of liveSources) {
      if ((chatId && (source.tabId === chatId || source.id === chatId)) || !source.text) continue;
      if (source.path && (!insideProject(directory, path.resolve(source.path)) || isExcludedPath(path.relative(directory, source.path)))) continue;
      if (source.tabId && !projectTabs.some(tab => tab.id === source.tabId)) continue;
      documents.push({ id: `live:${source.id || source.tabId}`, title: cleanText(source.title || source.tabId || 'Contenuto aperto'), kind: source.kind || 'browser', ...(source.tabId ? { tabId: source.tabId } : {}), ...(source.path ? { path: source.path } : {}), ...(source.url ? { url: cleanText(source.url) } : {}), capturedAt: source.capturedAt || new Date().toISOString(), status: 'indexed', pages: [{ text: cleanText(source.text) }] });
    }
    return documents;
  }

  async function refresh(request) {
    const directory = path.resolve(request.project.directory);
    report({ phase: 'extracting', projectId: request.project.id, message: 'Lettura dei contenuti del progetto.' });
    const previous = await loadIndex(directory);
    const previousDocuments = new Map(previous.documents.map(document => [document.id, document]));
    const documents = await collectDocuments(request, previousDocuments);
    let changedDocuments = 0;
    let reusedDocuments = 0;
    const missingVectors = [];
    for (const document of documents) {
      const contentHash = document.pages ? hash(JSON.stringify({ title: document.title, pages: document.pages, status: document.status })) : document.hash;
      const previousDocument = previousDocuments.get(document.id);
      document.hash = contentHash;
      if (previousDocument?.hash === contentHash) {
        document.chunks = previousDocument.chunks;
        reusedDocuments += 1;
      } else {
        document.chunks = document.pages.flatMap(chunkPage).map((chunk, index) => ({ ...chunk, id: `${document.id}:${index}`, terms: terms(document.title + ' ' + chunk.text) }));
        changedDocuments += 1;
      }
      delete document.pages;
      for (const chunk of document.chunks) if (!chunk.vector) missingVectors.push({ document, chunk });
    }
    if (missingVectors.length && embeddings) {
      try {
        await loadExtractor();
        for (let offset = 0; offset < missingVectors.length; offset += 8) {
          const batch = missingVectors.slice(offset, offset + 8);
          const vectors = await vectorize(batch.map(({ document, chunk }) => `${document.title}\n${chunk.text}`));
          batch.forEach(({ chunk }, index) => { chunk.vector = vectors[index]; });
          report({ phase: 'indexing', projectId: request.project.id, message: 'Indicizzazione semantica dei contenuti.', completed: Math.min(offset + 8, missingVectors.length), total: missingVectors.length });
        }
      } catch (error) {
        embeddingFailure ||= error.message;
      }
    }
    const chunks = documents.flatMap(document => document.chunks);
    const indexed = { version: indexVersion, model: embeddingModel, projectId: request.project.id, updatedAt: new Date().toISOString(), documents };
    await atomicWrite(path.join(directory, '.orbit', 'index', 'index.json'), indexed);
    documentIndexes.set(directory, indexed);
    const summary = { projectId: request.project.id, documentCount: documents.length, chunkCount: chunks.length, changedDocuments, reusedDocuments, embeddedChunks: chunks.filter(chunk => chunk.vector).length, dimensions: chunks.find(chunk => chunk.vector)?.vector.length || 0, model: embeddingModel, mode: chunks.length && chunks.every(chunk => chunk.vector) ? 'hybrid' : 'lexical', ...(embeddingFailure ? { fallbackReason: embeddingFailure } : {}), indexPath: path.join(directory, '.orbit', 'index', 'index.json'), updatedAt: indexed.updatedAt };
    report({ phase: 'ready', message: summary.mode === 'hybrid' ? 'Indice semantico aggiornato.' : 'Indice testuale aggiornato.', ...summary });
    return { documents: documents.map(publicDocument), summary };
  }

  function lexicalScore(queryTerms, chunkTerms, frequencies, totalChunks, averageLength) {
    const counts = new Map();
    chunkTerms.forEach(term => counts.set(term, (counts.get(term) || 0) + 1));
    let score = 0;
    for (const term of queryTerms) {
      const occurrences = counts.get(term) || 0;
      if (!occurrences) continue;
      const inverseFrequency = Math.log(1 + (totalChunks - (frequencies.get(term) || 0) + 0.5) / ((frequencies.get(term) || 0) + 0.5));
      score += inverseFrequency * occurrences * 2.2 / (occurrences + 1.2 * (0.25 + 0.75 * chunkTerms.length / Math.max(1, averageLength)));
    }
    return score;
  }

  async function retrieve(request) {
    const refreshed = await refresh(request);
    const index = documentIndexes.get(path.resolve(request.project.directory));
    const query = cleanText(request.query).slice(0, 4000);
    const queryTerms = [...new Set(terms(query))];
    const candidates = index.documents.flatMap(document => document.chunks.map(chunk => ({ document, chunk })));
    let queryVector;
    if (candidates.some(({ chunk }) => chunk.vector) && embeddings) {
      try {
        queryVector = queryVectors.get(query);
        if (!queryVector) {
          [queryVector] = await vectorize([query]);
          queryVectors.set(query, queryVector);
          if (queryVectors.size > 100) queryVectors.delete(queryVectors.keys().next().value);
        }
      } catch (error) {
        embeddingFailure ||= error.message;
      }
    }
    const frequencies = new Map();
    candidates.forEach(({ chunk }) => [...new Set(chunk.terms)].forEach(term => frequencies.set(term, (frequencies.get(term) || 0) + 1)));
    const averageLength = candidates.reduce((sum, candidate) => sum + candidate.chunk.terms.length, 0) / Math.max(1, candidates.length);
    const ranked = candidates.map(candidate => {
      const lexical = lexicalScore(queryTerms, candidate.chunk.terms, frequencies, candidates.length, averageLength);
      const semantic = queryVector && candidate.chunk.vector ? candidate.chunk.vector.reduce((sum, number, position) => sum + number * queryVector[position], 0) : 0;
      return { ...candidate, lexical, semantic };
    });
    const maxLexical = Math.max(1, ...ranked.map(candidate => candidate.lexical));
    const linkedIds = new Set(request.linkedIds || []);
    ranked.forEach(candidate => {
      candidate.score = (queryVector ? Math.max(0, candidate.semantic) * 0.7 : 0) + candidate.lexical / maxLexical * (queryVector ? 0.3 : 1) + (linkedIds.has(candidate.document.tabId) ? 0.12 : 0);
    });
    ranked.sort((first, second) => second.score - first.score);
    const limit = Math.max(1, Math.min(16, Number(request.limit) || 8));
    const selected = [];
    const perDocument = new Map();
    for (const candidate of ranked) {
      if (candidate.score <= 0 || (!candidate.lexical && candidate.semantic < 0.18) || (perDocument.get(candidate.document.id) || 0) >= 3) continue;
      selected.push(candidate);
      perDocument.set(candidate.document.id, (perDocument.get(candidate.document.id) || 0) + 1);
      if (selected.length === limit) break;
    }
    const linkedImages = ranked.filter(candidate => candidate.document.kind === 'image' && linkedIds.has(candidate.document.tabId)).slice(0, Math.min(4, limit));
    for (const candidate of linkedImages) {
      if (selected.includes(candidate)) continue;
      if (selected.length === limit) selected.pop();
      selected.unshift(candidate);
    }
    if (/preventiv|rispett|confront|avanzament|a che punto|svilupp/i.test(query) && limit >= 2) {
      for (const pattern of [/preventiv|offert|budget|costo|€|euro|scope|perimetro/i, /milestone|completat|implementat|avanzament|rilasci|consegn|svilupp|progress|todo/i]) {
        if (selected.some(candidate => pattern.test(candidate.document.title + ' ' + candidate.chunk.text))) continue;
        const candidate = ranked.find(candidate => pattern.test(candidate.document.title + ' ' + candidate.chunk.text));
        if (!candidate || selected.includes(candidate)) continue;
        if (selected.length === limit) selected.pop();
        selected.push(candidate);
      }
    }
    const sources = selected.map(({ document, chunk, score }) => ({
      id: chunk.id, title: chunk.section ? `${document.title} · ${chunk.section}` : document.title, kind: document.kind,
      ...(document.tabId ? { tabId: document.tabId } : {}), ...(document.path ? { path: document.path } : {}), ...(document.url ? { url: document.url } : {}),
      ...(chunk.page ? { page: chunk.page } : {}), lineStart: chunk.lineStart, lineEnd: chunk.lineEnd,
      text: chunk.text, score: Math.round(score * 10000) / 10000, capturedAt: document.capturedAt
    }));
    return { sources, documents: refreshed.documents, summary: { ...refreshed.summary, mode: queryVector ? 'hybrid' : 'lexical', ...(embeddingFailure ? { fallbackReason: embeddingFailure } : {}), query, retrievedSources: sources.length } };
  }

  async function stop() {
    if (extractor) await extractor.dispose();
    extractor = null;
    queryVectors.clear();
    documentIndexes.clear();
  }

  return { refresh, retrieve, stop };
}

module.exports = { createKnowledgeIndex, embeddingModel, chunkPage, terms };
