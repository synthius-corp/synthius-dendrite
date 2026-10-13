const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
const {assertOwnerContext,assertProtection,verifyReleaseAuthority,requireTelegramApplicationIdentity}=require('./release-guard.cjs');
const {assertGoldenVersion}=require('./check-parser-version.cjs');
const context={GITHUB_REPOSITORY:'synthius-corp/synthius-dendrite',GITHUB_REPOSITORY_ID:'1417119504',
  GITHUB_ACTOR:'x-aero',GITHUB_ACTOR_ID:'167432977',GITHUB_TRIGGERING_ACTOR:'x-aero',
  GITHUB_EVENT_NAME:'workflow_dispatch',GITHUB_REF:'refs/heads/main',GITHUB_SHA:'a'.repeat(40)};
const environment={protection_rules:[{type:'required_reviewers',prevent_self_review:false,
  reviewers:[{type:'User',reviewer:{id:167432977}}]}],deployment_branch_policy:{protected_branches:true}};
const protection={required_pull_request_reviews:{require_code_owner_reviews:true,dismiss_stale_reviews:true,required_approving_review_count:1},
  required_status_checks:{strict:true,contexts:['rust']},allow_force_pushes:{enabled:false},allow_deletions:{enabled:false}};

test('distribution authority rejects outside triggers, outsider reruns, fork repositories and other refs',()=>{
  assertOwnerContext(context);
  for(const change of [{GITHUB_ACTOR_ID:'3356474'},{GITHUB_ACTOR:'another-user'},
    {GITHUB_TRIGGERING_ACTOR:'another-user'},{GITHUB_REPOSITORY_ID:'1'},
    {GITHUB_REPOSITORY_ID:'1413068483'},
    {GITHUB_REPOSITORY:'fork/dendrite'},{GITHUB_EVENT_NAME:'push'},
    {GITHUB_EVENT_NAME:'pull_request'},{GITHUB_REF:'refs/tags/v2.1.4'},
    {GITHUB_REF:'refs/heads/contributor-change'},{GITHUB_SHA:'invalid'}]) {
    assert.throws(()=>assertOwnerContext({...context,...change}));
  }
});
test('changed expected corpora require an increased parser version',()=>{
  const manifest=value=>`[package]\nversion = "${value}"\n`;
  const files=['dendrite-parser/tests/fixtures/expected/synthetic.json'];
  assertGoldenVersion(manifest('1.0.0'),manifest('1.1.0'),files);
  assert.throws(()=>assertGoldenVersion(manifest('1.1.0'),manifest('1.1.0'),files));
  assert.throws(()=>assertGoldenVersion(manifest('1.1.0'),manifest('1.0.9'),files));
  assertGoldenVersion(manifest('1.1.0'),manifest('1.1.0'),['CONTRIBUTING.md']);
});
test('required owner reviews and release approval cannot be satisfied by another reviewer',()=>{
  assertProtection(protection,environment,'* @x-aero\n');
  for(const change of [{required_pull_request_reviews:{...protection.required_pull_request_reviews,require_code_owner_reviews:false}},
    {required_pull_request_reviews:{...protection.required_pull_request_reviews,dismiss_stale_reviews:false}},
    {required_status_checks:{strict:false,contexts:['rust']}},{allow_force_pushes:{enabled:true}},{allow_deletions:{enabled:true}}]) {
    assert.throws(()=>assertProtection({...protection,...change},environment,'* @x-aero'));
  }
  assert.throws(()=>assertProtection(protection,environment,'* @x-aero @another-user'));
  assert.throws(()=>assertProtection(protection,{...environment,protection_rules:[]},'* @x-aero'));
  assert.throws(()=>assertProtection(protection,{...environment,protection_rules:[{...environment.protection_rules[0],prevent_self_review:true}]},'* @x-aero'));
  assert.throws(()=>assertProtection(protection,{...environment,protection_rules:[{...environment.protection_rules[0],reviewers:[{type:'User',reviewer:{id:1}}]}]},'* @x-aero'));
});
test('preflight verifies the protected main SHA and configured environment before building',()=>{
  const execute=(program,args)=>JSON.stringify(args[1].endsWith('/branches/main')?{protected:true,commit:{sha:context.GITHUB_SHA}}
    :args[1].includes('/environments/')?environment:{content:Buffer.from('* @x-aero').toString('base64')});
  verifyReleaseAuthority(context,execute);
  assert.throws(()=>verifyReleaseAuthority(context,(program,args)=>args[1].endsWith('/branches/main')?JSON.stringify({protected:false,commit:{sha:context.GITHUB_SHA}}):execute(program,args)));
  assert.throws(()=>verifyReleaseAuthority(context,(program,args)=>args[1].endsWith('/branches/main')?JSON.stringify({protected:true,commit:{sha:'b'.repeat(40)}}):execute(program,args)));
});
test('official desktop builds retain Telegram credentials and reject missing or malformed identity',()=>{
  const secretEnv={SYNTHIUS_TELEGRAM_API_ID:'123',SYNTHIUS_TELEGRAM_API_HASH:'a'.repeat(32),SAFE:'unchanged'};
  assert.equal(requireTelegramApplicationIdentity(secretEnv),secretEnv);
  assert.equal(secretEnv.SYNTHIUS_TELEGRAM_API_ID,'123');
  assert.equal(secretEnv.SYNTHIUS_TELEGRAM_API_HASH,'a'.repeat(32));
  assert.equal(secretEnv.SAFE,'unchanged');
  for(const change of [{SYNTHIUS_TELEGRAM_API_ID:undefined},{SYNTHIUS_TELEGRAM_API_HASH:undefined},
    {SYNTHIUS_TELEGRAM_API_ID:'0'},{SYNTHIUS_TELEGRAM_API_ID:'2147483648'},
    {SYNTHIUS_TELEGRAM_API_HASH:'invalid'}]) {
    assert.throws(()=>requireTelegramApplicationIdentity({...secretEnv,...change}));
  }
});
test('the official build command passes Telegram credentials to the compiler without a feature flag',()=>{
  const calls=[];
  const cleanup=[];
  const fakeModule={exports:{}};
  const load=name=>name==='node:child_process'?{spawnSync:(program,args,options)=>{
    calls.push({program,args,options}); return {status:0};
  }}:name==='node:fs'?{...fs,rmSync:(target,options)=>cleanup.push({target,options})}:require(name);
  load.main=fakeModule;
  const version=require('../src-tauri/tauri.conf.json').version;
  const env={SYNTHIUS_RELEASE_CHANNEL:'production',DENDRITE_RELEASE_TAG:`v${version}`,
    SYNTHIUS_SUPABASE_PUBLIC_KEY:'sb_publishable_offline_test_fixture',
    SYNTHIUS_TELEGRAM_API_ID:'123',SYNTHIUS_TELEGRAM_API_HASH:'a'.repeat(32)};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname,'desktop.cjs'),'utf8'),{
    require:load,module:fakeModule,__dirname,Buffer,
    process:{env,argv:['node','desktop.cjs','build'],execPath:process.execPath},
  });
  assert.equal(cleanup.length,1);
  assert.equal(cleanup[0].target,path.join(__dirname,'..','src-tauri','target','release','bundle'));
  assert.equal(cleanup[0].options.recursive,true);
  assert.equal(cleanup[0].options.force,true);
  assert.equal(calls.length,1);
  assert.equal(calls[0].options.env.SYNTHIUS_TELEGRAM_API_ID,env.SYNTHIUS_TELEGRAM_API_ID);
  assert.equal(calls[0].options.env.SYNTHIUS_SUPABASE_PUBLIC_KEY,env.SYNTHIUS_SUPABASE_PUBLIC_KEY);
  assert.equal(calls[0].options.env.SYNTHIUS_TELEGRAM_API_HASH,env.SYNTHIUS_TELEGRAM_API_HASH);
  assert.ok(calls[0].args.includes('build'));
});
test('every distribution job is owner gated and credential/write jobs await the owner environment',()=>{
  const root=path.join(__dirname,'..');
  for(const name of ['package','desktop']) {
    const workflow=fs.readFileSync(path.join(root,'.github/workflows',name+'.yml'),'utf8');
    assert.match(workflow,/workflow_dispatch:/);
    assert.doesNotMatch(workflow,/^  push:/m);
    assert.doesNotMatch(workflow,/telegram_enabled|DENDRITE_TELEGRAM_POLICY|DENDRITE_TELEGRAM_ENABLED/);
    const jobs=workflow.split(/(?=^  [a-z]+:\n)/m).filter(part=>/^  (?:preflight|validate|package|desktop|publish):\n/.test(part));
    assert.equal(jobs.length,name==='package'?4:3);
    for(const job of jobs) {
      assert.match(job,/github.actor_id == '167432977'/);
      assert.match(job,/github.triggering_actor == 'x-aero'/);
      assert.match(job,/github.ref == 'refs\/heads\/main'/);
      if(!/^  validate:/.test(job)) assert.match(job,/environment: owner-release/);
    }
    assert.match(workflow,/node scripts\/release-guard.cjs/);
    assert.match(workflow,/SYNTHIUS_TELEGRAM_API_ID: \$\{\{ secrets.SYNTHIUS_TELEGRAM_API_ID \}\}/);
    assert.match(workflow,/SYNTHIUS_TELEGRAM_API_HASH: \$\{\{ secrets.SYNTHIUS_TELEGRAM_API_HASH \}\}/);
  }
  assert.equal(fs.readFileSync(path.join(root,'.github/CODEOWNERS'),'utf8').trim().split('\n').at(-1),'* @x-aero');
  const desktop=fs.readFileSync(path.join(root,'scripts/desktop.cjs'),'utf8');
  assert.match(desktop,/execute\('cargo',\['test','--workspace','--locked'\]/);
  assert.match(desktop,/requireTelegramApplicationIdentity\(env\)/);
  assert.doesNotMatch(desktop,/telegramBuildEnvironment|TELEGRAM_POLICY|TELEGRAM_ENABLED/);
});
