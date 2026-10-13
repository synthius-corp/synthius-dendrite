// Official release packaging only. This script never reads application keys.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {execFileSync} = require('node:child_process');
const {isDeepStrictEqual}=require('node:util');
const {validateReleaseTag,profiles}=require('./channels.cjs');
const {verifyLicenses,copyLicenses}=require('./licenses.cjs');
const ROOT = path.resolve(__dirname, '..');
const run = (program, args, cwd=ROOT) => execFileSync(program, args, {cwd, encoding:'utf8', maxBuffer:32*1024*1024, stdio:['ignore','pipe','inherit']});
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const read = name => JSON.parse(fs.readFileSync(path.join(ROOT, name), 'utf8'));
const writeJSON = (name, value) => fs.writeFileSync(name, JSON.stringify(value, null, 2)+'\n');

function validateVersion(tag, version, channel='production') {
  return validateReleaseTag(tag,version,channel);
}
function verifyArtifacts(directory) {
  const entries=fs.readdirSync(directory);
  if (entries.some(name=>name.endsWith('.sha256') && !entries.includes(name.slice(0,-7)))) throw new Error('Orphan artifact checksum');
  const names = entries.filter(name=>!name.endsWith('.sha256')).sort();
  if (!names.length) throw new Error('No release artifacts');
  for (const name of names) {
    if (!/^[a-zA-Z0-9_.-]+$/.test(name) || !fs.statSync(path.join(directory,name)).isFile()) throw new Error('Invalid artifact name');
    const receipt = fs.readFileSync(path.join(directory,name+'.sha256'),'ascii').trim();
    if (receipt !== `${digest(fs.readFileSync(path.join(directory,name)))}  ${name}`) throw new Error(`Artifact digest mismatch: ${name}`);
  }
  return names;
}
function createDraft(execute, repo, tag, commit, options={}) {
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('An exact source commit is required');
  const release=JSON.parse(execute('gh',['api',`repos/${repo}/releases`,'--method','POST',
    '-f',`tag_name=${tag}`,'-f',`target_commitish=${commit}`,'-F','draft=true','-F',`prerelease=${Boolean(options.prerelease)}`,
    '-f',`make_latest=${options.prerelease?'false':'true'}`,
    '-f',`name=Dendrite ${options.prerelease?'Preview ':''}${tag}`,'-f',`body=${options.body || 'Shared cloud/desktop parser, source selection package and Windows/macOS installers. Windows is unsigned; macOS uses ad-hoc signing. Automatic updater publication is disabled until its permanent release signing key is configured.'}`]));
  if (!Number.isSafeInteger(release.id) || release.id<=0 || release.tag_name!==tag || release.draft!==true) {
    throw new Error('Unexpected draft release identity');
  }
  return release;
}
function assertReleaseCanBeCreated(releases, tag) {
  const existing=releases.find(release=>release.tag_name===tag);
  if (!existing) return;
  if (existing.draft) {
    throw new Error('Unpublished draft exists; recover publication using the original verified artifacts. Do not rebuild installers or bump the version for a publication-only failure.');
  }
  throw new Error('Published release is immutable; a changed build requires a new version.');
}
function packageRelease(target, options={}) {
  if (!['linux-x86_64','windows-x86_64','macos-arm64','macos-x86_64'].includes(target)) throw new Error('Unknown release target');
  verifyLicenses();
  const channel=options.channel || 'production';
  const tag=options.tag || process.env.DENDRITE_RELEASE_TAG || process.env.GITHUB_REF_NAME;
  const version=validateVersion(tag,read('src-tauri/tauri.conf.json').version,channel);
  if (options.version && options.version!==version) throw new Error('Package version/channel mismatch');
  if (options.profile && !isDeepStrictEqual(options.profile,profiles[channel])) throw new Error('Package profile/channel mismatch');
  const stage = path.join(ROOT, 'staging');
  if (fs.existsSync(stage)) throw new Error('Use a clean staging directory');
  fs.mkdirSync(stage);
  if (options.desktopOnly && target==='linux-x86_64') throw new Error('Desktop-only packaging cannot rebuild the Linux parser');
  if (!options.desktopOnly) {
  const bin = target.startsWith('windows') ? 'dendrite-parser.exe' : 'dendrite-parser';
  const pair = path.join(ROOT, 'target', 'release-pair');
  fs.mkdirSync(pair, {recursive:true});
  fs.copyFileSync(path.join(ROOT,'target','release',bin), path.join(pair,bin));
  fs.writeFileSync(path.join(pair,bin+'.sha256'), `${digest(fs.readFileSync(path.join(pair,bin)))}  ${bin}\n`);
  copyLicenses(pair);
  run('tar', ['-czf',path.join(stage,`dendrite-parser-${target}.tar.gz`),'-C',pair,bin,bin+'.sha256','licenses']);
  }
  if (target !== 'linux-x86_64') {
    const extension = target.startsWith('windows') ? '.exe' : '.dmg';
    const walk = dir => fs.readdirSync(dir,{withFileTypes:true}).flatMap(e=>e.isDirectory()?walk(path.join(dir,e.name)):[path.join(dir,e.name)]);
    const installers = walk(path.join(ROOT,'src-tauri','target','release','bundle')).filter(file=>file.endsWith(extension));
    if (installers.length !== 1) throw new Error('Expected exactly one desktop installer');
    fs.copyFileSync(installers[0],path.join(stage,`${channel==='preview'?'SynthiusDendritePreview':'SynthiusDendrite'}-${version}-${target}${extension}`));
  } else {
    const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
    run(npm,['pack','./ui','--pack-destination',stage,'--ignore-scripts']);
  }
  const metadata = JSON.parse(run('cargo',['metadata','--locked','--format-version','1']));
  const packages = target === 'linux-x86_64' ? metadata.packages : [
    ...metadata.packages, ...JSON.parse(run('cargo',['metadata','--locked','--format-version','1','--manifest-path','src-tauri/Cargo.toml'])).packages
  ];
  const unique = [...new Map(packages.map(p=>[p.id,p])).values()];
  writeJSON(path.join(stage,`sbom-${target}.cdx.json`), {
    bomFormat:'CycloneDX', specVersion:'1.5', version:1,
    components:unique.map(p=>({type:'library', name:p.name, version:p.version, 'bom-ref':p.id,
      ...(p.license?{licenses:[{expression:p.license.replace(/\s*\/\s*/g,' OR ')}]}:{}),
      ...(p.source?.startsWith('registry+')?{purl:`pkg:cargo/${p.name}@${p.version}`}:{})}))
  });
  writeJSON(path.join(stage,`provenance-${target}.json`), {
    repository:'https://github.com/synthius-corp/synthius-dendrite',
    sourceCommit:process.env.GITHUB_SHA || run('git',['rev-parse','HEAD']).trim(),
    tag, version, target, corpusSchema:'synthius-corpus/v1', channel,
    desktopOnly:Boolean(options.desktopOnly),
    ...(target!=='linux-x86_64' ? {desktopProfile:profiles[channel]} : {}),
    rust:run('rustc',['--version']).trim(), lockfiles:{
      workspace:digest(fs.readFileSync(path.join(ROOT,'Cargo.lock'))),
      desktop:digest(fs.readFileSync(path.join(ROOT,'src-tauri','Cargo.lock')))
    }, installerSigning:target==='linux-x86_64'?null:target.startsWith('macos')?'ad-hoc':'unsigned',
    automaticUpdater:false,
    artifacts:fs.readdirSync(stage).map(name=>({name,sha256:digest(fs.readFileSync(path.join(stage,name)))}))
  });
  for (const name of fs.readdirSync(stage)) fs.writeFileSync(path.join(stage,name+'.sha256'),`${digest(fs.readFileSync(path.join(stage,name)))}  ${name}\n`);
  verifyArtifacts(stage);
}
function verifyFullRelease(stage,tag,channel,commit) {
  const version=validateVersion(tag,read('src-tauri/tauri.conf.json').version,channel);
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('An exact source commit is required');
  const files = verifyArtifacts(stage);
  const expected=new Set(), downloads={};
  const sourceSelection=files.filter(name=>/^synthius-dendrite-source-selection-\d+\.\d+\.\d+\.tgz$/.test(name));
  if (sourceSelection.length!==1) throw new Error('Full release needs one exact source-selection package');
  expected.add(sourceSelection[0]);
  for (const target of ['linux-x86_64','windows-x86_64','macos-arm64','macos-x86_64']) {
    const parser=`dendrite-parser-${target}.tar.gz`, sbom=`sbom-${target}.cdx.json`, receipt=`provenance-${target}.json`;
    const required=[parser,sbom,receipt];
    if (target==='linux-x86_64') required.push(sourceSelection[0]);
    else required.push(`${channel==='preview'?'SynthiusDendritePreview':'SynthiusDendrite'}-${version}-${target}${target.startsWith('windows')?'.exe':'.dmg'}`);
    required.forEach(name=>expected.add(name));
    if (required.some(name=>!files.includes(name))) throw new Error(`Missing full-release target: ${target}`);
    const provenance=JSON.parse(fs.readFileSync(path.join(stage,receipt),'utf8'));
    if (provenance.repository!=='https://github.com/synthius-corp/synthius-dendrite'
      || provenance.sourceCommit!==commit || provenance.tag!==tag || provenance.version!==version || provenance.target!==target
      || provenance.corpusSchema!=='synthius-corpus/v1' || provenance.channel!==channel
      || provenance.desktopOnly!==false || provenance.automaticUpdater!==false
      || (target!=='linux-x86_64' && !isDeepStrictEqual(provenance.desktopProfile,profiles[channel]))) throw new Error(`Full release provenance mismatch: ${target}`);
    if (!Array.isArray(provenance.artifacts) || required.filter(name=>name!==receipt).some(name=>!provenance.artifacts.some(a=>a.name===name))) {
      throw new Error(`Full release artifact missing from provenance: ${target}`);
    }
    for (const artifact of provenance.artifacts) {
      if (!files.includes(artifact.name) || digest(fs.readFileSync(path.join(stage,artifact.name)))!==artifact.sha256) throw new Error('Provenance artifact mismatch');
    }
    if (target!=='linux-x86_64') {
      const name=required.at(-1), platform={'windows-x86_64':'windows','macos-arm64':'mac_arm64','macos-x86_64':'mac_x64'}[target];
      downloads[platform]={url:`https://github.com/${releaseRepository(channel)}/releases/download/${tag}/${name}`,
        size:fs.statSync(path.join(stage,name)).size,sha256:digest(fs.readFileSync(path.join(stage,name)))};
    }
  }
  const manifest={channel,status:'verified',latest:{version,releaseTag:tag,sourceCommit:commit,productName:profiles[channel].productName},downloads};
  const manifestName=`manifest-${channel}.json`;
  expected.add(manifestName);
  if (files.some(name=>!expected.has(name))) throw new Error('Unexpected artifact or mixed release channels');
  if (files.includes(manifestName) && !isDeepStrictEqual(JSON.parse(fs.readFileSync(path.join(stage,manifestName),'utf8')),manifest)) throw new Error('Retained manifest differs from verified release');
  return manifest;
}
function publishFullRelease(stage,tag,channel,commit,options={}) {
  const manifest=verifyFullRelease(stage,tag,channel,commit);
  const name=`manifest-${channel}.json`;
  writeJSON(path.join(stage,name),manifest);
  fs.writeFileSync(path.join(stage,name+'.sha256'),`${digest(fs.readFileSync(path.join(stage,name)))}  ${name}\n`);
  publishFiles(stage,tag,commit,{...options,resumeDraft:true,prerelease:channel==='preview',
    body:`${profiles[channel].productName} full release: platform parsers, source-selection package and Windows/macOS installers. Channel: ${channel}. Windows is unsigned; macOS uses ad-hoc signing. Automatic updates are disabled.`});
  return manifest;
}
function releaseRepository(channel) {
  if (channel==='production') return 'synthius-corp/synthius-dendrite';
  if (channel==='preview') return 'synthius-corp/synthius-dendrite-private-archive';
  throw new Error('Unknown release channel');
}
function assertPrivatePreviewRepository(execute=run) {
  const repo=releaseRepository('preview');
  const metadata=JSON.parse(execute('gh',['api',`repos/${repo}`]));
  if (metadata.id!==1413068483 || metadata.full_name!==repo || metadata.private!==true) {
    throw new Error('Preview publication requires the verified private repository');
  }
}
function publishFiles(stage,tag,commit,options={}) {
  verifyArtifacts(stage);
  const files=fs.readdirSync(stage);
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('An exact source commit is required');
  const execute=options.execute || run;
  const repo = releaseRepository(options.prerelease?'preview':'production');
  if (options.prerelease) assertPrivatePreviewRepository(execute);
  const releases = JSON.parse(execute('gh',['api',`repos/${repo}/releases?per_page=100`]));
  const existing=releases.find(release=>release.tag_name===tag);
  if (!options.resumeDraft || !existing?.draft) assertReleaseCanBeCreated(releases,tag);
  const refs=JSON.parse(execute('gh',['api',`repos/${repo}/git/matching-refs/tags/${encodeURIComponent(tag)}`]));
  const exact=refs.find(ref=>ref.ref===`refs/tags/${tag}`);
  if (exact) {
    let object=exact.object;
    for (let depth=0; object.type==='tag' && depth<4; depth++) object=JSON.parse(execute('gh',['api',`repos/${repo}/git/tags/${object.sha}`])).object;
    if (object.type!=='commit' || object.sha!==commit) throw new Error('Existing tag does not identify the verified artifact source');
  }
  const draft=existing || createDraft(execute,repo,tag,commit,options);
  if (!Number.isSafeInteger(draft.id) || draft.id<=0 || draft.draft!==true || draft.tag_name!==tag
    || draft.target_commitish!==commit || Boolean(draft.prerelease)!==Boolean(options.prerelease)) throw new Error('Draft source/channel identity mismatch');
  const matches=(asset,name)=>asset && asset.state==='uploaded' && asset.size===fs.statSync(path.join(stage,name)).size
    && asset.digest===`sha256:${digest(fs.readFileSync(path.join(stage,name)))}`;
  for (const asset of draft.assets || []) {
    if (!files.includes(asset.name) || !matches(asset,asset.name)) throw new Error('Existing draft artifact mismatch; retained artifacts must stay unchanged');
  }
  // Draft stays private/incomplete until every upload is verified by GitHub.
  const missing=fs.readdirSync(stage).filter(name=>!(draft.assets || []).some(asset=>asset.name===name));
  if (missing.length) execute('gh',['release','upload',tag,'--repo',repo,...missing.map(name=>path.join(stage,name))]);
  // The tag lookup exposes published releases; drafts must be addressed by ID.
  const release = JSON.parse(execute('gh',['api',`repos/${repo}/releases/${draft.id}`]));
  if (release.id!==draft.id || release.tag_name!==tag || release.draft!==true
    || release.target_commitish!==commit || Boolean(release.prerelease)!==Boolean(options.prerelease)
    || release.assets.length!==fs.readdirSync(stage).length) throw new Error('Draft identity changed');
  for (const name of fs.readdirSync(stage)) {
    const asset=release.assets.find(a=>a.name===name);
    if (!matches(asset,name)) throw new Error('Uploaded artifact verification failed');
  }
  execute('gh',['api',`repos/${repo}/releases/${draft.id}`,'--method','PATCH','-F','draft=false','-f',`make_latest=${options.prerelease?'false':'true'}`]);
}
module.exports={validateVersion,verifyArtifacts,createDraft,assertReleaseCanBeCreated,packageRelease,verifyFullRelease,publishFullRelease,publishFiles,releaseRepository,assertPrivatePreviewRepository,run,digest};
if (require.main === module) {
  const command = process.argv[2];
  if (command==='publish-existing') {
    const [stage,channel,tag,commit]=process.argv.slice(3);
    if (!stage || !channel || !tag || !commit) throw new Error('Usage: publish-existing STAGING_DIRECTORY CHANNEL TAG SOURCE_COMMIT');
    publishFullRelease(path.resolve(stage),tag,channel,commit);
  } else {
  const channel=process.env.SYNTHIUS_RELEASE_CHANNEL || 'production';
  const tag=process.env.DENDRITE_RELEASE_TAG || process.env.GITHUB_REF_NAME;
  if (command==='validate') validateVersion(tag,read('src-tauri/tauri.conf.json').version,channel);
  else if (command==='collect') packageRelease(process.env.DENDRITE_RELEASE_TARGET,{channel,tag});
  else if (command==='publish') publishFullRelease(path.join(ROOT,'staging'),tag,channel,process.env.GITHUB_SHA);
  else throw new Error('Unknown release command');
  }
}
