// Standalone contributor builds/tests: no Supabase, private backend or real keys.
const path=require('node:path');
const {spawnSync}=require('node:child_process');
const {localBuildPlan}=require('./local-dev.cjs');
const ROOT=path.resolve(__dirname,'..');
function offlinePlan(command,input={}) {
  if(!['test','build','dev'].includes(command)) throw new Error('Use test, build or dev');
  const local=localBuildPlan('http://127.0.0.1:54321','sb_publishable_offline_test_fixture',true);
  const env={...input,...local.env};
  if(input.CARGO_TARGET_DIR) env.CARGO_TARGET_DIR=input.CARGO_TARGET_DIR;
  delete env.SYNTHIUS_TELEGRAM_API_ID;
  delete env.SYNTHIUS_TELEGRAM_API_HASH;
  const overlay=local.env.TAURI_CONFIG;
  if(command==='test') return {program:'cargo',args:['test','--manifest-path','src-tauri/Cargo.toml','--locked'],cwd:ROOT,env};
  return {program:process.execPath,
    args:command==='build'?local.args.filter(arg=>arg!=='--offline')
      :[path.join(ROOT,'node_modules/@tauri-apps/cli/tauri.js'),'dev','--config',overlay,'--','--locked'],
    cwd:path.join(ROOT,'src-tauri'),env};
}
module.exports={offlinePlan};
if(require.main===module) {
  const plan=offlinePlan(process.argv[2],process.env);
  const result=spawnSync(plan.program,plan.args,{cwd:plan.cwd,env:plan.env,stdio:'inherit',shell:false});
  if(result.error) throw result.error;
  process.exitCode=result.status ?? 1;
}
