const fs=require('node:fs');
const path=require('node:path');
const {profiles}=require('./channels.cjs');
function publicKeyIsValid(key,url) {
  if(typeof key!=='string' || !key) return false;
  if(/^sb_publishable_[A-Za-z0-9_-]+$/.test(key)) return true;
  try {
    if(key.split('.').length!==3) return false;
    const claims=JSON.parse(Buffer.from(key.split('.')[1],'base64url').toString('utf8'));
    return claims.role==='anon' && claims.ref===new URL(url).hostname.split('.')[0];
  } catch {return false;}
}
function clientConfiguration(channel,input={}) {
  if(!['production','preview'].includes(channel)) throw new Error('Unknown desktop channel');
  const file=channel==='production'?path.join(__dirname,'../distribution/public-client/production.json'):input.SYNTHIUS_PREVIEW_CONFIG_FILE;
  if(!file) throw new Error('Maintainer Preview builds require SYNTHIUS_PREVIEW_CONFIG_FILE outside the tracked repository');
  if(channel==='preview') {
    const root=fs.realpathSync(path.join(__dirname,'..'));
    const relative=path.relative(root,fs.realpathSync(file));
    if(relative==='' || (!relative.startsWith('..'+path.sep) && !path.isAbsolute(relative))) {
      throw new Error('Keep private Preview configuration outside the repository');
    }
  }
  const config=JSON.parse(fs.readFileSync(file,'utf8'));
  const endpoints=channel==='production'?profiles.production:config;
  for(const name of ['supabaseUrl','webAppUrl','backendUrl']) {
    const url=new URL(endpoints[name]);
    if(url.protocol!=='https:' || url.username || url.password || url.search || url.hash
      || (name!=='webAppUrl' && url.pathname!=='/')) throw new Error('Invalid hosted client endpoint');
  }
  if(channel==='production' && config.supabaseUrl!==profiles.production.supabaseUrl) throw new Error('Production public configuration belongs to another project');
  if(input.SYNTHIUS_SUPABASE_URL && input.SYNTHIUS_SUPABASE_URL!==endpoints.supabaseUrl) throw new Error('Supabase URL belongs to another channel');
  const key=input.SYNTHIUS_SUPABASE_PUBLIC_KEY ?? config.supabasePublicKey;
  if(!publicKeyIsValid(key,endpoints.supabaseUrl)) throw new Error('A matching public Supabase key is required');
  return {key,endpoints};
}
module.exports={publicKeyIsValid,clientConfiguration};
