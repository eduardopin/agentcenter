import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,readFile,readdir,rm} from 'node:fs/promises';
import {join} from 'node:path';
import {createConnection} from 'node:net';
import {randomUUID} from 'node:crypto';
const SDK=process.env.AGENTCENTER_GUARDED_PI_SDK;
assert.ok(SDK, 'Set AGENTCENTER_GUARDED_PI_SDK to a prepared private runtime');
const EXTENSION=new URL('./index.ts',import.meta.url).pathname;
const {createAgentSession,ModelRuntime,AuthStorage,SessionManager,SettingsManager,DefaultResourceLoader}=Object.assign({},...await Promise.all(['sdk','model-runtime','auth-storage','session-manager','settings-manager','resource-loader'].map(n=>import(`${SDK}/dist/core/${n}.js`))));
const {createAssistantMessageEventStream}=await import(`${SDK}/node_modules/@earendil-works/pi-ai/dist/index.js`);
globalThis.fetch=()=>{throw Error('NETWORK FORBIDDEN')};
async function setup() {
 const root=await mkdtemp(`/run/user/${process.getuid()}/cc1-verif-`); const runtimeDir=join(root,'runtime'); await mkdir(runtimeDir,{mode:0o700});
 const old={XDG_RUNTIME_DIR:process.env.XDG_RUNTIME_DIR,XDG_STATE_HOME:process.env.XDG_STATE_HOME};
 process.env.XDG_RUNTIME_DIR=runtimeDir;process.env.XDG_STATE_HOME=root;
 const prompts=[];
 const runtime=await ModelRuntime.create({credentials:AuthStorage.inMemory(),modelsPath:null,refreshOnCreate:false,allowModelNetwork:false});
 runtime.registerProvider('review-fake',{baseUrl:'http://invalid.invalid',apiKey:'synthetic',api:'review-fake',models:[{id:'fake',name:'fake',input:['text'],reasoning:false,cost:{input:0,output:0,cacheRead:0,cacheWrite:0},contextWindow:65536,maxTokens:2048}],streamSimple(model,context){
  prompts.push(JSON.stringify(context.messages)); const stream=createAssistantMessageEventStream(); const message={role:'assistant',content:[{type:'text',text:'Synthetic response.'}],api:model.api,provider:model.provider,model:model.id,stopReason:'stop',timestamp:Date.now(),usage:{input:10,output:2,cacheRead:0,cacheWrite:0,totalTokens:12,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}}}; stream.push({type:'done',reason:'stop',message});stream.end();return stream;
 }});
 const settingsManager=SettingsManager.inMemory({compaction:{enabled:false,keepRecentTokens:256,reserveTokens:1024}});
 const loader=new DefaultResourceLoader({cwd:root,agentDir:root,settingsManager,additionalExtensionPaths:[EXTENSION],noSkills:true,noThemes:true,noPromptTemplates:true,noContextFiles:true});await loader.reload();
 const manager=SessionManager.create(root,join(root,'sessions'));
 const {session}=await createAgentSession({cwd:root,agentDir:root,modelRuntime:runtime,model:runtime.getModel('review-fake','fake'),sessionManager:manager,settingsManager,resourceLoader:loader,noTools:'all'});
 const errors=[];await session.bindExtensions({onError:e=>errors.push(e)});
 const metadata=async()=>JSON.parse(await readFile(join(runtimeDir,(await readdir(runtimeDir))[0],'manifest.json'),'utf8'));
 const request=(socket,body)=>new Promise((resolve,reject)=>{const c=createConnection(socket);let b='';c.on('connect',()=>c.end(JSON.stringify(body)+'\n'));c.on('data',d=>b+=d);c.on('end',()=>{try{resolve(JSON.parse(b))}catch(e){reject(e)}});c.on('error',reject)});
 return {root,session,manager,prompts,errors,metadata,request,async close(){await session.extensionRunner.emit({type:'session_shutdown',reason:'quit'});session.dispose();for(const [k,v] of Object.entries(old)){if(v===undefined)delete process.env[k];else process.env[k]=v;}await rm(root,{recursive:true,force:true});}};
}
test('independent: discovery becomes enabled after a normal Pi turn settles',{timeout:5000},async()=>{
 const f=await setup();try {
  assert.equal((await f.metadata()).capabilities.handoff.enabled,true);
  await f.session.prompt('Synthetic review turn, return one sentence.');
  assert.equal(f.session.isIdle,true);
  await new Promise(resolve=>setTimeout(resolve,25));
  const m=await f.metadata();console.log('after_turn',JSON.stringify({isIdle:f.session.isIdle,capabilities:m.capabilities}));
  assert.deepEqual(f.errors,[]);
  assert.equal(m.capabilities.handoff.enabled,true,'settled turn must re-enable discovery');
 } finally {await f.close();}
});
test('independent: handoff preserves compaction goal and excludes system instructions',{timeout:5000},async()=>{
 const f=await setup();try {
  const system=f.manager.appendMessage({role:'system',content:'SYNTHETIC_SYSTEM_ONLY_CANARY',timestamp:Date.now()});
  f.manager.appendMessage({role:'user',content:'Old request summarized below',timestamp:Date.now()});
  const kept=f.manager.appendMessage({role:'user',content:'Recent request without the old goal',timestamp:Date.now()});
  f.manager.appendCompaction('SYNTHETIC_COMPACTED_GOAL_CANARY',kept,9000);
  const projection=f.manager.buildSessionProjection().messages;
  console.log('projection_kinds',JSON.stringify(projection.map(m=>({role:m.role,contentPresent:m.content!==undefined,summaryPresent:m.summary!==undefined}))));
  const m=await f.metadata();const {socket,capabilities,handoff_directory,model_auth,...identity}=m;
  const q={...identity,expected:f.manager.getGuardedCompactionState(),operation_id:randomUUID(),action:'handoff'};
  assert.equal((await f.request(socket,q)).state,'accepted');let r;
  for(let i=0;i<100;i++){r=await f.request(socket,{...q,action:'status'});if(!['accepted','running'].includes(r.state))break;await new Promise(r=>setTimeout(r,5));}
  assert.equal(r.state,'completed');const prompt=f.prompts.at(-1);
  console.log('provider_capture',JSON.stringify({hasSummary:prompt.includes('SYNTHETIC_COMPACTED_GOAL_CANARY'),hasSystem:prompt.includes('SYNTHETIC_SYSTEM_ONLY_CANARY')}));
  assert.ok(prompt.includes('SYNTHETIC_COMPACTED_GOAL_CANARY'),'projected compaction summary must reach handoff summarizer');
  assert.equal(prompt.includes('SYNTHETIC_SYSTEM_ONLY_CANARY'),false,'system-only content must not reach transcript summarizer');
 }finally{await f.close();}
});
