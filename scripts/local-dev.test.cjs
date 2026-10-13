const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const {localBuildPlan}=require('./local-dev.cjs');
const key=role=>`e30.${Buffer.from(JSON.stringify({role})).toString('base64url')}.synthetic`;
test('local builds use debug outputs, their own identity/storage and disabled updates',()=>{
  for(const noBundle of [false,true]) {
    const plan=localBuildPlan('http://127.0.0.1:54321',key('anon'),noBundle);
    assert.ok(plan.args.includes('--debug'));
    assert.equal(plan.outputProfile,'debug');
    assert.equal(plan.env.SYNTHIUS_RELEASE_CHANNEL,'local-dev');
    assert.equal(plan.env.CARGO_TARGET_DIR,path.join(__dirname,'..','src-tauri','target'));
    const overlay=JSON.parse(plan.env.TAURI_CONFIG);
    assert.equal(overlay.identifier,'app.synthius.dendrite.localdev');
    assert.deepEqual(overlay.plugins.updater,{endpoints:[],pubkey:''});
    assert.equal(plan.args.includes('--no-bundle'),noBundle);
    assert.deepEqual(plan.args.slice(-3),['--','--locked','--offline']);
  }
  const profiles=require('../distribution/profiles.json');
  assert.notEqual(profiles['local-dev'].dataDirectory,profiles.production.dataDirectory);
});
test('local builds reject hosted projects, URL tricks and privileged keys',()=>{
  for(const url of ['https://example.com','http://localhost.evil:54321','http://localhost:54321/','http://user@localhost:54321','http://localhost:54322']) {
    assert.throws(()=>localBuildPlan(url,key('anon')));
  }
  for(const value of ['',key('service_role'),'sb_secret_synthetic','malformed']) {
    assert.throws(()=>localBuildPlan('http://localhost:54321',value));
  }
});
test('PowerShell helper consumes the tested plan and copies debug artifacts',()=>{
  const root=path.join(__dirname,'..');
  const helper=fs.readFileSync(path.join(root,'build_local_dev.ps1'),'utf8');
  assert.match(helper,/scripts\/local-dev.cjs/);
  assert.match(helper,/& node @buildArguments/);
  assert.match(helper,/target\/debug\/synthius-dendrite.exe/);
  assert.doesNotMatch(helper,/cargo tauri|target\/release/);
  assert.match(helper,/\$\(\$overlay.productName\)_\$\{version\}_x64-setup.exe/);
  const fence=fs.readFileSync(path.join(root,'src-tauri/build_policy.rs'),'utf8');
  assert.match(fence,/channel == "local-dev"/);
  assert.match(fence,/"local-dev" => local_debug\(profile, channel, url\)/);
});
