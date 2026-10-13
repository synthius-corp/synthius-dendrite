const fs=require('node:fs');
const {run}=require('./release.cjs');
const {assertOwnerContext,OWNER,REPOSITORY}=require('./release-guard.cjs');
const CI_JOBS=['rust (ubuntu-22.04)','rust (windows-latest)','rust (macos-15)','rust (macos-15-intel)'];
const SHA=/^[a-f0-9]{40}$/;
const DOCS=new Set(['README.md','CONTRIBUTING.md','DEVELOPMENT.md','RELEASING.md','SECURITY.md',
  'BUILD_DISTRIBUTION.md','LICENSE','schemas/README.md','ui/README.md','dendrite-parser/README.md','dendrite-telegram/README.md','.github/PULL_REQUEST_TEMPLATE.md']);
function documentationOnly(files,complete) {
  return complete===true && Array.isArray(files) && files.length>0 && files.every(file=> {
    const names=[file.filename,...(file.previous_filename?[file.previous_filename]:[])];
    return names.every(name=>typeof name==='string' && !name.includes('\0') && !name.split('/').includes('..')
      && (DOCS.has(name) || (name.startsWith('docs/') && name.endsWith('.md'))));
  });
}
function assertCurrentPull(event,current) {
  const pull=event.pull_request;
  if (!Number.isSafeInteger(event.number) || event.number<=0 || pull?.number!==event.number || current.number!==event.number
    || !SHA.test(pull?.head?.sha||'') || !SHA.test(pull?.base?.sha||'')
    || pull.base.ref!=='main' || pull.base.repo?.full_name!==REPOSITORY || current.state!=='open'
    || current.base?.ref!=='main' || current.base?.repo?.full_name!==REPOSITORY
    || current.head?.sha!==pull.head.sha || current.base?.sha!==pull.base.sha) throw new Error('Stale or invalid pull request; test its current head/base');
}
function api(env,execute=run) {
  if (env.GITHUB_REPOSITORY!==REPOSITORY || env.GITHUB_REPOSITORY_ID!=='1417119504') throw new Error('Canonical CI repository required');
  return suffix=>JSON.parse(execute('gh',['api',`repos/${REPOSITORY}${suffix}`]));
}
function planCI(env,event,execute=run) {
  const get=api(env,execute);
  if (env.GITHUB_EVENT_NAME==='pull_request') {
    if (event.pull_request?.user?.login==='dependabot[bot]' && env.GITHUB_TRIGGERING_ACTOR!==OWNER.login) {
      throw new Error('Dependabot CI requires an owner-authorized rerun');
    }
    if (!Number.isSafeInteger(event.number) || event.number<=0) throw new Error('Invalid pull request number');
    const current=get(`/pulls/${event.number}`);assertCurrentPull(event,current);
    const compare=get(`/compare/${current.base.sha}...${current.head.sha}`);
    const files=compare.files;
    const complete=Array.isArray(files) && files.length<300 && Number.isSafeInteger(current.changed_files)
      && files.length===current.changed_files;
    return {native:!documentationOnly(files,complete),reason:complete?'changed-files':'incomplete-diff'};
  }
  assertOwnerContext(env);
  const scope=env.DENDRITE_CI_SCOPE || 'full';
  if (!['full','policy'].includes(scope) || (scope==='policy' && env.GITHUB_WORKFLOW!=='Dendrite CI')) throw new Error('Invalid CI scope');
  return {native:scope==='full',reason:scope};
}
function assertCIAccepted(plan,nativeRequired,nativeResult) {
  if (plan!=='success' || !((nativeRequired==='true' && nativeResult==='success')
    || (nativeRequired==='false' && nativeResult==='skipped'))) throw new Error('Required CI checks did not pass');
}
function assertNativeJobs(jobs,prefix='') {
  if (!Array.isArray(jobs.jobs) || CI_JOBS.some(name=>!jobs.jobs.some(job=>job.name===prefix+name && job.conclusion==='success'))) throw new Error('All four native platforms must pass');
}
function assertValidatedMainCI(validation,jobs,repository=REPOSITORY,source) {
  if (repository!==REPOSITORY || validation.status!=='completed' || validation.conclusion!=='success'
    || validation.path!=='.github/workflows/ci.yml' || !['push','workflow_dispatch'].includes(validation.event)
    || validation.head_branch!=='main' || validation.head_repository?.full_name!==repository
    || !SHA.test(validation.head_sha||'') || (source && validation.head_sha!==source)
    || jobs.jobs?.length!==jobs.total_count || ![4,6].includes(jobs.total_count)) throw new Error('Reuse only completed successful four-platform main CI from this repository and source');
  if (validation.event==='workflow_dispatch' && (validation.actor?.id!==Number(OWNER.id)
    || validation.actor?.login!==OWNER.login || validation.triggering_actor?.login!==OWNER.login)) throw new Error('Reusable manual CI requires owner authority');
  assertNativeJobs(jobs);
  if (jobs.total_count===6 && ['plan','CI acceptance'].some(name=>!jobs.jobs.some(job=>job.name===name && job.conclusion==='success'))) throw new Error('Reusable CI requires its planning and acceptance checks');
}
function verifyReusableCI(env,execute=run) {
  assertOwnerContext(env);
  if (!env.DENDRITE_VALIDATED_RUN_ID) return false;
  if (!/^[1-9]\d{0,19}$/.test(env.DENDRITE_VALIDATED_RUN_ID)) throw new Error('Invalid CI run ID');
  const get=api(env,execute),id=env.DENDRITE_VALIDATED_RUN_ID;
  assertValidatedMainCI(get(`/actions/runs/${id}`),get(`/actions/runs/${id}/jobs?per_page=100`),REPOSITORY,env.GITHUB_SHA);
  return true;
}
function acceptCI(env,event,execute=run) {
  assertCIAccepted(env.DENDRITE_PLAN_RESULT,env.DENDRITE_NATIVE_REQUIRED,env.DENDRITE_NATIVE_RESULT);
  const get=api(env,execute);
  if (env.GITHUB_EVENT_NAME==='pull_request') assertCurrentPull(event,get(`/pulls/${event.number}`));
  else assertOwnerContext(env);
  // GitHub names called-workflow jobs with the caller job's prefix.
  const prefix=env.GITHUB_WORKFLOW==='Release Dendrite (Production)'?'validate / ':'';
  if (env.DENDRITE_NATIVE_REQUIRED==='true') assertNativeJobs(get(`/actions/runs/${env.GITHUB_RUN_ID}/jobs?per_page=100`),prefix);
}
module.exports={documentationOnly,assertCurrentPull,planCI,assertCIAccepted,assertNativeJobs,assertValidatedMainCI,verifyReusableCI,acceptCI,CI_JOBS};
if (require.main===module) {
  const env=process.env,command=process.argv[2];
  if (command==='reuse') fs.appendFileSync(env.GITHUB_OUTPUT,`reuse_ci=${verifyReusableCI(env)}\n`);
  else {
    const event=JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH,'utf8'));
    if (command==='plan') {const result=planCI(env,event);fs.appendFileSync(env.GITHUB_OUTPUT,`native=${result.native}\n`);console.log(`CI plan: ${result.reason}; native=${result.native}`);}
    else if (command==='acceptance') acceptCI(env,event);
    else throw new Error('Unknown CI policy command');
  }
}
