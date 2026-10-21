import assert from 'node:assert/strict';
import {mkdir,readFile,readdir} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {spawn} from 'node:child_process';
const root=process.env.HOME;
const original=process.env.PI_SDK_DIR;
const sdk=process.env.AGENTCENTER_GUARDED_PI_SDK;
assert.ok(original, 'PI_SDK_DIR must point to the original Pi SDK');
assert.ok(sdk, 'AGENTCENTER_GUARDED_PI_SDK must point to the guarded Pi SDK');
const extension=process.env.AGENTCENTER_PI_CONTROL_EXTENSION || fileURLToPath(new URL('../../../extensions/pi-control/index.ts', import.meta.url));
const moduleUrl=p=>pathToFileURL(resolve(p)).href;
const {createAgentSession,ModelRuntime,AuthStorage,SessionManager,SettingsManager,DefaultResourceLoader}=Object.assign({},...await Promise.all(['sdk','model-runtime','auth-storage','session-manager','settings-manager','resource-loader'].map(n=>import(moduleUrl(`${sdk}/dist/core/${n}.js`)).catch(e=>{throw Error(`Required CC0 SDK missing: ${sdk}`,{cause:e});}))));
const {createAssistantMessageEventStream}=await import(moduleUrl(`${original}/node_modules/@earendil-works/pi-ai/dist/index.js`));
globalThis.fetch=()=>{throw Error('NETWORK FORBIDDEN')};
let calls=0;
const runtime=await ModelRuntime.create({credentials:AuthStorage.inMemory(),modelsPath:null,refreshOnCreate:false,allowModelNetwork:false});
runtime.registerProvider('cc1-fake',{baseUrl:'http://invalid.invalid',apiKey:'synthetic',api:'cc1-fake',models:[{id:'fake',name:'fake',input:['text'],reasoning:false,cost:{input:0,output:0,cacheRead:0,cacheWrite:0},contextWindow:65536,maxTokens:2048}],streamSimple(model){calls++;const stream=createAssistantMessageEventStream(); const message={role:'assistant',content:[{type:'text',text:'Verified handoff. Next: continue.'}],api:model.api,provider:model.provider,model:model.id,stopReason:'stop',timestamp:Date.now(),usage:{input:0,output:0,cacheRead:0,cacheWrite:0,totalTokens:0,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}}};stream.push({type:'done',reason:'stop',message});stream.end();return stream;}});
const settingsManager=SettingsManager.inMemory({compaction:{keepRecentTokens:256,reserveTokens:1024}});
const loader=new DefaultResourceLoader({cwd:root,agentDir:root,settingsManager,additionalExtensionPaths:[extension],noSkills:true,noThemes:true,noPromptTemplates:true,noContextFiles:true});await loader.reload();
const manager=SessionManager.create(root,join(root,'sessions'));
for(let i=0;i<12;i++){manager.appendMessage({role:'user',content:`Synthetic goal ${i} `+'history '.repeat(500),timestamp:Date.now()});manager.appendMessage({role:'assistant',content:[{type:'text',text:'Synthetic response.'}],api:'cc1-fake',provider:'cc1-fake',model:'fake',stopReason:'stop',timestamp:Date.now(),usage:{input:100,output:5,cacheRead:0,cacheWrite:0,totalTokens:105,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}}});}
const {session}=await createAgentSession({cwd:root,agentDir:root,modelRuntime:runtime,model:runtime.getModel('cc1-fake','fake'),sessionManager:manager,settingsManager,resourceLoader:loader,noTools:'all'});
try{
 const errors=[];await session.bindExtensions({onError:e=>errors.push(e)});assert.deepEqual(errors,[]);
 const initial=manager.getGuardedCompactionState(); const id=manager.getSessionId(); const file=manager.getSessionFile();
 const dirs=await readdir(process.env.XDG_RUNTIME_DIR);const dir=dirs.find(d=>d.startsWith('ac-'));assert.ok(dir,JSON.stringify(dirs));
 const manifest=join(process.env.XDG_RUNTIME_DIR,dir,'manifest.json');
 const gui=spawn(process.env.CC3_GUI,[manifest],{stdio:['ignore','pipe','inherit']});let output='';gui.stdout.on('data',d=>{output+=d;process.stdout.write(d)});
 const stop=()=>gui.kill('SIGTERM');
 process.on('SIGTERM',stop); process.on('SIGINT',stop);
 const timer=setTimeout(()=>gui.kill('SIGKILL'),30000);
 let code;
 try { code=await new Promise((resolve,reject)=>{gui.on('error',reject);gui.on('close',resolve)}); }
 finally {clearTimeout(timer);process.off('SIGTERM',stop);process.off('SIGINT',stop);if(gui.exitCode===null && gui.signalCode===null) {gui.kill('SIGKILL');await new Promise(resolve=>gui.once('close',resolve));}}
 assert.equal(code,0);assert.match(output,/HANDOFF Saved:/);assert.match(output,/COMPACT Native compact persisted:/);
 const metadata=JSON.parse(await readFile(manifest,'utf8'));
 const artifacts=await readdir(metadata.handoff_directory);assert.equal(artifacts.length,1);
 const artifact=await readFile(join(metadata.handoff_directory,artifacts[0]),'utf8');assert.deepEqual(JSON.parse(artifact.split('\n')[0]).expected,initial);assert.match(artifact,/Verified handoff/);
 const entries=(await readFile(file,'utf8')).trim().split('\n').map(JSON.parse);const compact=entries.filter(e=>e.type==='compaction');assert.equal(compact.length,1);assert.equal(compact[0].parentId,initial.leafId);assert.equal(manager.getLeafId(),compact[0].id);assert.equal(manager.getSessionId(),id);assert.equal(manager.getSessionFile(),file);assert.equal(calls,2);assert.equal(entries.filter(e=>e.type==='message').length,24);assert.deepEqual(errors,[]);
 console.log('PASS native GUI handoff + compact; 2 fake calls; 24 raw messages preserved; same session/file');
}finally{await session.extensionRunner.emit({type:'session_shutdown',reason:'quit'});session.dispose();}
