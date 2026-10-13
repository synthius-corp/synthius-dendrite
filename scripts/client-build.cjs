// Community Production builds load public configuration; no maintainer secrets.
const path=require('node:path');
const {spawnSync}=require('node:child_process');
const {buildProfile}=require('./desktop.cjs');
const ROOT=path.resolve(__dirname,'..');
function clientBuildPlan(input={}) {
  const {overlay,env}=buildProfile('production',input);
  return {program:process.execPath,args:[path.join(ROOT,'node_modules/@tauri-apps/cli/tauri.js'),
    'build','--ci','--no-bundle','--config',JSON.stringify(overlay),'--','--locked'],
    cwd:path.join(ROOT,'src-tauri'),env};
}
module.exports={clientBuildPlan};
if(require.main===module) {
  const plan=clientBuildPlan(process.env);
  const result=spawnSync(plan.program,plan.args,{cwd:plan.cwd,env:plan.env,stdio:'inherit',shell:false});
  if(result.error) throw result.error;
  process.exitCode=result.status ?? 1;
}
