const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path');
const {selectLicenses,prepareLicenses,verifyLicenses,copyLicenses}=require('./licenses.cjs');
const ROOT=path.resolve(__dirname,'..');
test('license selection preserves required AND/exception terms and chooses permissive dual-license options',()=>{
  assert.deepEqual(selectLicenses('(Apache-2.0 OR MIT) AND BSD-3-Clause'),['Apache-2.0','BSD-3-Clause']);
  assert.deepEqual(selectLicenses('MIT OR Apache-2.0 OR LGPL-2.1-or-later'),['Apache-2.0']);
  assert.deepEqual(selectLicenses('Apache-2.0 WITH LLVM-exception'),['Apache-2.0','LLVM-exception']);
  assert.deepEqual(selectLicenses('MIT/Apache-2.0'),['Apache-2.0']);
  assert.deepEqual(selectLicenses('MPL-2.0'),['MPL-2.0']);
  for(const value of ['', 'MIT AND', '(MIT OR Apache-2.0'])assert.throws(()=>selectLicenses(value));
});
test('distribution notices retain copyright, license copies and exact MPL source references; stale inputs fail',()=>{
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'dendrite-notices-'));
  try {
    for(const dir of ['ui','src-tauri','distribution/license-texts','fixture-crate/src'])fs.mkdirSync(path.join(root,dir),{recursive:true});
    for(const name of ['LICENSE','Cargo.lock','src-tauri/Cargo.lock'])fs.writeFileSync(path.join(root,name),`Synthetic ${name}\n`);
    fs.writeFileSync(path.join(root,'distribution/license-texts/MPL-2.0.txt'),'Synthetic MPL test text\n');
    fs.writeFileSync(path.join(root,'fixture-crate/LICENSE'),'Original synthetic fixture license\n');
    fs.writeFileSync(path.join(root,'fixture-crate/src/lib.rs'),'// Copyright 2026 Synthetic Fixture Authors\n');
    const packages=[{name:'fixture',version:'1.2.3',license:'MPL-2.0',authors:['Synthetic Fixture Authors'],source:'registry+test',manifest_path:path.join(root,'fixture-crate/Cargo.toml')}];
    const components=prepareLicenses(root,packages);assert.equal(components.length,1);
    assert.deepEqual(components[0].copyright,['Copyright 2026 Synthetic Fixture Authors']);
    const dir=verifyLicenses(root),notices=fs.readFileSync(path.join(dir,'THIRD_PARTY_NOTICES.txt'),'utf8');
    assert.match(notices,/Original synthetic fixture license/);assert.match(notices,/Synthetic MPL test text/);
    assert.match(notices,/https:\/\/static.crates.io\/crates\/fixture\/fixture-1.2.3.crate/);
    const destination=path.join(root,'package');copyLicenses(destination,root);
    assert.equal(fs.readFileSync(path.join(destination,'licenses/LICENSE.txt'),'utf8'),'Synthetic LICENSE\n');
    fs.appendFileSync(path.join(dir,'THIRD_PARTY_NOTICES.txt'),'tampered');assert.throws(()=>verifyLicenses(root),/bundle changed/);
    prepareLicenses(root,packages);fs.appendFileSync(path.join(root,'Cargo.lock'),'changed');assert.throws(()=>verifyLicenses(root),/locks changed/);
  } finally {fs.rmSync(root,{recursive:true,force:true});}
});
test('future parser/UI/desktop distributions include the checked license bundle without compiling',()=>{
  verifyLicenses();
  const read=name=>fs.readFileSync(path.join(ROOT,name),'utf8');
  assert.equal(read('ui/LICENSE'),read('LICENSE'));
  assert.ok(JSON.parse(read('ui/package.json')).files.includes('LICENSE'));
  assert.deepEqual(JSON.parse(read('src-tauri/tauri.conf.json')).bundle.resources,{'../distribution/licenses/':'licenses/'});
  assert.match(read('scripts/release.cjs'),/copyLicenses\(pair\)/);
  assert.match(read('scripts/release.cjs'),/bin,bin\+'\.sha256','licenses'/);
  assert.match(read('scripts/desktop.cjs'),/require\('\.\/licenses.cjs'\).verifyLicenses\(\)/);
});
