const test=require('node:test');
const assert=require('node:assert/strict');
const {offlinePlan}=require('./offline-dev.cjs');
test('standalone tests/builds select local debug identity with a nonfunctional key, overriding hosted credentials',()=>{
  for(const action of ['test','build','dev']) {
    const plan=offlinePlan(action,{SYNTHIUS_RELEASE_CHANNEL:'production',SYNTHIUS_SUPABASE_URL:'https://prod.example',
      SYNTHIUS_SUPABASE_PUBLIC_KEY:'sb_secret_private',SYNTHIUS_TELEGRAM_API_ID:'123',SYNTHIUS_TELEGRAM_API_HASH:'private'});
    assert.equal(plan.env.SYNTHIUS_RELEASE_CHANNEL,'local-dev');
    assert.equal(plan.env.SYNTHIUS_SUPABASE_URL,'http://127.0.0.1:54321');
    assert.equal(plan.env.SYNTHIUS_SUPABASE_PUBLIC_KEY,'sb_publishable_offline_test_fixture');
    assert.equal(plan.env.SYNTHIUS_TELEGRAM_API_ID,undefined);
    assert.equal(plan.env.SYNTHIUS_TELEGRAM_API_HASH,undefined);
    const config=JSON.parse(plan.env.TAURI_CONFIG);
    assert.equal(config.identifier,'app.synthius.dendrite.localdev');
    assert.deepEqual(config.plugins.updater,{endpoints:[],pubkey:''});
    if(action==='build') {
      assert.ok(plan.args.includes('--debug'));assert.ok(plan.args.includes('--no-bundle'));
      assert.ok(!plan.args.includes('--offline'));assert.ok(!plan.args.includes('--release'));
    }
    if(action==='test') assert.deepEqual(plan.args,['test','--manifest-path','src-tauri/Cargo.toml','--locked']);
    assert.ok(plan.args.includes('--locked'));
  }
  assert.throws(()=>offlinePlan('release'));
  assert.equal(offlinePlan('test',{CARGO_TARGET_DIR:'custom-cache'}).env.CARGO_TARGET_DIR,'custom-cache');
});
