const test = require('node:test');
const assert = require('node:assert/strict');
const selection = require('./source-selection.cjs');
const file = () => new File(['Synthetic gardening note'], 'note.txt', {type:'text/plain', lastModified:1});

test('ordinary files retain identity and folder names are display-only', async () => {
  const original = file();
  assert.equal(selection.normalizeSelectedUploadFiles([original])[0], original);
  const relative = file();
  Object.defineProperty(relative, 'webkitRelativePath', {value:'Folder/Notes/note.txt'});
  const normalized = selection.normalizeSelectedUploadFiles([relative])[0];
  assert.equal(normalized.name, 'Folder ∕ Notes ∕ note.txt');
  assert.equal(await normalized.text(), await original.text());
  assert.equal(selection.wrapFileWithFlattenedName(original, '/Folder/.././Notes/note.txt').name,
    'Folder ∕ Notes ∕ note.txt');
});
test('directory drops read every page', async () => {
  let page = 0;
  const child = {isFile:true, fullPath:'/Folder/note.txt', file(resolve){resolve(file());}};
  const folder = {isDirectory:true, createReader(){return {readEntries(resolve){
    resolve(++page === 1 ? Array(100).fill(child) : page === 2 ? [child] : []);
  }};}};
  const rows = await selection.collectDroppedUploadFiles({items:[{kind:'file', webkitGetAsEntry:()=>folder}], files:[]});
  assert.equal(rows.length, 101);
  assert.equal(page, 3);
  assert.equal(rows[100].name, 'Folder ∕ note.txt');
});
test('modern handles and mixed fallback files are retained', async () => {
  const original = file();
  const handle = {kind:'directory', name:'Folder', async *values(){
    yield {kind:'file', name:'note.txt', getFile:async()=>original};
  }};
  assert.equal((await selection.collectDroppedUploadFiles({items:[{kind:'file', getAsFileSystemHandle:async()=>handle}], files:[]}))[0].name,
    'Folder ∕ note.txt');
  assert.equal((await selection.collectDroppedUploadFiles({items:[], files:[original]}))[0], original);
  const child = {isFile:true, fullPath:'/Folder/note.txt', file(resolve){resolve(original);}};
  assert.equal((await selection.collectDroppedUploadFiles({items:[
    {kind:'file', webkitGetAsEntry:()=>child}, {kind:'file', getAsFile:()=>original}
  ], files:[]})).length, 2);
});
test('empty folder placeholders, oversized batches and deep recursion are bounded', async () => {
  await assert.rejects(selection.collectDroppedUploadFiles({items:[], files:[new File([], 'Folder')]}), /Choose folders/);
  const child = {isFile:true, fullPath:'/Folder/note.txt', file(resolve){resolve(file());}};
  let once = false;
  const huge = {isDirectory:true, createReader(){return {readEntries(resolve){
    resolve(once ? [] : Array(501).fill(child)); once = true;
  }};}};
  await assert.rejects(selection.collectDroppedUploadFiles({items:[{kind:'file', webkitGetAsEntry:()=>huge}], files:[]}), /500 files/);
  const deep = n => n ? {isDirectory:true, createReader(){let visited=false; return {readEntries(resolve){
    resolve(visited ? [] : [deep(n-1)]); visited=true;
  }};}} : child;
  await assert.rejects(selection.collectDroppedUploadFiles({items:[{kind:'file', webkitGetAsEntry:()=>deep(34)}], files:[]}), /deeply nested/);
});
