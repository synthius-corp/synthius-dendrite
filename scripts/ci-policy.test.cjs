const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const {documentationOnly,planCI,assertCurrentPull,assertCIAccepted,assertValidatedMainCI,verifyReusableCI,acceptCI,CI_JOBS}=require('./ci-policy.cjs');
const env={GITHUB_REPOSITORY:'synthius-corp/synthius-dendrite',GITHUB_REPOSITORY_ID:'1417119504',GITHUB_ACTOR:'x-aero',GITHUB_ACTOR_ID:'167432977',GITHUB_TRIGGERING_ACTOR:'x-aero',GITHUB_EVENT_NAME:'workflow_dispatch',GITHUB_REF:'refs/heads/main',GITHUB_SHA:'a'.repeat(40),GITHUB_WORKFLOW:'Dendrite CI',GITHUB_RUN_ID:'10'};
const pull={number:1,state:'open',changed_files:1,base:{sha:'b'.repeat(40),ref:'main',repo:{full_name:env.GITHUB_REPOSITORY}},head:{sha:'c'.repeat(40)}},event={number:1,pull_request:pull};
const validated={status:'completed',conclusion:'success',path:'.github/workflows/ci.yml',event:'workflow_dispatch',head_branch:'main',head_sha:env.GITHUB_SHA,head_repository:{full_name:env.GITHUB_REPOSITORY},actor:{id:167432977,login:'x-aero'},triggering_actor:{login:'x-aero'}};
const jobs={total_count:6,jobs:[...CI_JOBS,'plan','CI acceptance'].map(name=>({name,conclusion:'success'}))};
test('complete documentation diffs skip native tests; code renames and unknown paths fail closed',()=>{
  assert.equal(documentationOnly([{filename:'README.md'},{filename:'docs/ADDING_FORMAT.md'}],true),true);
  for(const filename of ['src-tauri/src/lib.rs','Cargo.lock','ui/uploads.js','.github/workflows/ci.yml','docs/example.zip','schemas/synthius-corpus-v1.schema.json','docs/../src.rs'])assert.equal(documentationOnly([{filename}],true),false);
  assert.equal(documentationOnly([{filename:'docs/a.md',previous_filename:'src-tauri/src/lib.rs'}],true),false);
  assert.equal(documentationOnly([{filename:'README.md'}],false),false);assert.equal(documentationOnly([],true),false);
});
test('PR planning requires current, complete head/base metadata and never executes PR code',()=>{
  const execute=(p,a)=>JSON.stringify(a[1].includes('/compare/')?{files:[{filename:'README.md'}]}:pull);
  assert.equal(planCI({...env,GITHUB_EVENT_NAME:'pull_request'},event,execute).native,false);
  assert.equal(planCI({...env,GITHUB_EVENT_NAME:'pull_request'},event,(p,a)=>a[1].includes('/compare/')?JSON.stringify({files:[{filename:'README.md'}]}):JSON.stringify({...pull,changed_files:301})).native,true);
  assert.throws(()=>assertCurrentPull(event,{...pull,head:{sha:'d'.repeat(40)}}),/Stale/);
  assert.throws(()=>assertCurrentPull(event,{...pull,base:{...pull.base,sha:'d'.repeat(40)}}),/Stale/);
  assert.throws(()=>planCI({...env,GITHUB_EVENT_NAME:'pull_request'},{...event,number:'1/invalid'},()=>{throw Error('network called');}),/Invalid pull/);
  assert.throws(()=>assertCurrentPull(event,{...pull,number:2}),/Stale/);
});
test('Dependabot cannot start tests without the owner authorizing its rerun',()=>{
  const proposal={...event,pull_request:{...pull,user:{login:'dependabot[bot]'}}};
  const context={...env,GITHUB_EVENT_NAME:'pull_request',GITHUB_ACTOR:'dependabot[bot]'};
  assert.throws(()=>planCI({...context,GITHUB_TRIGGERING_ACTOR:'dependabot[bot]'},proposal,()=>{throw Error('network called');}),/owner-authorized/);
  const execute=(p,a)=>JSON.stringify(a[1].includes('/compare/')?{files:[{filename:'Cargo.lock'}]}:proposal.pull_request);
  assert.equal(planCI(context,proposal,execute).native,true);
});
test('manual CI is owner/main only; policy-only scope cannot reach release callers',()=>{
  assert.equal(planCI(env,{},()=>{throw Error('network');}).native,true);
  assert.equal(planCI({...env,DENDRITE_CI_SCOPE:'policy'},{},()=>{}).native,false);
  for(const changed of [{GITHUB_ACTOR_ID:'2'},{GITHUB_TRIGGERING_ACTOR:'outside'},{GITHUB_REF:'refs/heads/feature'},{DENDRITE_CI_SCOPE:'policy',GITHUB_WORKFLOW:'Release Dendrite'}])assert.throws(()=>planCI({...env,...changed},{},()=>{}));
});
test('acceptance blocks failed, cancelled, missing and unexpectedly skipped native checks',()=>{
  assertCIAccepted('success','true','success');assertCIAccepted('success','false','skipped');
  for(const args of [['failure','true','success'],['cancelled','false','skipped'],['success','true','skipped'],['success','true','failure'],['success','','success'],['success','false','success']])assert.throws(()=>assertCIAccepted(...args));
  assert.throws(()=>acceptCI({...env,DENDRITE_PLAN_RESULT:'success',DENDRITE_NATIVE_REQUIRED:'true',DENDRITE_NATIVE_RESULT:'success'},{},()=>JSON.stringify({...jobs,jobs:jobs.jobs.slice(1)})),/four native/);
});
test('release reuse requires exact source/all platforms and rejects policy-only, PR, fork and failed runs',()=>{
  assertValidatedMainCI(validated,jobs,env.GITHUB_REPOSITORY,env.GITHUB_SHA);
  for(const change of [{event:'pull_request'},{conclusion:'failure'},{head_sha:'b'.repeat(40)},{head_branch:'feature'},{actor:{id:2,login:'outside'}},{triggering_actor:{login:'outside'}},{path:'.github/workflows/package.yml'}])assert.throws(()=>assertValidatedMainCI({...validated,...change},jobs,env.GITHUB_REPOSITORY,env.GITHUB_SHA));
  assert.throws(()=>assertValidatedMainCI(validated,{total_count:3,jobs:[jobs.jobs[4],jobs.jobs[5],{name:'rust',conclusion:'skipped'}]}));
  for(const index of [0,5])assert.throws(()=>assertValidatedMainCI(validated,{...jobs,jobs:jobs.jobs.map((j,i)=>i===index?{...j,conclusion:'failure'}:j)}));
});
test('release-called CI accepts its namespaced native jobs without accepting unrelated jobs',()=>{
  const context={...env,GITHUB_WORKFLOW:'Release Dendrite (Production)',DENDRITE_PLAN_RESULT:'success',DENDRITE_NATIVE_REQUIRED:'true',DENDRITE_NATIVE_RESULT:'success'};
  const called={jobs:CI_JOBS.map(name=>({name:`validate / ${name}`,conclusion:'success'}))};
  acceptCI(context,{},()=>JSON.stringify(called));
  assert.throws(()=>acceptCI(context,{},()=>JSON.stringify(jobs)),/four native/);
  assert.throws(()=>acceptCI(context,{},()=>JSON.stringify({jobs:called.jobs.slice(1)})),/four native/);
  assert.throws(()=>assertValidatedMainCI(validated,{...jobs,jobs:jobs.jobs.map(job=>({...job,name:`validate / ${job.name}`}))}),/four native/);
});
test('optional reuse performs no API without an ID and rejects outsiders before any network access',()=>{
  const execute=(p,a)=>JSON.stringify(a[1].includes('/jobs?')?jobs:validated);
  assert.equal(verifyReusableCI(env,()=>{throw Error('network');}),false);
  assert.equal(verifyReusableCI({...env,DENDRITE_VALIDATED_RUN_ID:'123'},execute),true);
  assert.throws(()=>verifyReusableCI({...env,GITHUB_ACTOR_ID:'2',DENDRITE_VALIDATED_RUN_ID:'123'},()=>{throw Error('network called');}),/owner-triggered/);
});
test('workflow cancels stale PR tests, avoids main-push duplication and preserves a stable bounded acceptance gate',()=>{
  const read=name=>fs.readFileSync(path.join(__dirname,'../',name),'utf8');const yaml=read('.github/workflows/ci.yml');
  assert.doesNotMatch(yaml,/^  push:|pull_request_target:/m);assert.match(yaml,/cancel-in-progress:.*pull_request/);
  assert.match(yaml,/name: CI acceptance/);assert.match(yaml,/timeout-minutes: 45/);assert.match(yaml,/github.actor_id == '167432977'/);
  assert.match(yaml,/ref:.*pull_request.base.sha/);assert.match(yaml,/needs.plan.outputs.native == 'true'/);
  assert.ok(yaml.includes("github.event.pull_request.user.login != 'dependabot[bot]' || github.triggering_actor == 'x-aero'"));
  assert.deepEqual(JSON.parse(read('distribution/github-main-protection.json')).required_status_checks.checks,[{context:'CI acceptance',app_id:15368}]);
  for(const name of ['package','desktop']){const source=read(`.github/workflows/${name}.yml`);assert.match(source,/validation_run_id:/);assert.match(source,/node scripts\/ci-policy.cjs reuse/);assert.match(source,/needs.preflight.outputs.reuse_ci/);}
});
