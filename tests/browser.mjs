import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdtemp, writeFile, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";

const { chromium } = await import(process.env.AIMARK_PLAYWRIGHT || "playwright");
const binary = resolve(process.env.AIMARK_BIN || "target/debug/aimark");
const root = await mkdtemp(join(tmpdir(),"aimark-browser-"));
let server; let browser;
try {
  await mkdir(join(root,"src"));
  const human = "export function greet(name) {\n  return `Hello, ${name}`;\n}\n";
  const mixed = "export function greet(name) {\n  if (!name) throw new Error('Name is required');\n  return `Hello, ${name}`;\n}\n\nexport function farewell(name) {\n  return `See you, ${name}`;\n}\n";
  await writeFile(join(root,"src/greet.js"),human); await writeFile(join(root,"README.md"),"# Hello\n\nA little example project.\n");
  execFileSync(binary,["--root",root,"checkpoint"]);
  await writeFile(join(root,"src/greet.js"),mixed); execFileSync(binary,["--root",root,"record","--harness","pi","--model","example-model","--files","src/greet.js"]);
  await writeFile(join(root,"notes.txt"),"<img src=x onerror=alert('unsafe')>\nPending human notes\n");
  server=spawn(binary,["--root",root,"dashboard","--port","0"],{stdio:["ignore","pipe","pipe"]});
  const url=await new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>reject(new Error("Dashboard did not start")),10000);
    server.once("exit",code=>{clearTimeout(timer);reject(new Error(`Dashboard exited ${code}`));});
    createInterface({input:server.stdout}).on("line",line=>{if(line.startsWith("aimark dashboard: ")){clearTimeout(timer);resolve(line.slice(18));}});
    server.stderr.on("data",data=>process.stderr.write(data));
  });
  browser=await chromium.launch({headless:true,...(process.env.AIMARK_CHROMIUM ? {executablePath:process.env.AIMARK_CHROMIUM}: {})});
  const page=await browser.newPage({viewport:{width:1440,height:1050}}); const errors=[];
  page.on("pageerror",error=>errors.push(error.message));
  page.on("dialog",async dialog=>{errors.push(`Unexpected dialog: ${dialog.message()}`);await dialog.dismiss();});
  await page.goto(url); await page.waitForFunction(()=>document.querySelectorAll(".code-line").length===8);
  assert.equal(await page.locator(".code-line.ai").count(),5); assert.equal(await page.locator("#files-count").textContent(),"3");
  await page.locator('[data-lines="ai"]').click(); assert.equal(await page.locator(".code-line").count(),5);
  await page.locator('[data-lines="human"]').click(); assert.equal(await page.locator(".code-line").count(),3);
  await page.locator('[data-lines="all"]').click(); await page.locator("#line-search").fill("farewell"); assert.equal(await page.locator(".code-line").count(),1); await page.locator("#line-search").fill("");
  await page.locator("#file-search").fill("notes"); assert.equal(await page.locator(".file-row").count(),1); await page.locator(".file-row").click(); await page.waitForFunction(()=>document.querySelectorAll(".code-line").length===2);
  assert.equal(await page.locator("#source img").count(),0); assert.match(await page.locator("#source").textContent(),/<img src=x/);
  await page.locator("#file-search").fill(""); await page.locator("#file-filter").selectOption("ai"); assert.equal(await page.locator(".file-row").count(),1); await page.locator(".file-row").click(); await page.waitForFunction(()=>document.querySelectorAll(".code-line").length===8);
  await page.locator("#file-filter").selectOption("all");
  if(process.env.AIMARK_SCREENSHOTS) await page.screenshot({path:join(process.env.AIMARK_SCREENSHOTS,"dashboard-desktop.png"),fullPage:true});
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),"mobile page overflows horizontally");
  if(process.env.AIMARK_SCREENSHOTS) await page.screenshot({path:join(process.env.AIMARK_SCREENSHOTS,"dashboard-mobile.png"),fullPage:true});
  await writeFile(join(root,"fresh.txt"),"new pending file\n"); await page.locator("#refresh").click(); await page.waitForFunction(()=>document.querySelector("#files-count").textContent==="4");
  assert.deepEqual(errors,[]); console.log("Browser checks passed: attribution, filters, search, escaping, mobile layout, refresh.");
} finally {
  await browser?.close(); if(server && server.exitCode === null && server.signalCode === null){server.kill(); await new Promise(resolve=>server.once("exit",resolve));} await rm(root,{recursive:true,force:true});
}
