const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const html = fs.readFileSync(path.join(__dirname, 'telegram.html'), 'utf8');
const script = fs.readFileSync(path.join(__dirname, 'telegram.js'), 'utf8');
function setup() {
  const element = () => ({ value:'', hidden:false, disabled:false, children:[], textContent:'',
    className:'', attributes:{}, replaceChildren(...children) { this.children=children; }, append(...children) { this.children.push(...children); },
    setAttribute(name,value) { this.attributes[name]=value; }, removeAttribute(name) { delete this.attributes[name]; } });
  const elements = new Map([...html.matchAll(/id="([^"]+)"/g)].map(([,id])=>[id,element()]));
  let state = { phase:'consent', job_id:'11111111-1111-4111-8111-111111111111', persona_name:'Synthetic', source_usage:'context_only', pages:0, sources:0 };
  const calls = []; let action = async name => { state={...state,phase:name==='telegram_native_discard'?'declined':'login'}; };
  let queryResult=[];
  const context = vm.createContext({ document: { getElementById:id=>elements.get(id), createElement:element, querySelectorAll:()=>queryResult },
    window: { confirm:()=>true, __TAURI__:{core:{invoke:async(name,args)=>{
      if(name==='telegram_native_state') return state;
      calls.push({name,args}); return await action(name,args);
    }}}}, Date, btoa:text=>Buffer.from(text).toString('base64'),
    setTimeout:()=>1,clearTimeout:()=>{},setInterval:()=>1,clearInterval:()=>{},
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname,'telegram-errors.js'),'utf8'),context);
  vm.runInContext(script,context);
  return { context,elements,calls, setState:view=>{state={...state,...view};context.render(state);}, setAction:fn=>{action=fn;}, setQueryResult:value=>{queryResult=value;} };
}
const tick = ()=>new Promise(resolve=>setImmediate(resolve));
test('first-use explanation appears before login and requires an explicit native acceptance',async()=>{
  const h=setup();await tick();
  assert.equal(h.elements.get('feature-consent').hidden,false);
  assert.equal(h.elements.get('login').hidden,true);
  assert.equal(h.elements.get('selection').hidden,true);
  assert.equal(h.calls.length,0);
  h.elements.get('export').onclick();h.elements.get('resume').onclick();assert.equal(h.calls.length,0);
  await h.elements.get('consent-accept').onclick();
  assert.equal(h.calls[0].name,'telegram_native_accept_consent');
  assert.equal(h.calls[0].args.jobId,'11111111-1111-4111-8111-111111111111');
  assert.equal(h.elements.get('feature-consent').hidden,true);
  assert.equal(h.elements.get('login').hidden,false);
  assert.match(html,/not used to train LLMs/);
  assert.doesNotMatch(script,/localStorage|sessionStorage|\bfetch\s*\(/);
});
test('declining writes no acceptance and starts no Telegram login',async()=>{
  const h=setup();await tick();await h.elements.get('consent-decline').onclick();
  assert.deepEqual(h.calls.map(c=>c.name),['telegram_native_discard']);
  assert.equal(h.elements.get('login').hidden,true);
  assert.match(h.elements.get('done-text').textContent,/when you want/);
});
test('failed storage stays on consent; duplicate acceptance is suppressed; retry is possible',async()=>{
  const h=setup();await tick();let reject;
  h.setAction(()=>new Promise((_,r)=>{reject=r;}));
  const pending=h.elements.get('consent-accept').onclick();
  await h.elements.get('consent-accept').onclick();assert.equal(h.calls.length,1);
  assert.equal(h.elements.get('consent-accept').disabled,true);
  reject('consent_unavailable');await pending;
  assert.equal(h.elements.get('feature-consent').hidden,false);
  assert.match(h.elements.get('error').textContent,/could not be saved or checked/);
  assert.equal(h.elements.get('consent-accept').disabled,false);
  h.setAction(async()=>{});await h.elements.get('consent-accept').onclick();assert.equal(h.calls.length,2);
});
test('returning account skips consent; stale async result cannot overwrite a reopened window',async()=>{
  const h=setup();await tick();let resolve;
  h.setAction(()=>new Promise(r=>{resolve=r;}));
  const pending=h.elements.get('consent-accept').onclick();
  h.setState({phase:'login',job_id:'22222222-2222-4222-8222-222222222222'});
  h.context.restartImport();await tick();resolve();await pending;
  assert.equal(h.elements.get('feature-consent').hidden,true);
  h.setAction(async()=>{});await h.context.send({type:'qr'});
  assert.equal(h.calls.at(-1).args.jobId,'22222222-2222-4222-8222-222222222222');
});
test('Telegram connectivity failures provide retryable, accurate guidance',async()=>{
  const h=setup();await tick();h.setState({phase:'login'});
  h.context.showError('telegram_network_blocked');
  assert.match(h.elements.get('error').textContent,/open it normally/);
  assert.doesNotMatch(h.elements.get('error').textContent,/paused safely/);
  h.context.showError('telegram_unavailable');
  assert.match(h.elements.get('error').textContent,/Show QR code again/);
  h.context.showError('unknown_safe_code');
  assert.match(h.elements.get('error').textContent,/Try again/);
  assert.doesNotMatch(h.elements.get('error').textContent,/Return to Sources/);
});
test('completed download is registered natively and links to Sources for the selected persona',async()=>{
  const h=setup();await tick();
  h.setAction(async name=>name==='telegram_native_register'?{sources:1,registered:1,persona_id:'22222222-2222-4222-8222-222222222222',persona_name:'Synthetic'}:undefined);
  h.setState({phase:'complete',sources:1,persona_name:'Synthetic'});await tick();
  assert.equal(h.calls.filter(call=>call.name==='telegram_native_register').length,1);
  assert.match(h.elements.get('done-text').textContent,/downloaded successfully and prepared as Local source/);
  assert.equal(h.elements.get('done-persona').hidden,false);
  assert.equal(h.elements.get('done-close').hidden,false);
  assert.match(h.elements.get('done-persona').textContent,/Continue with Synthetic/);
  await h.elements.get('done-persona').onclick();
  assert.equal(h.calls.at(-1).name,'telegram_native_open_sources');
  assert.equal(h.calls.at(-1).args.personaId,'22222222-2222-4222-8222-222222222222');
  h.setState({phase:'complete'});await tick();
  assert.equal(h.calls.filter(call=>call.name==='telegram_native_register').length,1,'render polling does not duplicate registration');
  assert.doesNotMatch(html,/register your files|Add snapshot files/);
});
test('selected chats and current-chat pages have separate determinate progress',async()=>{
  const h=setup();await tick();h.setState({phase:'exporting',scope:{chats:[{}, {}, {}],from:1,until:2},chats_total:3,chats_completed:1,current_chat:2,current_chat_pages:60,total_pages:125});
  assert.equal(h.elements.get('chat-counts').textContent,'1 of 3 selected chats saved');
  assert.equal(h.elements.get('chat-progress').max,3);
  assert.equal(h.elements.get('chat-progress').value,1);
  assert.equal(h.elements.get('counts').textContent,'Chat 2 of 3: 60 of about 125 history pages saved');
  assert.equal(h.elements.get('page-progress').max,125);
  assert.equal(h.elements.get('page-progress').value,60);
});
test('snapshot dates default to the previous three calendar months through today',async()=>{
  const h=setup();await tick();
  assert.equal(h.context.monthsBefore(new Date('2026-05-31T12:00:00Z'),3).toISOString().slice(0,10),'2026-02-28');
  const today=new Date().toISOString().slice(0,10);
  assert.equal(h.elements.get('until').value,today);
  assert.equal(h.elements.get('from').value,h.context.monthsBefore(new Date(),3).toISOString().slice(0,10));
  assert.match(html,/Messages and captions are saved as Local source files/);
  assert.match(html,/class="action-buttons"/);
});
test('chat catalogue explains metadata-only loading and keeps forum controls aligned',async()=>{
  const h=setup();await tick();
  h.setState({phase:'select',event:{type:'catalog',truncated:false,dialogs:[{id:42,title:'Forum',available:true,forum:true}]}});
  const row=h.elements.get('chats').children[0];
  assert.equal(row.className,'chat');
  assert.equal(row.children[1].className,'topic-controls');
  assert.equal(row.children[1].children[0].textContent,'Load topics');
  assert.match(html,/does not download any messages/);
  assert.match(html,/Refresh chat list/);
});
test('selection prevents a twenty-first chat and disables import for invalid dates',async()=>{
  const h=setup();await tick();
  const boxes=Array.from({length:21},(_,index)=>({checked:index<19,disabled:false,_selectable:true,value:String(index+1)}));
  h.setQueryResult(boxes);h.setState({phase:'select'});
  assert.equal(h.elements.get('export').disabled,false);
  boxes[19].checked=true;h.context.updateSelectionControls();
  assert.equal(boxes[20].disabled,true);
  assert.equal(h.elements.get('selection-note').textContent,'20 of 20 chats or topics selected. Deselect one to choose another.');
  boxes[19].checked=false;h.context.updateSelectionControls();
  assert.equal(boxes[20].disabled,false);
  h.elements.get('from').value='2099-01-01';h.context.updateSelectionControls();
  assert.equal(h.elements.get('export').disabled,true);
});
