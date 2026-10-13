// Prepare distribution notices from locked Cargo metadata; never compile or publish.
const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
const {execFileSync}=require('node:child_process');
const ROOT=path.resolve(__dirname,'..');
const hash=bytes=>crypto.createHash('sha256').update(bytes).digest('hex');
const read=file=>fs.readFileSync(file,'utf8').replace(/\r\n/g,'\n').replace(/[ \t]+$/gm,'');
const LOCKS=['Cargo.lock','src-tauri/Cargo.lock'];
const preference=['Apache-2.0','MIT','Zlib','BSD-3-Clause','BSD-2-Clause','BSD-1-Clause','ISC','0BSD','MIT-0','BSL-1.0','CC0-1.0','Unlicense'];

function selectLicenses(expression) {
  const tokens=expression.replace(/\s*\/\s*/g,' OR ').match(/[A-Za-z0-9.-]+|[()]/g)||[];
  let index=0;
  const atom=()=>{
    if(tokens[index]==='('){index++;const value=or();if(tokens[index++]!==')')throw Error('Invalid license expression');return value;}
    const id=tokens[index++];if(!id||['AND','OR','WITH',')'].includes(id))throw Error('Invalid license expression');
    if(tokens[index]==='WITH'){index++;const exception=tokens[index++];if(!exception)throw Error('Missing license exception');return [id,exception];}
    return [id];
  };
  const and=()=>{let value=atom();while(tokens[index]==='AND'){index++;value=[...value,...atom()];}return value;};
  const rank=ids=>ids.reduce((sum,id)=>sum+(preference.indexOf(id)<0?100:preference.indexOf(id)),0);
  const or=()=>{let value=and();while(tokens[index]==='OR'){index++;const alternative=and();if(rank(alternative)<rank(value))value=alternative;}return value;};
  const chosen=or();if(index!==tokens.length)throw Error('Unsupported license expression');
  return [...new Set(chosen)].sort();
}

function crateNotices(root) {
  const notices=[],copyright=new Set();
  const walk=(directory,depth=0)=>{
    for(const entry of fs.readdirSync(directory,{withFileTypes:true}).sort((a,b)=>a.name<b.name?-1:a.name>b.name?1:0)) {
      if(entry.isSymbolicLink())continue;
      const file=path.join(directory,entry.name),relative=path.relative(root,file).split(path.sep).join('/');
      if(entry.isDirectory()){
        if(!['.git','target','tests','examples','benches'].includes(entry.name))walk(file,depth+1);
      } else if(entry.isFile()) {
        if(/^(?:licen[sc]e|copying|notice|copyright)(?:$|[._-])/i.test(entry.name) && fs.statSync(file).size<4*1024*1024) {
          notices.push({file:relative,text:read(file)});
        } else if(/\.(?:rs|c|h|cpp|hpp|py)$/.test(entry.name)) {
          const fd=fs.openSync(file,'r'),buffer=Buffer.alloc(8192);
          let length;try{length=fs.readSync(fd,buffer,0,buffer.length,0);}finally{fs.closeSync(fd);}
          for(const line of buffer.subarray(0,length).toString('utf8').split(/\r?\n/)) {
            if(/^\s*(?:\/\/|\/\*|\*|#)\s*copyright\b/i.test(line) && line.length<600)copyright.add(line.replace(/^\s*(?:\/\/|\/\*|\*|#)\s*/,'').trim());
          }
        }
      }
    }
  };
  walk(root);
  return {notices,copyright:[...copyright].sort()};
}

function metadata(root,manifest) {
  return JSON.parse(execFileSync('cargo',['metadata','--locked','--offline','--format-version','1','--manifest-path',manifest],
    {cwd:root,encoding:'utf8',maxBuffer:32*1024*1024})).packages;
}
function prepareLicenses(root=ROOT,packages) {
  packages ||= [...metadata(root,'Cargo.toml'),...metadata(root,'src-tauri/Cargo.toml')];
  const registry=[...new Map(packages.filter(p=>p.source?.startsWith('registry+')).map(p=>[`${p.name}@${p.version}`,p])).values()]
    .sort((a,b)=>a.name<b.name?-1:a.name>b.name?1:a.version<b.version?-1:a.version>b.version?1:0);
  const output=path.join(root,'distribution/licenses');fs.mkdirSync(output,{recursive:true});
  const texts=new Set(),components=[],sections=[],noticeTexts=new Map();
  for(const pkg of registry) {
    if(!pkg.license)throw Error(`Missing license declaration: ${pkg.name}@${pkg.version}`);
    const selected=selectLicenses(pkg.license);selected.forEach(id=>texts.add(id));
    const collected=crateNotices(path.dirname(pkg.manifest_path));
    const source=`https://static.crates.io/crates/${pkg.name}/${pkg.name}-${pkg.version}.crate`;
    const noticeRefs=collected.notices.map(n=>{const id=hash(n.text);noticeTexts.set(id,n.text);return {file:n.file,sha256:id};});
    const component={name:pkg.name,version:pkg.version,declaredLicense:pkg.license,selectedLicenses:selected,
      authors:pkg.authors||[],source,noticeFiles:collected.notices.map(n=>n.file),noticeRefs,copyright:collected.copyright};
    components.push(component);
    sections.push(`${pkg.name} ${pkg.version}\nDeclared license: ${pkg.license}\nSelected terms: ${selected.join(' AND ')}\nSource: ${source}\nDeclared authors: ${(pkg.authors||[]).join('; ')||'See source archive'}\n`
      +collected.copyright.join('\n')+'\n'+noticeRefs.map(n=>`Original notice ${n.file}: ${n.sha256}`).join('\n'));
  }
  const header='Synthius Dendrite third-party notices\n\nThis inventory conservatively includes locked Cargo metadata across targets and optional/build/development dependencies. Inclusion does not assert every component is linked into each binary. Synthius application code remains Apache-2.0; these components retain their own terms.\n\nMPL-2.0 source availability: unmodified source for the listed MPL components is available at each exact-version Source URL below. These source archives retain their MPL notices. No additional restriction is placed on those files.\n\n';
  const standards=[...texts].sort().map(id=>{
    const file=path.join(root,'distribution/license-texts',id+'.txt');
    if(!fs.existsSync(file))throw Error(`Add the standard license text: ${id}`);
    return `\n=== ${id} ===\n${read(file)}`;
  }).join('');
  fs.writeFileSync(path.join(output,'LICENSE.txt'),read(path.join(root,'LICENSE')));
  const originals=[...noticeTexts].sort(([a],[b])=>a<b?-1:a>b?1:0).map(([id,text])=>`\n=== Original notice ${id} ===\n${text}`).join('');
  fs.writeFileSync(path.join(output,'THIRD_PARTY_NOTICES.txt'),header+sections.join('\n\n========================================\n\n')+'\n\nOriginal notice texts (identical copies stored once)\n'+originals+'\n\nStandard license texts\n'+standards);
  fs.writeFileSync(path.join(output,'components.json'),JSON.stringify({components},null,2)+'\n');
  const receipt={lockfiles:Object.fromEntries(LOCKS.map(name=>[name,hash(read(path.join(root,name)))])),
    applicationLicense:hash(read(path.join(root,'LICENSE'))),
    files:Object.fromEntries(['LICENSE.txt','THIRD_PARTY_NOTICES.txt','components.json'].map(name=>[name,hash(read(path.join(output,name)))]))};
  fs.writeFileSync(path.join(output,'manifest.json'),JSON.stringify(receipt,null,2)+'\n');
  fs.writeFileSync(path.join(root,'ui/LICENSE'),read(path.join(root,'LICENSE')));
  return components;
}
function verifyLicenses(root=ROOT) {
  const output=path.join(root,'distribution/licenses'),receipt=JSON.parse(read(path.join(output,'manifest.json')));
  for(const name of LOCKS)if(receipt.lockfiles[name]!==hash(read(path.join(root,name))))throw Error('Dependency locks changed; run npm run licenses');
  if(receipt.applicationLicense!==hash(read(path.join(root,'LICENSE')))
    || read(path.join(root,'ui/LICENSE'))!==read(path.join(root,'LICENSE')))throw Error('Application license copies are stale');
  for(const name of ['LICENSE.txt','THIRD_PARTY_NOTICES.txt','components.json'])
    if(receipt.files[name]!==hash(read(path.join(output,name))))throw Error(`License bundle changed: ${name}`);
  const catalogue=JSON.parse(read(path.join(output,'components.json'))).components;
  if(!Array.isArray(catalogue)||!catalogue.length||catalogue.some(p=>!p.selectedLicenses?.length||p.source!==`https://static.crates.io/crates/${p.name}/${p.name}-${p.version}.crate`))throw Error('Incomplete versioned license/source inventory');
  return output;
}
function copyLicenses(destination,root=ROOT) {
  const source=verifyLicenses(root);fs.cpSync(source,path.join(destination,'licenses'),{recursive:true});
  return 'licenses';
}
module.exports={selectLicenses,crateNotices,prepareLicenses,verifyLicenses,copyLicenses};
if(require.main===module){
  if(process.argv[2]==='prepare')console.log(`Prepared notices for ${prepareLicenses().length} locked registry packages`);
  else if(process.argv[2]==='check'){verifyLicenses();console.log('License bundle and lockfile receipts passed');}
  else throw Error('Use licenses.cjs prepare or check');
}
