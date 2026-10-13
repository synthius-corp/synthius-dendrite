// No distribution job may accept an external trigger or an unreviewed branch.
const {run}=require('./release.cjs');
const OWNER={login:'x-aero',id:'167432977'};
const REPOSITORY='synthius-corp/synthius-dendrite';
const REPOSITORY_ID='1417119504';

function assertOwnerContext(env) {
  if(env.GITHUB_REPOSITORY!==REPOSITORY || env.GITHUB_REPOSITORY_ID!==REPOSITORY_ID
    || env.GITHUB_ACTOR_ID!==OWNER.id || env.GITHUB_ACTOR!==OWNER.login
    || env.GITHUB_TRIGGERING_ACTOR!==OWNER.login || env.GITHUB_EVENT_NAME!=='workflow_dispatch'
    || env.GITHUB_REF!=='refs/heads/main' || !/^[a-f0-9]{40}$/.test(env.GITHUB_SHA || '')) {
    throw new Error('Official distributions require an owner-triggered main workflow; reruns also require the owner');
  }
}
function assertProtection(branch,environment,owners) {
  const reviews=branch.required_pull_request_reviews;
  if(!reviews?.require_code_owner_reviews || !reviews.dismiss_stale_reviews
    || reviews.required_approving_review_count<1 || !branch.required_status_checks?.strict
    || !(branch.required_status_checks.checks?.length || branch.required_status_checks.contexts?.length)
    || branch.allow_force_pushes?.enabled!==false || branch.allow_deletions?.enabled!==false) {
    throw new Error('Configure main protection, owner reviews and required CI before releasing');
  }
  assertEnvironment(environment,owners);
}
function assertEnvironment(environment,owners) {
  const lines=owners.split(/\r?\n/).map(line=>line.trim()).filter(line=>line && !line.startsWith('#'));
  if(lines.length!==1 || lines[0]!==`* @${OWNER.login}`) throw new Error('Only the verified owner may be the required code owner');
  const rule=environment.protection_rules?.find(value=>value.type==='required_reviewers');
  if(!rule || rule.prevent_self_review!==false || rule.reviewers?.length!==1
    || rule.reviewers[0].type!=='User' || String(rule.reviewers[0].reviewer?.id)!==OWNER.id
    || environment.deployment_branch_policy?.protected_branches!==true) {
    throw new Error('Configure owner-release approval by the owner and protected deployment branches');
  }
}
function verifyReleaseAuthority(env,execute=run,administrator=false) {
  assertOwnerContext(env);
  const get=endpoint=>JSON.parse(execute('gh',['api',`repos/${REPOSITORY}/${endpoint}`]));
  const main=get('branches/main');
  if(!main.protected || main.commit?.sha!==env.GITHUB_SHA) throw new Error('Release source must be the exact protected main commit');
  const environment=get('environments/owner-release');
  const file=get(`contents/.github/CODEOWNERS?ref=${env.GITHUB_SHA}`);
  const owners=Buffer.from(file.content || '', 'base64').toString('utf8');
  assertEnvironment(environment,owners);
  // The workflow token has read-only contents/actions access, not admin access.
  // Full rule readback belongs to owner setup with an administration token.
  if(administrator) assertProtection(get('branches/main/protection'),environment,owners);
}
function requireTelegramApplicationIdentity(env) {
  const id=Number(env.SYNTHIUS_TELEGRAM_API_ID);
  if(!/^\d+$/.test(env.SYNTHIUS_TELEGRAM_API_ID || '')
    || !Number.isInteger(id) || id<=0 || id>2147483647
    || !/^[a-f0-9]{32}$/i.test(env.SYNTHIUS_TELEGRAM_API_HASH || '')) {
    throw new Error('Official desktop builds require a valid Telegram application identity');
  }
  return env;
}
module.exports={OWNER,REPOSITORY,assertOwnerContext,assertProtection,assertEnvironment,verifyReleaseAuthority,requireTelegramApplicationIdentity};
if(require.main===module) verifyReleaseAuthority(process.env);
