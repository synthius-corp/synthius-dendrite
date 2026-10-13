/* The native view uses the same technical parser and source-registration APIs
 * as the web app. Account credentials are held in Rust, never this page. */
(() => {
 const $ = id => document.getElementById(id), dialog = $('upload-dialog');
 let context = null, rows = [], busy = false, pause = false, batch = null, selected = [], duplicateCount = 0;
 const error = message => { $('upload-error').textContent = message; $('upload-error').hidden = !message; };
 function updateRetry() {
  const remaining=selected.filter(item=>!item.done).length;
  $('upload-retry').hidden=!remaining||busy;
  $('upload-retry').textContent=`Continue ${remaining} selected file${remaining===1?'':'s'}`;
 }
 function stage(name) {
  for (const part of ['browse','progress','review','complete']) $('upload-'+part).hidden = part !== name;
  $('upload-close').disabled = busy;
  updateRetry();
  if(name==='progress')$('upload-pause').textContent='Pause after this file';
 }
 async function api(path, body) {
  const response = await fetch('/api/desktop/uploads/'+path, {
   method:'POST', cache:'no-store', headers:{'X-Dendrite-Update-Token':UPDATE_TOKEN, ...(body instanceof FormData ? {} : {'Content-Type':'application/json'})},
   body: body instanceof FormData ? body : body === undefined ? undefined : JSON.stringify(body),
  });
  const value = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(typeof value.detail === 'string' ? value.detail : value.detail?.message || 'Connect your Synthius account while Dendrite is running, then try again.');
  return value;
 }
 function fillCategories(select, includeDetected=false) {
  select.replaceChildren();
  if(includeDetected){const option=document.createElement('option');option.value='';option.textContent='Keep detected categories';select.append(option);}
  for(const category of context.categories){const option=document.createElement('option');option.value=category.value;option.textContent=category.label;select.append(option);}
 }
 async function open() {
  if(busy)return;
  error(''); context=null;rows=[];stage('browse');
  $('upload-usage').value='persona_source_and_context';
  $('upload-usage-help').textContent='Persona sources join the queue and support source search.';
  dialog.showModal();
  for(const id of ['choose-files','choose-folders','upload-resume'])$(id).disabled=true;
  $('upload-persona').replaceChildren();$('upload-formats').textContent='Connecting to your Synthius account…';
  try {
   context=await api('context');
   if(context.upload_allowed===false)throw new Error('Complete your Synthius account setup before adding sources.');
   if(batch&&batch.user_id!==context.user_id){batch=null;selected=[];updateRetry();}
   for(const persona of context.personas){const option=document.createElement('option');option.value=persona.id;option.textContent=persona.name;$('upload-persona').append(option);}
   if(context.personas.length>1){const placeholder=document.createElement('option');placeholder.value='';placeholder.textContent='Choose a persona';$('upload-persona').prepend(placeholder);$('upload-persona').value='';}
   const accept=context.extensions.map(e=>'.'+e).join(',');$('upload-files').accept=accept;$('upload-folders').accept=accept;
   $('upload-formats').textContent=`${context.extensions.map(e=>e.toUpperCase()).join(' · ')} · Up to ${Math.round(context.max_file_bytes/1024/1024)} MB per file`;
   $('upload-structured-formats').textContent=context.structured_formats || 'Structured formats: Telegram JSON chats; JSON documents containing readable narrative text; Notion, LinkedIn, Instagram and Facebook ZIP exports.';
   $('upload-resume').hidden=!context.pending.length;$('upload-resume').textContent=`Continue ${context.pending.length} unfinished source${context.pending.length===1?'':'s'}`;
   for(const id of ['choose-files','choose-folders'])$(id).disabled=!$('upload-persona').value;
   $('upload-resume').disabled=false;
   if(!context.personas.length)error('Create a persona in Synthius, then return here to add its sources.');
  }catch(e){context=null;error(e.message);$('upload-retry').hidden=true;$('upload-formats').textContent='Connect your account in Synthius once. Dendrite then keeps its upload connection in this computer’s secure credential store.';}
 }
 function review() {
  busy=false;stage('review');const list=$('upload-review-list');list.replaceChildren();
  $('upload-target').textContent=`${rows.length} local source${rows.length===1?'':'s'} · ${[...new Set(rows.map(r=>context.personas.find(p=>p.id===r.persona_id)?.name||'Persona'))].join(', ')}`;
  $('upload-bulk').hidden=rows.length<2;fillCategories($('bulk-category'),true);$('bulk-subject').value='';
  for(const row of rows){
   const card=document.createElement('div');card.className='review-item';
   const title=document.createElement('div');title.className='review-name';title.textContent=row.filename;
   const hints=document.createElement('p');hints.className='review-hints';hints.textContent=`${row.subtype} · ${Math.round(row.confidence*100)}% confidence · ${row.source_usage==='context_only'?'Context only':'Persona source and context'}${row.hints?.length?' · '+row.hints.slice(0,2).join(' · '):''}`;
   const fields=document.createElement('div');fields.className='review-row';
   const label=document.createElement('label');label.className='review-field';label.textContent='Category';const category=document.createElement('select');fillCategories(category);
   category.value=context.categories.some(c=>c.value===row.category)?row.category:'notes';row.category=category.value;category.dataset.id=row.file_id;category.setAttribute('aria-label',`Category for ${row.filename}`);label.append(category);
   const subjectLabel=document.createElement('label');subjectLabel.className='review-field';subjectLabel.textContent='Person described';const subject=document.createElement('input');subject.type='text';subject.maxLength=500;subject.value=row.subject_name||context.personas.find(p=>p.id===row.persona_id)?.name||'';row.subject_name=subject.value;subject.setAttribute('aria-label',`Person described in ${row.filename}`);subjectLabel.append(subject);
   function required(){const needs=context.categories.find(c=>c.value===row.category)?.needs_subject_name;subjectLabel.hidden=!needs;subject.required=!!needs;}
   category.addEventListener('change',()=>{row.category=category.value;required();});subject.addEventListener('input',()=>{row.subject_name=subject.value;});required();
   fields.append(label,subjectLabel);card.append(title,hints,fields);list.append(card);
  }
 }
 async function choose(files) {
  if(busy||!context||!files.length)return;
  error('');if(!$('upload-persona').value){error('Choose a persona for these sources.');return;}
  if(selected.some(item=>!item.done)){error('Continue the selected files before starting another batch.');return;}
  if(files.length>500){error('Select up to 500 files per batch.');return;}
  const normalized=SynthiusSourceSelection.normalizeSelectedUploadFiles(Array.from(files));
  const accepted=normalized.filter(file=>file.size<=context.max_file_bytes&&context.extensions.includes(file.name.split('.').pop().toLowerCase()));
  const rejected=normalized.length-accepted.length;
  if(rejected)error(`${rejected} file${rejected===1?' was':'s were'} skipped. Choose supported formats, up to 50 MB each.`);
  if(!accepted.length)return;
  batch={id:crypto.randomUUID(),user_id:context.user_id,persona_id:$('upload-persona').value,source_usage:$('upload-usage').value};
  selected=accepted.map(file=>({file,id:crypto.randomUUID(),done:false}));rows=[];duplicateCount=0;
  await storeFiles();
 }
 async function storeFiles() {
  busy=true;pause=false;stage('progress');const failures=[];
  for(let i=0;i<selected.length;i++){
   const item=selected[i];if(item.done)continue;if(pause)break;
   $('upload-progress-label').textContent=`Saving and classifying ${i+1} of ${selected.length}: ${item.file.name}`;$('upload-progress-bar').value=100*i/selected.length;
   const form=new FormData();for(const [key,value]of Object.entries({user_id:batch.user_id,persona_id:batch.persona_id,batch_id:batch.id,file_id:item.id,source_usage:batch.source_usage,filename:item.file.name}))form.append(key,value);form.append('file',item.file,item.file.name);
   try{const row=await api('add',form);item.done=true;if(row.duplicate)duplicateCount++;if(!rows.some(existing=>existing.file_id===row.file_id)&&!row.complete)rows.push(row);}catch(e){failures.push(`${item.file.name}: ${e.message}`);}
  }
  busy=false;$('upload-progress-bar').value=100;
  if(failures.length)error(failures.join('\n'));
  if(rows.length)review();else if(duplicateCount){stage('complete');$('upload-result').textContent=`${duplicateCount} selected source${duplicateCount===1?' is':'s are'} already in Sources`;}else{stage('browse');$('upload-resume').hidden=false;$('upload-resume').disabled=false;}
  // Files which failed before registration stay selected with the same IDs.
  // Resume first reconciles the durable journal, then retries untouched files.
 }
 async function resume() {
  if(busy||!context)return;error('');
  try {
   const current=await api('context');
   if(current.user_id!==context.user_id)throw new Error('The connected account changed. Close and reopen Add sources.');
   context=current;rows=current.pending;
   if(selected.some(item=>!item.done)){await storeFiles();const refreshed=await api('context');rows=refreshed.pending;}
   if(rows.length)review();else error('All saved sources have finished parsing.');
  }catch(e){error(e.message);}
 }
 async function parse() {
  if(busy||!rows.length)return;error('');
  const incomplete=rows.find(row=>context.categories.find(c=>c.value===row.category)?.needs_subject_name&&!row.subject_name?.trim());
  if(incomplete){error(`Enter the name of the person described in ${incomplete.filename}.`);return;}
  busy=true;pause=false;stage('progress');let completed=0;const remaining=[],failures=[];
  for(let i=0;i<rows.length;i++){
   const row=rows[i];if(pause){remaining.push(...rows.slice(i));break;}
   $('upload-progress-label').textContent=`Parsing ${i+1} of ${rows.length}: ${row.filename}`;$('upload-progress-bar').value=100*i/rows.length;
   try{const result=await api('parse',{user_id:context.user_id,file_id:row.file_id,category:row.category,subject_name:row.subject_name||''});if(result.duplicate)duplicateCount++;else completed++;}
   catch(e){remaining.push(row);failures.push(`${row.filename}: ${e.message}`);}
  }
  busy=false;rows=remaining;await refreshCatalog();await refresh();
  if(rows.length){review();if(failures.length)error(failures.join('\n'));}
  else{stage('complete');$('upload-result').textContent=`${completed} local source${completed===1?' is':'s are'} ready${duplicateCount?` · ${duplicateCount} already in Sources`:''}`;}
 }
 for(const id of ['add-local','add-local-sources','upload-again'])$(id).addEventListener('click',open);
 for(const id of ['upload-close','upload-later','upload-done'])$(id).addEventListener('click',()=>{if(!busy)dialog.close();});
 dialog.addEventListener('cancel',event=>{if(busy){event.preventDefault();pause=true;}});
 $('upload-pause').addEventListener('click',()=>{pause=true;$('upload-pause').textContent='Pausing after this file…';});
 $('upload-resume').addEventListener('click',resume);$('upload-confirm').addEventListener('click',parse);
 $('upload-retry').addEventListener('click',()=>{if(!busy&&context)storeFiles().catch(e=>error(e.message));});
 $('choose-files').addEventListener('click',()=>$('upload-files').click());$('choose-folders').addEventListener('click',()=>$('upload-folders').click());
 for(const id of ['upload-files','upload-folders'])$(id).addEventListener('change',event=>{choose(event.target.files).catch(e=>error(e.message));event.target.value='';});
 $('upload-persona').addEventListener('change',()=>{for(const id of ['choose-files','choose-folders'])$(id).disabled=!$('upload-persona').value;});
 $('upload-usage').addEventListener('change',()=>{$('upload-usage-help').textContent=$('upload-usage').value==='context_only'?'Context-only files support source search and stay outside the persona-processing queue.':'Persona sources join the queue and support source search.';});
 $('bulk-apply').addEventListener('click',()=>{const category=$('bulk-category').value,subject=$('bulk-subject').value.trim();for(const row of rows){if(category)row.category=category;if(subject)row.subject_name=subject;}review();});
 const drop=$('upload-drop');for(const type of ['dragenter','dragover'])drop.addEventListener(type,event=>{event.preventDefault();drop.classList.add('dragging');});drop.addEventListener('dragleave',()=>drop.classList.remove('dragging'));
 drop.addEventListener('drop',async event=>{event.preventDefault();drop.classList.remove('dragging');try{const files=await SynthiusSourceSelection.collectDroppedUploadFiles(event.dataTransfer);await choose(files);}catch(e){error(e.message);}});
 // Keep ordinary document drops inside Dendrite instead of navigating the WebView.
 for(const type of ['dragover','drop'])document.addEventListener(type,event=>event.preventDefault());
})();
