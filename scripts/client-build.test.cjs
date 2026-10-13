const test=require('node:test');
const assert=require('node:assert/strict');
const {clientBuildPlan}=require('./client-build.cjs');
const config=require('../distribution/public-client/production.json');
test('community Production builds automatically load the public configuration and compile without official release secrets',()=>{
  const plan=clientBuildPlan();
  assert.equal(plan.env.SYNTHIUS_RELEASE_CHANNEL,'production');
  assert.equal(plan.env.SYNTHIUS_SUPABASE_URL,config.supabaseUrl);
  assert.equal(plan.env.SYNTHIUS_SUPABASE_PUBLIC_KEY,config.supabasePublicKey);
  assert.equal(plan.env.SYNTHIUS_TELEGRAM_API_ID,undefined);
  assert.equal(plan.env.SYNTHIUS_TELEGRAM_API_HASH,undefined);
  assert.ok(plan.args.includes('--no-bundle'));assert.ok(plan.args.includes('--locked'));
  assert.ok(!plan.args.includes('--debug'));
  const claims=JSON.parse(Buffer.from(config.supabasePublicKey.split('.')[1],'base64url').toString('utf8'));
  assert.equal(claims.role,'anon');
  assert.equal(claims.ref,new URL(config.supabaseUrl).hostname.split('.')[0]);
  assert.equal(claims.sub,undefined);
  assert.throws(()=>clientBuildPlan({SYNTHIUS_SUPABASE_PUBLIC_KEY:'sb_secret_private'}));
});
