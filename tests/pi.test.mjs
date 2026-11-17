import { test } from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import extension from "../integrations/pi/aimark.ts";

const exec = promisify(execFile);
const binary = resolve(process.env.AIMARK_BIN || "target/debug/aimark");
test("pi captures scoped overlapping edits with model and session metadata", async () => {
  const root = await mkdtemp(join(tmpdir(),"aimark-pi-"));
  try {
    await writeFile(join(root,"a.txt"),"human a\n"); await writeFile(join(root,"b.txt"),"human b\n");
    await exec(binary,["--root",root,"checkpoint"]);
    const handlers = new Map(); const notifications = [];
    extension({
      on: (name,callback) => handlers.set(name,callback), registerCommand: () => {},
      exec: async (name,args,options) => {
        assert.equal(name,"aimark");
        try { return { ...await exec(binary,args,options), code:0 }; }
        catch (error) { return { code:error.code, stdout:error.stdout, stderr:error.stderr }; }
      },
    });
    const ctx = { cwd:root, model:{id:"test-model"}, sessionManager:{getSessionId:()=>"test-session"}, hasUI:true, ui:{notify:(...args)=>notifications.push(args)} };
    const start = handlers.get("tool_call"); const end = handlers.get("tool_execution_end");
    assert.equal(await start({toolName:"edit",toolCallId:"a",input:{path:"a.txt"}},ctx),undefined);
    await writeFile(join(root,"a.txt"),"robot a\n");
    await start({toolName:"write",toolCallId:"b",input:{path:"b.txt"}},ctx);
    await writeFile(join(root,"b.txt"),"robot b\n");
    await end({toolCallId:"b",isError:false},ctx); await end({toolCallId:"a",isError:true},ctx);
    for (const file of ["a.txt","b.txt"]) {
      const { stdout } = await exec(binary,["--root",root,"blame",file,"--json"]);
      assert.equal(JSON.parse(stdout)[0].author,"pi/test-model");
    }
    assert.deepEqual(notifications,[]);
    const { stdout } = await exec("git",["--git-dir",join(root,".aimark/repo"),"log","-1","--format=%B"]);
    assert.match(stdout,/AI-Session: test-session/);
  } finally { await rm(root,{recursive:true,force:true}); }
});

test("pi blocks an edit if its human checkpoint cannot be saved", async () => {
  const handlers = new Map();
  extension({on:(name,callback)=>handlers.set(name,callback),registerCommand:()=>{},exec:async()=>({code:1,stderr:"ledger unavailable"})});
  const result=await handlers.get("tool_call")({toolName:"write",toolCallId:"x",input:{path:"a.txt"}},{cwd:tmpdir()});
  assert.equal(result.block,true); assert.match(result.reason,/ledger unavailable/);
});
