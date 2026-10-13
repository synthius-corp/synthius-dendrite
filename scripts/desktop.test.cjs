const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const os=require('node:os');
const {validateDesktopTag,reserveDesktopTag,publicKeyIsValid,buildProfile,verifyDesktopRelease,profiles,TARGETS}=require('./desktop.cjs');
const {digest}=require('./release.cjs');
const key=(role,ref)=>`e30.${Buffer.from(JSON.stringify({role,ref})).toString('base64url')}.synthetic`;
test('production identity is preserved and preview is isolated',()=>{
  const prod=profiles.production,preview=profiles.preview;
  assert.equal(prod.identifier,'app.synthius.sidecar');assert.equal(prod.dataDirectory,'SynthiusSidecar');assert.equal(prod.port,1543);
  for(const field of ['identifier','dataDirectory','port','supabaseUrl','webAppUrl','backendUrl']) assert.notEqual(prod[field],preview[field]);
  assert.equal(preview.port,1544);
});
test('Production loads its checked-in public key; Preview requires a private file and rejects unsuitable keys',()=>{
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-private-preview-'));
  const file=path.join(directory,'preview.json');
  const preview={supabaseUrl:'https://synthetic-preview.supabase.co',webAppUrl:'https://preview.example/files',backendUrl:'https://api.preview.example',supabasePublicKey:key('anon','synthetic-preview')};
  fs.writeFileSync(file,JSON.stringify(preview));
  try {
  assert.equal(buildProfile('production').env.SYNTHIUS_SUPABASE_PUBLIC_KEY,require('../distribution/public-client/production.json').supabasePublicKey);
  assert.throws(()=>buildProfile('preview'),/private|SYNTHIUS_PREVIEW_CONFIG_FILE/);
  assert.throws(()=>buildProfile('preview',{SYNTHIUS_PREVIEW_CONFIG_FILE:path.join(__dirname,'../distribution/public-client/production.json')}),/outside/);
  for(const channel of ['production','preview']) {
    const endpoints=channel==='production'?profiles.production:preview;
    const project=new URL(endpoints.supabaseUrl).hostname.split('.')[0];
    const otherProject=channel==='production'?'synthetic-preview':'wjegriwrnrwndbonxkeg';
    const input={SYNTHIUS_SUPABASE_PUBLIC_KEY:key('anon',project)};
    if(channel==='preview') input.SYNTHIUS_PREVIEW_CONFIG_FILE=file;
    const configured=buildProfile(channel,input).env;
    assert.equal(configured.SYNTHIUS_SUPABASE_URL,endpoints.supabaseUrl);
    assert.equal(configured.SYNTHIUS_SUPABASE_PUBLIC_KEY,input.SYNTHIUS_SUPABASE_PUBLIC_KEY);
    for(const value of ['','sb_secret_synthetic',key('service_role',project),key('anon',otherProject)]) {
      assert.throws(()=>buildProfile(channel,{...input,SYNTHIUS_SUPABASE_PUBLIC_KEY:value}));
    }
    assert.throws(()=>buildProfile(channel,{...input,SYNTHIUS_SUPABASE_URL:'https://wrong-project.supabase.co'}));
  }
  assert.equal(publicKeyIsValid('sb_publishable_synthetic',preview.supabaseUrl),true);
  assert.equal(fs.existsSync(path.join(__dirname,'../distribution/public-client/preview.json')),false);
  } finally {fs.rmSync(directory,{recursive:true,force:true});}
});
test('each channel has an exact tag and disables the development updater',()=>{
  assert.equal(validateDesktopTag('v2.1.4-preview.1','2.1.4','preview'),'2.1.4-preview.1');
  assert.equal(validateDesktopTag('v2.1.4','2.1.4','production'),'2.1.4');
  for(const [tag,channel] of [['v2.1.4','preview'],['v2.1.4-preview.1','production'],['v2.1.4-preview.0','preview'],['v2.1.3-preview.1','preview']]) assert.throws(()=>validateDesktopTag(tag,'2.1.4',channel));
  for(const channel of ['production','preview']) {
    const directory=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-profile-overlay-'));
    const file=path.join(directory,'private.json');
    fs.writeFileSync(file,JSON.stringify({supabaseUrl:'https://synthetic-preview.supabase.co',webAppUrl:'https://preview.example/files',backendUrl:'https://api.preview.example',supabasePublicKey:key('anon','synthetic-preview')}));
    const {overlay}=buildProfile(channel,channel==='preview'?{SYNTHIUS_PREVIEW_CONFIG_FILE:file}:{});
    assert.equal(overlay.bundle.createUpdaterArtifacts,false);assert.deepEqual(overlay.plugins.updater,{endpoints:[],pubkey:''});
    fs.rmSync(directory,{recursive:true,force:true});
  }
});

test('desktop sidebar note matches footer typography and current webapp wording',()=>{
  const status=fs.readFileSync(path.join(__dirname,'../src-tauri/src/status.html'),'utf8');
  assert.match(status,/\.sidebar-note\{[^}]*padding:14px 0 0[^}]*font-size:9px[^}]*line-height:1\.35/);
  assert.match(status,/\.foot\{[^}]*font-size:9px/);
  assert.match(status,/>Local sources stay on this computer\. Manage your personas in Synthius webapp\.<\/div>/);
  assert.doesNotMatch(status,/Manage your personas in Synthius\.<\/div>/);
});

test('desktop preflight reserves the exact source tag before expensive builds',()=>{
  const tag='v2.1.4-preview.1',commit='a'.repeat(40),calls=[];
  const execute=(program,args)=>{
    calls.push(args);
    if(args[1]==='repos/synthius-corp/synthius-dendrite-private-archive')return JSON.stringify({id:1413068483,full_name:'synthius-corp/synthius-dendrite-private-archive',private:true});
    return JSON.stringify(args.includes('POST')?{ref:`refs/tags/${tag}`,object:{type:'commit',sha:commit}}:[]);
  };
  reserveDesktopTag(tag,'2.1.4','preview',commit,execute);
  assert.equal(calls.length,3);
  assert.ok(calls[2].includes(`sha=${commit}`));
  assert.ok(calls.every(args=>args[1].startsWith('repos/synthius-corp/synthius-dendrite-private-archive')));
  const existing=(program,args)=>JSON.stringify(args[1]==='repos/synthius-corp/synthius-dendrite-private-archive'?{id:1413068483,full_name:'synthius-corp/synthius-dendrite-private-archive',private:true}:[{ref:`refs/tags/${tag}`,object:{type:'commit',sha:commit}}]);
  reserveDesktopTag(tag,'2.1.4','preview',commit,existing);
  assert.throws(()=>reserveDesktopTag(tag,'2.1.4','preview','b'.repeat(40),existing),/exact build source/);
  assert.throws(()=>reserveDesktopTag(tag,'2.1.4','production',commit,execute));
  const annotated=(program,args)=>JSON.stringify(args[1]==='repos/synthius-corp/synthius-dendrite-private-archive'?{id:1413068483,full_name:'synthius-corp/synthius-dendrite-private-archive',private:true}:args[1].includes('matching-refs')?
    [{ref:`refs/tags/${tag}`,object:{type:'tag',sha:'b'.repeat(40)}}]:{object:{type:'commit',sha:commit}});
  reserveDesktopTag(tag,'2.1.4','preview',commit,annotated);
});
test('Production requires the owner-created exact tag and never creates a tag with the workflow token',()=>{
  const tag='v2.1.4',commit='a'.repeat(40),calls=[];
  const execute=(program,args)=>{calls.push(args);return JSON.stringify([]);};
  assert.throws(()=>reserveDesktopTag(tag,'2.1.4','production',commit,execute),/owner must create/);
  assert.equal(calls.length,1);
  assert.ok(!calls[0].includes('POST'));
  reserveDesktopTag(tag,'2.1.4','production',commit,()=>JSON.stringify([{ref:`refs/tags/${tag}`,object:{type:'commit',sha:commit}}]));
  assert.throws(()=>reserveDesktopTag(tag,'2.1.4','production',commit,()=>JSON.stringify([{ref:`refs/tags/${tag}`,object:{type:'commit',sha:'b'.repeat(40)}}])),/exact build source/);
});
test('desktop workflow builds only the three affected production targets',()=>{
  const workflow=fs.readFileSync(path.join(__dirname,'../.github/workflows/desktop.yml'),'utf8');
  assert.match(workflow,/workflow_dispatch/);
  assert.match(workflow,/DENDRITE_RELEASE_TAG: \$\{\{ inputs.release_tag \}\}/);
  assert.doesNotMatch(workflow,/GITHUB_REF_NAME:|cargo build -p synthius-dendrite-parser|uses: \.\/.github\/workflows\/ci.yml/);
  assert.equal([...workflow.matchAll(/target: (windows-x86_64|macos-arm64|macos-x86_64)/g)].length,3);
  assert.match(workflow,/SYNTHIUS_RELEASE_CHANNEL: production/);
  assert.doesNotMatch(workflow,/DENDRITE_PREVIEW_SUPABASE_PUBLIC_KEY|options: \[preview, production\]|inputs\.channel/);
  assert.match(workflow,/assertReleaseCanBeCreated/);
  assert.match(workflow,/reserveDesktopTag\(process.env.DENDRITE_RELEASE_TAG/);
  assert.match(workflow,/cache-on-failure: true/);
  assert.match(workflow,/id: upload\s+continue-on-error: true/);
  assert.match(workflow,/Retry only the artifact upload\s+if: \$\{\{ steps.upload.outcome == 'failure' \}\}/);
  assert.equal([...workflow.matchAll(/node scripts\/desktop.cjs build/g)].length,1);
  const fullRelease=fs.readFileSync(path.join(__dirname,'../.github/workflows/package.yml'),'utf8');
  assert.match(fullRelease,/workflow_dispatch:/);
  assert.doesNotMatch(fullRelease,/^  push:/m);
  assert.match(fullRelease,/SYNTHIUS_RELEASE_CHANNEL: production/);
  assert.doesNotMatch(fullRelease,/DENDRITE_PREVIEW_SUPABASE_PUBLIC_KEY|options: \[preview, production\]|inputs\.channel/);
});
test('desktop builds clear stale generated installers before packaging',()=>{
  const source=fs.readFileSync(path.join(__dirname,'desktop.cjs'),'utf8');
  assert.match(source,/cleanDesktopBundleOutputs\(\);\s*execute\(process\.execPath/);
  assert.match(source,/src-tauri','target','release','bundle/);
});
test('desktop-only publication checks all three installers and their channel/source provenance',()=>{
  const stage=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-desktop-release-'));
  const commit='a'.repeat(40),tag='v2.1.4-preview.1';
  const write=(name,value)=>{const bytes=typeof value==='string'?value:JSON.stringify(value);fs.writeFileSync(path.join(stage,name),bytes);fs.writeFileSync(path.join(stage,name+'.sha256'),`${digest(bytes)}  ${name}\n`);};
  try {
    for(const target of Object.keys(TARGETS)) {
      const name=`SynthiusDendritePreview-2.1.4-preview.1-${target}${target.startsWith('windows')?'.exe':'.dmg'}`;
      write(name,'synthetic installer');
      write(`provenance-${target}.json`,{repository:'https://github.com/synthius-corp/synthius-dendrite',sourceCommit:commit,tag,version:tag.slice(1),target,channel:'preview',desktopOnly:true,desktopProfile:profiles.preview,automaticUpdater:false,artifacts:[{name,sha256:digest('synthetic installer')}]});
    }
    const manifest=verifyDesktopRelease(stage,tag,'preview',commit);
    assert.equal(Object.keys(manifest.downloads).length,3);assert.ok(manifest.downloads.windows.url.includes('/synthius-dendrite-private-archive/'));assert.match(manifest.downloads.windows.url,/releases\/download\/v2.1.4-preview.1\/SynthiusDendritePreview/);
    assert.throws(()=>verifyDesktopRelease(stage,tag,'preview','b'.repeat(40)),/provenance mismatch/);
    assert.throws(()=>verifyDesktopRelease(stage,tag,'production',commit));
    write('dendrite-parser-linux-x86_64.tar.gz','unexpected');
    assert.throws(()=>verifyDesktopRelease(stage,tag,'preview',commit),/reuse existing/);
  } finally {fs.rmSync(stage,{recursive:true,force:true});}
});
test('a local preview may publish only the platforms actually built',()=>{
  const stage=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-local-preview-'));
  const commit='a'.repeat(40),tag='v2.1.4-preview.3',target='windows-x86_64';
  const name=`SynthiusDendritePreview-2.1.4-preview.3-${target}.exe`;
  const write=(file,value)=>{const bytes=typeof value==='string'?value:JSON.stringify(value);fs.writeFileSync(path.join(stage,file),bytes);fs.writeFileSync(path.join(stage,file+'.sha256'),`${digest(bytes)}  ${file}\n`);};
  try {
    write(name,'synthetic installer');
    write(`provenance-${target}.json`,{repository:'https://github.com/synthius-corp/synthius-dendrite',sourceCommit:commit,tag,version:tag.slice(1),target,channel:'preview',desktopOnly:true,desktopProfile:profiles.preview,automaticUpdater:false,artifacts:[{name,sha256:digest('synthetic installer')}]});
    const manifest=verifyDesktopRelease(stage,tag,'preview',commit,[target]);
    assert.deepEqual(Object.keys(manifest.downloads),['windows']);
    assert.match(manifest.downloads.windows.url,/v2\.1\.4-preview\.3/);
  } finally {fs.rmSync(stage,{recursive:true,force:true});}
});
