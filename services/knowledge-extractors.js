const fs = require('node:fs/promises');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { unzipSync, strFromU8 } = require('fflate');

const maxFileBytes = 32 * 1024 * 1024;
const maxTextCharacters = 800000;
const textExtensions = new Set('.txt .md .mdx .js .jsx .mjs .cjs .json .jsonl .csv .tsv .html .htm .css .scss .sass .less .xml .yaml .yml .ini .conf .config .log .sql .php .py .rb .go .rs .java .c .h .cpp .sh .ps1 .bat .cmd .vue .svelte .toml .tex .rtf .info'.split(' '));
const imageExtensions = new Set(['.jpg', '.jpeg', '.png', '.gif', '.webp', '.avif', '.bmp', '.svg', '.tiff', '.tif']);
const excludedDirectories = new Set(['node_modules', '.git', '.orbit', '.runtime', '.cache', '.ssh', '.aws', '.azure', 'screenshots', 'cache', 'dist', 'build', 'coverage']);

function isExcludedPath(filename) {
  const parts = filename.replace(/\\/g, '/').split('/').filter(Boolean);
  if (parts.some(part => excludedDirectories.has(part.toLowerCase()))) return true;
  const name = (parts.at(-1) || '').toLowerCase();
  return /^(?:\.env(?:\..*)?|\.npmrc|\.pypirc|id_rsa.*|id_ed25519.*|credentials.*|secrets?(?:[._-].*)?|auth(?:[._-].*)?|tokens?(?:[._-].*)?|.*\.(?:pem|key|p12|pfx)|package-lock\.json|yarn\.lock|pnpm-lock\.yaml|bun\.lockb?|composer\.lock|cargo\.lock|.*\.tmp)$/.test(name);
}

function cleanText(value) {
  return String(value || '').replace(/\u0000/g, '').replace(/\r\n?/g, '\n').slice(0, maxTextCharacters)
    .replace(/(\bauthorization\s*[:=]\s*["']?bearer\s+)[\w.~+/=-]+/gi, '$1[redacted]')
    .replace(/\b(?:sk-(?:ant-|proj-)?[a-zA-Z0-9_-]{16,}|gh[pousr]_[a-zA-Z0-9]{20,}|hf_[a-zA-Z0-9]{20,}|AKIA[A-Z0-9]{16})\b/g, '[redacted]')
    .replace(/([?&](?:access_token|refresh_token|id_token|api_key|apikey|token|authorization|auth|password|secret)=)[^&#\s"'<>]+/gi, '$1[redacted]')
    .replace(/(\b(?:password|passwd|secret|api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|client[_-]?secret|token)\b["']?\s*[:=]\s*)(["'])([^"'\n]*)\2/gi, '$1$2[redacted]$2')
    .replace(/(\b(?:password|passwd|secret|api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|client[_-]?secret|token)\b["']?\s*[:=]\s*)(?!["'\[])([^\s,;"'{}\]]+)/gi, '$1[redacted]')
    .trim();
}

function decodeXml(value) {
  return value.replace(/&#x([\da-f]+);/gi, (_, code) => String.fromCodePoint(parseInt(code, 16)))
    .replace(/&#(\d+);/g, (_, code) => String.fromCodePoint(Number(code)))
    .replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&amp;/g, '&');
}

function xmlText(value) {
  return decodeXml(value.replace(/<\/(?:w:p|a:p|row|p)>/g, '\n').replace(/<[^>]+>/g, ''));
}

function officeEntries(bytes, pattern) {
  let acceptedBytes = 0;
  return unzipSync(bytes, { filter: entry => {
    if (!pattern.test(entry.name) || entry.originalSize > maxFileBytes) return false;
    acceptedBytes += entry.originalSize;
    return acceptedBytes <= maxFileBytes;
  } });
}

function extractOffice(bytes, extension) {
  if (extension === '.docx') {
    const entries = officeEntries(bytes, /^word\/(?:document|header\d+|footer\d+)\.xml$/);
    return { status: 'indexed', pages: Object.entries(entries).map(([name, content]) => ({ title: name, text: xmlText(strFromU8(content)) })) };
  }
  if (extension === '.pptx') {
    const entries = officeEntries(bytes, /^ppt\/slides\/slide\d+\.xml$/);
    return { status: 'indexed', pages: Object.entries(entries).sort(([first], [second]) => first.localeCompare(second, undefined, { numeric: true })).map(([name, content]) => ({ page: Number(name.match(/slide(\d+)/)[1]), text: xmlText(strFromU8(content)) })) };
  }
  const entries = officeEntries(bytes, /^xl\/(?:sharedStrings|worksheets\/sheet\d+)\.xml$/);
  const sharedStrings = [...strFromU8(entries['xl/sharedStrings.xml'] || new Uint8Array()).matchAll(/<si(?:\s[^>]*)?>([\s\S]*?)<\/si>/g)].map(match => xmlText(match[1]));
  const pages = Object.entries(entries).filter(([name]) => name.includes('/worksheets/')).map(([name, content]) => {
    const rows = [...strFromU8(content).matchAll(/<row(?:\s[^>]*)?>([\s\S]*?)<\/row>/g)].map(row => [...row[1].matchAll(/<c\b([^>]*)>([\s\S]*?)<\/c>/g)].map(cell => {
      const value = cell[2].match(/<v>([\s\S]*?)<\/v>/)?.[1] || xmlText(cell[2]);
      return /\bt="s"/.test(cell[1]) ? sharedStrings[Number(value)] || '' : decodeXml(value);
    }).join('\t'));
    return { title: name, text: rows.join('\n') };
  });
  return { status: 'indexed', pages };
}

let pdfModule;

async function extractPdf(bytes) {
  if (!pdfModule) {
    const canvas = require('@napi-rs/canvas');
    globalThis.DOMMatrix ||= canvas.DOMMatrix;
    globalThis.ImageData ||= canvas.ImageData;
    globalThis.Path2D ||= canvas.Path2D;
    pdfModule = await import(pathToFileURL(path.resolve(__dirname, '../../vendor/pdfjs/pdf.mjs')).href);
  }
  const loadingTask = pdfModule.getDocument({
    data: new Uint8Array(bytes), useSystemFonts: false, disableFontFace: true,
    standardFontDataUrl: pathToFileURL(path.resolve(__dirname, '../../vendor/pdfjs/standard_fonts')).href + '/',
    cMapUrl: pathToFileURL(path.resolve(__dirname, '../../vendor/pdfjs/cmaps')).href + '/',
    wasmUrl: pathToFileURL(path.resolve(__dirname, '../../vendor/pdfjs/wasm')).href + '/',
    verbosity: 0
  });
  const document = await loadingTask.promise;
  try {
    const pages = [];
    for (let number = 1; number <= Math.min(document.numPages, 500); number += 1) {
      const page = await document.getPage(number);
      const content = await page.getTextContent();
      let previousY;
      let text = '';
      for (const item of content.items) {
        if (!('str' in item)) continue;
        if (previousY !== undefined && Math.abs(item.transform[5] - previousY) > 3 && !text.endsWith('\n')) text += '\n';
        text += item.str + (item.hasEOL ? '\n' : ' ');
        previousY = item.transform[5];
      }
      pages.push({ page: number, text: cleanText(text) });
      page.cleanup();
    }
    const hasText = pages.some(page => page.text);
    return { status: hasText ? 'indexed' : 'needs-ocr', pages, ...(hasText ? {} : { error: 'PDF senza testo estraibile: serve OCR.' }) };
  } finally {
    await loadingTask.destroy();
  }
}

async function extractFile(filename, { chatId } = {}) {
  const extension = path.extname(filename).toLowerCase();
  const statistics = await fs.stat(filename);
  if (statistics.size > maxFileBytes) return { status: 'too-large', error: 'File oltre il limite di 32 MB.', pages: [] };
  if (imageExtensions.has(extension)) return { kind: 'image', status: 'metadata-only', error: 'Contenuto visivo non analizzato; OCR non attivo.', pages: [{ text: `Immagine: ${path.basename(filename)}\nFormato: ${extension.slice(1)}\nDimensione: ${statistics.size} byte.` }] };
  if (extension === '.pdf') return extractPdf(await fs.readFile(filename));
  if (['.docx', '.pptx', '.xlsx'].includes(extension)) return extractOffice(new Uint8Array(await fs.readFile(filename)), extension);
  if (extension === '.chat') {
    const chat = JSON.parse(await fs.readFile(filename, 'utf8'));
    if (chat.id === chatId) return { status: 'excluded', pages: [] };
    return { status: 'indexed', chatId: chat.id, pages: [{ text: (chat.messages || []).map(message => `${message.role || 'message'}: ${message.text || ''}`).join('\n\n') }] };
  }
  if (!textExtensions.has(extension) && extension !== '') return { status: 'unsupported', error: `Formato ${extension || 'binario'} non ancora estraibile.`, pages: [] };
  const bytes = await fs.readFile(filename);
  if (bytes.subarray(0, 4096).includes(0)) return { status: 'unsupported', error: 'Contenuto binario non estraibile.', pages: [] };
  let text = bytes.toString('utf8');
  if (['.html', '.htm'].includes(extension)) text = decodeXml(text.replace(/<(script|style|noscript)\b[^>]*>[\s\S]*?<\/\1>/gi, '').replace(/<\/(p|div|h[1-6]|li|tr)>/gi, '\n').replace(/<[^>]+>/g, ' '));
  if (extension === '.rtf') text = text.replace(/\\[a-z]+-?\d* ?|[{}]/g, '');
  return { status: 'indexed', pages: [{ text: cleanText(text) }] };
}

async function listProjectFiles(directory) {
  const files = [];
  async function walk(current) {
    const entries = await fs.readdir(current, { withFileTypes: true });
    for (const entry of entries.sort((first, second) => first.name.localeCompare(second.name))) {
      if (files.length >= 3000) return;
      const filename = path.join(current, entry.name);
      const relative = path.relative(directory, filename);
      if (isExcludedPath(relative) || entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) await walk(filename);
      else if (entry.isFile() && !['project.info', 'workspace.info'].includes(entry.name)) files.push(filename);
    }
  }
  await walk(directory);
  return files;
}

module.exports = { cleanText, extractFile, isExcludedPath, listProjectFiles };
