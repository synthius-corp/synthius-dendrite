const fs=require('node:fs');
const path=require('node:path');
const {spawnSync}=require('node:child_process');
const {packageRelease,verifyArtifacts,publishFiles,releaseRepository,assertPrivatePreviewRepository,digest,run}=require('./release.cjs');
const ROOT=path.resolve(__dirname,'..');
const {validateReleaseTag,profiles}=require('./channels.cjs');
const {publicKeyIsValid,clientConfiguration}=require('./client-config.cjs');
const TARGETS={ 'windows-x86_64':'windows', 'macos-arm64':'mac_arm64', 'macos-x86_64':'mac_x64' };

function validateDesktopTag(tag,version,channel) {
  return validateReleaseTag(tag,version,channel);
}
function reserveDesktopTag(tag,version,channel,commit,execute=run) {
  validateDesktopTag(tag,version,channel);
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('Exact source commit required');
  const repo=releaseRepository(channel);
  if (channel==='preview') assertPrivatePreviewRepository(execute);
  const refs=JSON.parse(execute('gh',['api',`repos/${repo}/git/matching-refs/tags/${encodeURIComponent(tag)}`]));
  let ref=refs.find(value=>value.ref===`refs/tags/${tag}`);
  if (!ref) ref=JSON.parse(execute('gh',['api',`repos/${repo}/git/refs`,'--method','POST','-f',`ref=refs/tags/${tag}`,'-f',`sha=${commit}`]));
  if (ref.ref!==`refs/tags/${tag}`) throw new Error('Unexpected desktop tag identity');
  let object=ref.object;
  for (let depth=0; object.type==='tag' && depth<4; depth++) object=JSON.parse(execute('gh',['api',`repos/${repo}/git/tags/${object.sha}`])).object;
  if (object.type!=='commit' || object.sha!==commit) throw new Error('Desktop tag does not identify the exact build source');
}
function buildProfile(channel,input={}) {
  const profile=profiles[channel];
  const {key,endpoints}=clientConfiguration(channel,input);
  const overlay={productName:profile.productName,identifier:profile.identifier,
    bundle:{createUpdaterArtifacts:false},plugins:{updater:{endpoints:[],pubkey:''}}};
  const env={...input,SYNTHIUS_RELEASE_CHANNEL:channel,SYNTHIUS_SUPABASE_URL:endpoints.supabaseUrl,
    SYNTHIUS_SUPABASE_PUBLIC_KEY:key,TAURI_CONFIG:JSON.stringify(overlay)};
  return {profile,overlay,env};
}
function execute(program,args,env,cwd=ROOT) {
  const result=spawnSync(program,args,{cwd,env,stdio:'inherit',shell:false});
  if (result.error) throw result.error;
  if (result.status!==0) throw new Error(`${program} failed (${result.status})`);
}
function cleanDesktopBundleOutputs() {
  fs.rmSync(path.join(ROOT,'src-tauri','target','release','bundle'),{recursive:true,force:true});
}
function verifyDesktopRelease(stage,tag,channel,commit,targets=Object.keys(TARGETS)) {
  const base=JSON.parse(fs.readFileSync(path.join(ROOT,'src-tauri/tauri.conf.json'),'utf8')).version;
  const version=validateDesktopTag(tag,base,channel);
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('Exact source commit required');
  if (!Array.isArray(targets) || !targets.length || new Set(targets).size!==targets.length
    || targets.some(target=>!TARGETS[target])) throw new Error('At least one supported desktop target is required');
  const files=verifyArtifacts(stage);
  const downloads={};
  for (const target of targets) {
    const platform=TARGETS[target];
    const name=`${channel==='preview'?'SynthiusDendritePreview':'SynthiusDendrite'}-${version}-${target}${target.startsWith('windows')?'.exe':'.dmg'}`;
    const provenance=JSON.parse(fs.readFileSync(path.join(stage,`provenance-${target}.json`),'utf8'));
    if (!files.includes(name) || provenance.repository!=='https://github.com/synthius-corp/synthius-dendrite'
      || provenance.sourceCommit!==commit || provenance.tag!==tag || provenance.version!==version || provenance.target!==target
      || provenance.channel!==channel || provenance.desktopOnly!==true || provenance.automaticUpdater!==false
      || JSON.stringify(provenance.desktopProfile)!==JSON.stringify(profiles[channel])) throw new Error(`Desktop provenance mismatch: ${target}`);
    for (const artifact of provenance.artifacts) {
      if (!files.includes(artifact.name) || digest(fs.readFileSync(path.join(stage,artifact.name)))!==artifact.sha256) throw new Error('Provenance artifact mismatch');
    }
    if (!provenance.artifacts.some(a=>a.name===name)) throw new Error('Installer missing from provenance');
    downloads[platform]={url:`https://github.com/${releaseRepository(channel)}/releases/download/${tag}/${name}`,
      size:fs.statSync(path.join(stage,name)).size,sha256:digest(fs.readFileSync(path.join(stage,name)))};
  }
  if (files.some(name=>name.startsWith('dendrite-parser-') || name.endsWith('.tgz'))) throw new Error('Desktop release must reuse existing parser/UI artifacts');
  return {channel,status:'verified',latest:{version,releaseTag:tag,sourceCommit:commit,productName:profiles[channel].productName},downloads};
}
function publishDesktop(stage,tag,channel,commit,targets=Object.keys(TARGETS)) {
  const manifest=verifyDesktopRelease(stage,tag,channel,commit,targets);
  const name=`manifest-${channel}.json`;
  fs.writeFileSync(path.join(stage,name),JSON.stringify(manifest,null,2)+'\n');
  fs.writeFileSync(path.join(stage,name+'.sha256'),`${digest(fs.readFileSync(path.join(stage,name)))}  ${name}\n`);
  const platformNames=targets.map(target=>({
    'windows-x86_64':'Windows x64','macos-arm64':'macOS Apple Silicon','macos-x86_64':'macOS Intel'
  })[target]).join(', ');
  publishFiles(stage,tag,commit,{resumeDraft:true,prerelease:channel==='preview',body:`${profiles[channel].productName} desktop installer${targets.length===1?'':'s'} for ${platformNames}. Channel: ${channel}. Windows is unsigned; macOS uses ad-hoc signing. Automatic updates are disabled.`});
  return manifest;
}
function publishLocalPreview(stage,tag,commit) {
  const version=validateDesktopTag(tag,JSON.parse(fs.readFileSync(path.join(ROOT,'src-tauri/tauri.conf.json'),'utf8')).version,'preview');
  const targets=Object.keys(TARGETS).filter(target=>fs.existsSync(path.join(stage,
    `SynthiusDendritePreview-${version}-${target}${target.startsWith('windows')?'.exe':'.dmg'}`)));
  return publishDesktop(stage,tag,'preview',commit,targets);
}
module.exports={validateDesktopTag,reserveDesktopTag,publicKeyIsValid,buildProfile,verifyDesktopRelease,publishDesktop,publishLocalPreview,profiles,TARGETS};
if (require.main===module && process.argv[2]==='publish-existing') {
  const [stage,channel,tag,commit]=process.argv.slice(3);
  if (!stage || !channel || !tag || !commit) throw new Error('Usage: publish-existing STAGING_DIRECTORY CHANNEL TAG SOURCE_COMMIT');
  publishDesktop(path.resolve(stage),tag,channel,commit);
} else if (require.main===module && process.argv[2]==='publish-local-preview') {
  const [stage,tag,commit]=process.argv.slice(3);
  if (!stage || !tag || !commit) throw new Error('Usage: publish-local-preview STAGING_DIRECTORY TAG SOURCE_COMMIT');
  publishLocalPreview(path.resolve(stage),tag,commit);
} else if (require.main===module) {
  const command=process.argv[2];
  const channel=process.env.SYNTHIUS_RELEASE_CHANNEL;
  const tag=process.env.DENDRITE_RELEASE_TAG || process.env.GITHUB_REF_NAME;
  const config=JSON.parse(fs.readFileSync(path.join(ROOT,'src-tauri/tauri.conf.json'),'utf8'));
  const version=validateDesktopTag(tag,config.version,channel);
  const {profile,overlay,env}=buildProfile(channel,process.env);
  if (command==='test') {
    require('./licenses.cjs').verifyLicenses();
    execute(process.execPath,['--test','ui/source-selection.test.cjs','ui/telegram.test.cjs','scripts/release.test.cjs','scripts/desktop.test.cjs','scripts/release-guard.test.cjs','scripts/ci-policy.test.cjs','scripts/licenses.test.cjs','scripts/local-dev.test.cjs','scripts/offline-dev.test.cjs','scripts/client-build.test.cjs'],env);
    execute('cargo',['fmt','--all','--check'],env);
    execute('cargo',['test','--workspace','--locked'],env);
    execute('cargo',['fmt','--manifest-path','src-tauri/Cargo.toml','--all','--check'],env);
    execute('cargo',['test','--manifest-path','src-tauri/Cargo.toml','--locked'],env);
  } else if (command==='build') {
    require('./licenses.cjs').verifyLicenses();
    const {requireTelegramApplicationIdentity}=require('./release-guard.cjs');
    requireTelegramApplicationIdentity(env);
    cleanDesktopBundleOutputs();
    execute(process.execPath,[path.join(ROOT,'node_modules/@tauri-apps/cli/tauri.js'),'build','--ci','--config',JSON.stringify(overlay),'--','--locked'],env,path.join(ROOT,'src-tauri'));
  } else if (command==='collect') {
    packageRelease(process.env.DENDRITE_RELEASE_TARGET,{desktopOnly:true,channel,profile,version,tag});
  } else if (command==='publish') {
    publishDesktop(path.join(ROOT,'staging'),tag,channel,process.env.GITHUB_SHA);
  } else throw new Error('Unknown desktop command');
}
