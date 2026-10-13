const fs=require('node:fs');
const path=require('node:path');
const {execFileSync}=require('node:child_process');
function version(manifest) {
  const text=/^version = "(\d+\.\d+\.\d+)"$/m.exec(manifest)?.[1];
  if(!text) throw new Error('Parser package version is required');
  return text.split('.').map(Number);
}
function assertGoldenVersion(base,current,files) {
  if(!files.some(file=>file.startsWith('dendrite-parser/tests/fixtures/expected/'))) return;
  const before=version(base),after=version(current);
  let increased=false;
  for(let i=0;i<3;i++) {
    if(after[i]!==before[i]) {increased=after[i]>before[i];break;}
  }
  if(!increased) throw new Error('Changed expected corpus/metadata requires a parser version increase and owner review');
}
module.exports={assertGoldenVersion};
if(require.main===module) {
  const base=process.env.DENDRITE_BASE_SHA;
  if(!/^[a-f0-9]{40}$/.test(base || '')) throw new Error('Exact PR base commit required');
  const root=path.resolve(__dirname,'..');
  const git=args=>execFileSync('git',args,{cwd:root,encoding:'utf8',maxBuffer:4*1024*1024});
  const files=git(['diff','--name-only',`${base}...HEAD`,'--','dendrite-parser/tests/fixtures/expected']).trim().split(/\r?\n/);
  assertGoldenVersion(git(['show',`${base}:dendrite-parser/Cargo.toml`]),fs.readFileSync(path.join(root,'dendrite-parser/Cargo.toml'),'utf8'),files);
}
