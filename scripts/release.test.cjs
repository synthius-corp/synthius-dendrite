const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const os=require('node:os');
const path=require('node:path');
const crypto=require('node:crypto');
const {validateVersion,verifyArtifacts,createDraft,assertReleaseCanBeCreated,publishFiles,releaseRepository,assertPrivatePreviewRepository,verifyFullRelease,publishFullRelease,run,digest}=require('./release.cjs');
const {profiles}=require('./channels.cjs');
test('release tag matches a stable exact version',()=>{
  assert.equal(validateVersion('v2.1.0','2.1.0'),'2.1.0');
  for(const tag of ['main','v2.0.0','v2.1.0-beta','v../../x']) assert.throws(()=>validateVersion(tag,'2.1.0'));
});
test('full and desktop release tags share explicit preview and production rules',()=>{
  assert.equal(validateVersion('v2.1.4-preview.2','2.1.4','preview'),'2.1.4-preview.2');
  for(const [tag,channel] of [['v2.1.4-preview.2','production'],['v2.1.4','preview'],
    ['v2.1.4-preview.0','preview'],['v2.1.4-preview.1','local-dev'],['v2.1.5-preview.1','preview']]) {
    assert.throws(()=>validateVersion(tag,'2.1.4',channel));
  }
});
function fullStage(channel) {
  const stage=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-full-release-'));
  const base=JSON.parse(fs.readFileSync(path.join(__dirname,'../src-tauri/tauri.conf.json'),'utf8')).version;
  const tag=`v${base}${channel==='preview'?'-preview.2':''}`,commit='a'.repeat(40);
  const write=(name,value)=>{const bytes=typeof value==='string'?value:JSON.stringify(value);
    fs.writeFileSync(path.join(stage,name),bytes);fs.writeFileSync(path.join(stage,name+'.sha256'),`${digest(bytes)}  ${name}\n`);};
  for(const target of ['linux-x86_64','windows-x86_64','macos-arm64','macos-x86_64']) {
    const names=[`dendrite-parser-${target}.tar.gz`,`sbom-${target}.cdx.json`];
    names.push(target==='linux-x86_64'?'synthius-dendrite-source-selection-1.0.0.tgz':
      `${channel==='preview'?'SynthiusDendritePreview':'SynthiusDendrite'}-${tag.slice(1)}-${target}${target.startsWith('windows')?'.exe':'.dmg'}`);
    names.forEach(name=>write(name,`Synthetic artifact ${name}`));
    write(`provenance-${target}.json`,{repository:'https://github.com/synthius-corp/synthius-dendrite',
      sourceCommit:commit,tag,version:tag.slice(1),target,channel,desktopOnly:false,
      corpusSchema:'synthius-corpus/v1',automaticUpdater:false,
      ...(target==='linux-x86_64'?{}:{desktopProfile:profiles[channel]}),
      artifacts:names.map(name=>({name,sha256:digest(fs.readFileSync(path.join(stage,name)))}))});
  }
  return {stage,tag,commit,write};
}
test('full packages verify all four targets and reject mixed profiles, sources and artifacts',()=>{
  for(const channel of ['preview','production']) {
    const h=fullStage(channel);
    try {
      const manifest=verifyFullRelease(h.stage,h.tag,channel,h.commit);
      assert.equal(manifest.channel,channel);assert.equal(Object.keys(manifest.downloads).length,3);
      assert.ok(Object.values(manifest.downloads).every(link=>link.url.includes(`/download/${h.tag}/`)));
      assert.throws(()=>verifyFullRelease(h.stage,h.tag,channel,'b'.repeat(40)),/provenance mismatch/);
      const name='provenance-linux-x86_64.json', original=JSON.parse(fs.readFileSync(path.join(h.stage,name),'utf8'));
      for(const change of [{channel:channel==='preview'?'production':'preview'},{desktopOnly:true},{corpusSchema:'unknown'}]) {
        h.write(name,{...original,...change});assert.throws(()=>verifyFullRelease(h.stage,h.tag,channel,h.commit),/provenance mismatch/);
      }
      h.write(name,original);
      const desktopName='provenance-windows-x86_64.json', desktop=JSON.parse(fs.readFileSync(path.join(h.stage,desktopName),'utf8'));
      h.write(desktopName,{...desktop,desktopProfile:profiles[channel==='preview'?'production':'preview']});
      assert.throws(()=>verifyFullRelease(h.stage,h.tag,channel,h.commit),/provenance mismatch/);h.write(desktopName,desktop);
      h.write('dendrite-parser-linux-x86_64.tar.gz','changed bytes with a newly forged adjacent checksum');
      assert.throws(()=>verifyFullRelease(h.stage,h.tag,channel,h.commit),/Provenance artifact mismatch/);
    } finally {fs.rmSync(h.stage,{recursive:true,force:true});}
  }
});
test('full packaging rejects incomplete targets and unexpected channel manifests',()=>{
  for(const missing of ['sbom-macos-arm64.cdx.json','dendrite-parser-windows-x86_64.tar.gz','synthius-dendrite-source-selection-1.0.0.tgz']) {
    const h=fullStage('preview');
    try {fs.unlinkSync(path.join(h.stage,missing));fs.unlinkSync(path.join(h.stage,missing+'.sha256'));assert.throws(()=>verifyFullRelease(h.stage,h.tag,'preview',h.commit));}
    finally {fs.rmSync(h.stage,{recursive:true,force:true});}
  }
  const h=fullStage('preview');
  try {
    h.write('manifest-production.json',{});
    assert.throws(()=>verifyFullRelease(h.stage,h.tag,'preview',h.commit),/mixed release channels/);
  } finally {fs.rmSync(h.stage,{recursive:true,force:true});}
});
test('full preview publication stays a prerelease and never becomes production latest',()=>{
  for(const channel of ['preview','production']) {
    const h=fullStage(channel),calls=[];let current;
    const execute=(program,args)=>{
      calls.push(args);
      if(args[1]==='repos/synthius-corp/synthius-dendrite-private-archive')return JSON.stringify({id:1413068483,full_name:'synthius-corp/synthius-dendrite-private-archive',private:true});
      if(args[1]?.includes('releases?'))return '[]';
      if(args[1]?.includes('matching-refs'))return JSON.stringify([{ref:`refs/tags/${h.tag}`,object:{type:'commit',sha:h.commit}}]);
      if(args.includes('POST')) {current={id:123,tag_name:h.tag,target_commitish:h.commit,draft:true,prerelease:channel==='preview',assets:[]};return JSON.stringify(current);}
      if(args[0]==='release') {current.assets=fs.readdirSync(h.stage).map(name=>({name,state:'uploaded',
        size:fs.statSync(path.join(h.stage,name)).size,digest:`sha256:${digest(fs.readFileSync(path.join(h.stage,name)))}`}));return '';}
      if(args.includes('PATCH'))return JSON.stringify({...current,draft:false});
      if(args[1].endsWith('/releases/123'))return JSON.stringify(current);
      throw Error('Unexpected publication call');
    };
    try {
      publishFullRelease(h.stage,h.tag,channel,h.commit,{execute});
      const repo=releaseRepository(channel);
      for(const call of calls) {
        if(call[0]==='api') assert.ok(call[1].startsWith(`repos/${repo}`));
        else if(call[0]==='release') assert.equal(call[call.indexOf('--repo')+1],repo);
      }
      for(const call of calls.filter(args=>args.includes('POST')||args.includes('PATCH')))assert.ok(call.includes(`make_latest=${channel==='preview'?'false':'true'}`));
      assert.ok(calls.find(args=>args.includes('POST')).includes(`prerelease=${channel==='preview'}`));
      const name=`manifest-${channel}.json`;
      assert.equal(JSON.parse(fs.readFileSync(path.join(h.stage,name),'utf8')).latest.releaseTag,h.tag);
      assert.equal(verifyFullRelease(h.stage,h.tag,channel,h.commit).channel,channel,'retained artifacts can be verified without rebuilding');
    } finally {fs.rmSync(h.stage,{recursive:true,force:true});}
  }
});
test('full hosted workflow is production-only and isolates uploaded/downloaded artifacts',()=>{
  const source=fs.readFileSync(path.join(__dirname,'../.github/workflows/package.yml'),'utf8');
  assert.match(source,/SYNTHIUS_RELEASE_CHANNEL: production/);
  assert.match(source,/pattern: full-production-\*/);
  assert.match(source,/name: full-production-\$\{\{ matrix.target \}\}/);
  assert.doesNotMatch(source,/DENDRITE_PREVIEW_SUPABASE_PUBLIC_KEY|options: \[preview, production\]|inputs\.channel/);
  const desktop=fs.readFileSync(path.join(__dirname,'desktop.cjs'),'utf8');
  assert.match(desktop,/'ui\/telegram.test.cjs'/,'desktop release validation includes first-use consent tests');
});
test('desktop version is synchronized across configuration and dependency lock',()=>{
  const root=path.join(__dirname,'..');
  const version=JSON.parse(fs.readFileSync(path.join(root,'src-tauri/tauri.conf.json'),'utf8')).version;
  const manifest=fs.readFileSync(path.join(root,'src-tauri/Cargo.toml'),'utf8');
  const lock=fs.readFileSync(path.join(root,'src-tauri/Cargo.lock'),'utf8');
  assert.equal(/^version = "([^"]+)"$/m.exec(manifest)[1],version);
  assert.equal(/name = "synthius-dendrite"\r?\nversion = "([^"]+)"/.exec(lock)[1],version);
});
test('all release assets require exact adjacent digests',()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-release-test-'));
  try {
    assert.throws(()=>verifyArtifacts(dir));
    fs.writeFileSync(path.join(dir,'orphan.sha256'),'not an artifact');
    assert.throws(()=>verifyArtifacts(dir),/Orphan/);fs.unlinkSync(path.join(dir,'orphan.sha256'));
    fs.writeFileSync(path.join(dir,'synthetic.txt'),'synthetic');
    assert.throws(()=>verifyArtifacts(dir));
    const hash=crypto.createHash('sha256').update('synthetic').digest('hex');
    fs.writeFileSync(path.join(dir,'synthetic.txt.sha256'),`${hash}  synthetic.txt\n`);
    assert.deepEqual(verifyArtifacts(dir),['synthetic.txt']);
    fs.writeFileSync(path.join(dir,'synthetic.txt'),'modified');
    assert.throws(()=>verifyArtifacts(dir),/mismatch/);
  } finally {fs.rmSync(dir,{recursive:true,force:true});}
});
test('release and validation actions use reviewed supported runtimes',()=>{
  const root=path.join(__dirname,'..');
  const pkg=JSON.parse(fs.readFileSync(path.join(root,'package.json'),'utf8'));
  const lock=JSON.parse(fs.readFileSync(path.join(root,'package-lock.json'),'utf8'));
  assert.equal(pkg.engines.node,'24.x');
  assert.equal(lock.packages[''].engines.node,pkg.engines.node);
  assert.equal(fs.readFileSync(path.join(root,'.nvmrc'),'utf8').trim(),'24');
  const pins={
    'actions/checkout':'3d3c42e5aac5ba805825da76410c181273ba90b1',
    'actions/setup-node':'949feb2413d6458794dcd2491c4babbbce0c15c1',
    'actions/upload-artifact':'cf430e030ddbb5b0abf93d22962f4752f3646cd9',
    'actions/download-artifact':'9000827ccba6bdab643e8b6fd33ac0654aef8333',
    'Swatinem/rust-cache':'6323deb102c322ba6fcbdcafc7e3dddab59af2b6',
  };
  for(const file of ['ci.yml','package.yml','desktop.yml']) {
    const text=fs.readFileSync(path.join(__dirname,'../.github/workflows',file),'utf8');
    for(const match of text.matchAll(/uses:\s*([\w/-]+)@([a-f0-9]+)/g)) {
      assert.equal(match[2].length,40,'Every external action stays commit-pinned');
      if(pins[match[1]]) assert.equal(match[2],pins[match[1]]);
    }
    assert.doesNotMatch(text,/ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION|macos-14/);
    assert.match(text,/node-version: '24'/);
  }
});
test('publication-only failures reuse verified artifacts instead of rebuilding installers',()=>{
  const guide=fs.readFileSync(path.join(__dirname,'../RELEASING.md'),'utf8');
  assert.match(guide,/reuse their original artifacts after checksum\/provenance verification/);
  assert.match(guide,/rerun retains the\s+original commit and workflow/);
  assert.match(guide,/Do not bump a version, recreate installers/);
});
test('an unpublished draft directs publication recovery, not a version bump',()=>{
  assertReleaseCanBeCreated([],'v2.1.3');
  assertReleaseCanBeCreated([{tag_name:'v2.1.2',draft:true}],'v2.1.3');
  assert.throws(()=>assertReleaseCanBeCreated([{tag_name:'v2.1.3',draft:true}],'v2.1.3'),/original verified artifacts.*Do not rebuild installers or bump the version/);
  assert.throws(()=>assertReleaseCanBeCreated([{tag_name:'v2.1.3',draft:false}],'v2.1.3'),/Published release is immutable/);
});
test('desktop dependency metadata can exceed the Node default buffer',()=>{
  const output=run(process.execPath,['-e','process.stdout.write("x".repeat(2*1024*1024))']);
  assert.equal(output.length,2*1024*1024);
});
test('desktop build passes locked Cargo arguments without Windows shim rewriting',()=>{
  const workflow=fs.readFileSync(path.join(__dirname,'../.github/workflows/package.yml'),'utf8');
  assert.match(workflow,/name: Build desktop installer\n\s+if: matrix.desktop\n\s+shell: bash/);
  assert.match(workflow,/node \.\.\/scripts\/desktop.cjs build/);
  const builder=fs.readFileSync(path.join(__dirname,'desktop.cjs'),'utf8');
  assert.match(builder,/node_modules\/@tauri-apps\/cli\/tauri.js/);
  assert.match(builder,/'--','--locked'/);
});
test('draft creation retains the returned release ID for verification and publication',()=>{
  const calls=[];
  const execute=(program,args)=>{calls.push([program,args]);return JSON.stringify({id:123,tag_name:'v2.1.3',draft:true});};
  const draft=createDraft(execute,'synthius-corp/synthius-dendrite','v2.1.3','a'.repeat(40));
  assert.equal(draft.id,123);
  assert.equal(calls[0][0],'gh');
  assert.ok(calls[0][1].includes('POST'));
  assert.ok(calls[0][1].includes('draft=true'));
  assert.throws(()=>createDraft(()=>JSON.stringify({id:0,tag_name:'v2.1.3',draft:true}), 'repo','v2.1.3','a'.repeat(40)));
  const source=fs.readFileSync(path.join(__dirname,'release.cjs'),'utf8');
  assert.doesNotMatch(source,/releases\/tags\//);
  assert.match(source,/releases\/\$\{draft.id\}/);
});

test('publication recovery resumes only an exact draft and uploads only missing assets',()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-publish-recovery-'));
  const tag='v2.1.4-preview.1',commit='a'.repeat(40);
  fs.writeFileSync(path.join(dir,'synthetic.txt'),'synthetic');
  fs.writeFileSync(path.join(dir,'synthetic.txt.sha256'),`${digest('synthetic')}  synthetic.txt\n`);
  const assets=fs.readdirSync(dir).map(name=>({name,state:'uploaded',size:fs.statSync(path.join(dir,name)).size,digest:`sha256:${digest(fs.readFileSync(path.join(dir,name)))}`}));
  const draft={id:123,tag_name:tag,target_commitish:commit,draft:true,prerelease:true,assets:[assets[0]]};
  const calls=[];
  let resumed=structuredClone(draft);
  const execute=(program,args)=>{
    calls.push(args);
    if(args[1]==='repos/synthius-corp/synthius-dendrite-private-archive')return JSON.stringify({id:1413068483,full_name:'synthius-corp/synthius-dendrite-private-archive',private:true});
    if(args[0]==='release') {resumed.assets=assets;return '';}
    if(args[1].includes('releases?'))return JSON.stringify([resumed]);
    if(args[1].includes('matching-refs'))return JSON.stringify([{ref:`refs/tags/${tag}`,object:{type:'commit',sha:commit}}]);
    if(args.includes('PATCH'))return JSON.stringify({...resumed,draft:false});
    if(args[1].endsWith('releases/123'))return JSON.stringify(resumed);
    throw Error('Unexpected publication call');
  };
  try {
    publishFiles(dir,tag,commit,{execute,resumeDraft:true,prerelease:true});
    const upload=calls.find(args=>args[0]==='release');
    assert.deepEqual(upload.slice(5),[path.join(dir,assets[1].name)]);
    assert.equal(calls.filter(args=>args.includes('POST')).length,0);
    assert.equal(calls.filter(args=>args.includes('PATCH')).length,1);
    for(const change of [{target_commitish:'b'.repeat(40)},{prerelease:false},{assets:[{...assets[0],digest:'sha256:'+'0'.repeat(64)}]}]) {
      resumed={...structuredClone(draft),...change};calls.length=0;
      assert.throws(()=>publishFiles(dir,tag,commit,{execute,resumeDraft:true,prerelease:true}),/mismatch/);
      assert.ok(!calls.some(args=>args[0]==='release'||args.includes('PATCH')));
    }
    resumed={...structuredClone(draft),draft:false};
    assert.throws(()=>publishFiles(dir,tag,commit,{execute,resumeDraft:true,prerelease:true}),/Published release is immutable/);
  } finally {fs.rmSync(dir,{recursive:true,force:true});}
});

test('Preview destinations are private and cannot silently switch to a public repository',()=>{
  const metadata={id:1413068483,full_name:'synthius-corp/synthius-dendrite-private-archive',private:true};
  assert.equal(releaseRepository('production'),'synthius-corp/synthius-dendrite');
  assert.equal(releaseRepository('preview'),metadata.full_name);
  assert.throws(()=>releaseRepository('unknown'));
  assertPrivatePreviewRepository(()=>JSON.stringify(metadata));
  for(const change of [{private:false},{id:1417119504},{full_name:'attacker/repo'}]) {
    assert.throws(()=>assertPrivatePreviewRepository(()=>JSON.stringify({...metadata,...change})),/verified private/);
  }
});
