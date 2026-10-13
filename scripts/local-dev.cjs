const path=require('node:path');
const profiles=require('../distribution/profiles.json');
const ROOT=path.resolve(__dirname,'..');
function localBuildPlan(url,key,noBundle=false) {
  if(!['http://127.0.0.1:54321','http://localhost:54321'].includes(url)) {
    throw new Error('Local-dev requires the isolated loopback Supabase project on port 54321');
  }
  let valid=/^sb_publishable_[A-Za-z0-9_-]+$/.test(key || '');
  if(!valid) {
    try {
      const parts=key.split('.');
      valid=parts.length===3 && JSON.parse(Buffer.from(parts[1],'base64url').toString('utf8')).role==='anon';
    } catch {valid=false;}
  }
  if(!valid) throw new Error('Local-dev requires a public anon/publishable key, never a privileged key');
  const profile=profiles['local-dev'];
  const overlay={productName:profile.productName,identifier:profile.identifier,
    bundle:{createUpdaterArtifacts:false},plugins:{updater:{endpoints:[],pubkey:''}}};
  return {env:{CARGO_TARGET_DIR:path.join(ROOT,'src-tauri','target'),SYNTHIUS_RELEASE_CHANNEL:'local-dev',SYNTHIUS_SUPABASE_URL:url,
    SYNTHIUS_SUPABASE_PUBLIC_KEY:key,TAURI_CONFIG:JSON.stringify(overlay)},
    args:[path.join(ROOT,'node_modules/@tauri-apps/cli/tauri.js'),'build','--ci','--debug','--config',JSON.stringify(overlay),
      ...(noBundle?['--no-bundle']:['--bundles','nsis']),'--','--locked','--offline'],outputProfile:'debug'};
}
module.exports={localBuildPlan};
if(require.main===module) {
  console.log(JSON.stringify(localBuildPlan(process.env.SYNTHIUS_SUPABASE_URL,process.env.SYNTHIUS_SUPABASE_PUBLIC_KEY,process.argv.includes('--no-bundle'))));
}
