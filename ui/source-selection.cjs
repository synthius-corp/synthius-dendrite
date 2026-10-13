/* Shared by the web Sources workflow and Dendrite's native WebView (UMD). */
(function(root) {
  const MAX_FILES = 500, MAX_DEPTH = 32;
  const DIRECTORY_INPUT_ATTRIBUTES = { webkitdirectory: '', directory: '', mozdirectory: '' };
  function wrapFileWithFlattenedName(file, pathLike) {
    const parts = String(pathLike || file.name).replace(/\\/g, '/').split('/').map(p => p.trim())
      .filter(p => p && p !== '.' && p !== '..');
    const name = parts.join(' ∕ ') || file.name;
    return name === file.name && !file.webkitRelativePath ? file : new File([file], name, { type: file.type, lastModified: file.lastModified });
  }
  function normalizeSelectedUploadFiles(files) {
    return files.map(file => wrapFileWithFlattenedName(file, file.webkitRelativePath || file.name));
  }
  async function collectDroppedUploadFiles(transfer) {
    const items = Array.from(transfer.items || []).filter(item => item.kind === 'file');
    // Capture handles/entries while the drop's data store is still readable.
    const captured = items.map((item, index) => ({
      entry: item.webkitGetAsEntry?.(),
      handle: item.getAsFileSystemHandle?.().catch(() => null),
      file: item.getAsFile?.() || transfer.files?.[index],
    }));
    const files = Array.from(transfer.files || []);
    const result = []; let visited = 0;
    function check(depth) {
      if (depth > MAX_DEPTH || ++visited > 10000) throw new Error('This folder is too large or too deeply nested. Choose a smaller folder.');
      if (result.length >= MAX_FILES) throw new Error('Select up to 500 files per batch.');
    }
    async function entry(e, depth) {
      check(depth);
      if (e.isFile) {
        const file = await new Promise((resolve, reject) => e.file(resolve, reject));
        result.push(wrapFileWithFlattenedName(file, e.fullPath || file.name)); return;
      }
      if (!e.isDirectory) return;
      const reader = e.createReader();
      while (true) {
        const page = await new Promise((resolve, reject) => reader.readEntries(resolve, reject));
        if (!page.length) break;
        for (const child of page) await entry(child, depth + 1);
      }
    }
    async function handle(h, parent, depth) {
      check(depth); const path = parent ? `${parent}/${h.name}` : h.name;
      if (h.kind === 'file') { result.push(wrapFileWithFlattenedName(await h.getFile(), path)); return; }
      if (h.kind !== 'directory') return;
      if (h.values) { for await (const child of h.values()) await handle(child, path, depth + 1); }
      else if (h.entries) { for await (const [, child] of h.entries()) await handle(child, path, depth + 1); }
      else throw new Error('Use Choose folders to read this folder.');
    }
    // WebKit entries cover Windows WebView2 and macOS WKWebView; handles are
    // the standards-based fallback. Read every page, not only the first 100.
    if (captured.some(item => item.entry || item.handle || item.file)) {
      for (const item of captured) {
        const h = item.entry ? null : await item.handle;
        if (item.entry) await entry(item.entry, 0);
        else if (h) await handle(h, '', 0);
        else if (item.file) {
          check(0);
          if (isFolderPlaceholder(item.file)) throw new Error('Use Choose folders to read the files inside this folder.');
          result.push(wrapFileWithFlattenedName(item.file, item.file.webkitRelativePath || item.file.name));
        } else throw new Error('This dropped item could not be read. Use Choose files or Choose folders.');
      }
      return result;
    }
    if (files.some(isFolderPlaceholder)) {
      throw new Error('Use Choose folders to read the files inside this folder.');
    }
    return normalizeSelectedUploadFiles(files);
  }
  function isFolderPlaceholder(file) {
    return file.size === 0 && !file.type && !/\.[^./\\]+$/.test(file.name);
  }
  const api = { DIRECTORY_INPUT_ATTRIBUTES, normalizeSelectedUploadFiles, collectDroppedUploadFiles, wrapFileWithFlattenedName };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.SynthiusSourceSelection = api;
})(globalThis);
