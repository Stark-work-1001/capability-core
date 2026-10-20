"""Isolated end-to-end acceptance. Cloud behavior uses a local mock, no paid calls.
Run with --exe <assets-serve.exe> --out <new directory under artifacts>.
"""
import argparse, concurrent.futures, hashlib, http.server, json, os, pathlib, shutil, socket, sqlite3, subprocess, threading, time, urllib.request, urllib.error

class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        v=json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path.endswith("embeddings"):
            data=[]
            for i,text in enumerate(v["input"]):
                h=hashlib.sha256(text.encode()).digest();data.append({"index":i,"embedding":[1+int(x) for x in h[:4]]})
            out={"data":list(reversed(data))}
        elif self.path.endswith("rerank"):
            out={"results":[{"index":i,"relevance_score":1/(i+1)} for i in range(len(v["documents"]))]}
        else:
            system=v["messages"][0]["content"];context=json.loads(v["messages"][-1]["content"])
            if v["model"]=="failing-repair":
                self.send_response(503);self.end_headers();self.wfile.write(b'{"error":{"message":"simulated outage"}}');return
            if "approved" in system:answer=json.dumps({"approved":True,"reason":"mock verification of structured draft"})
            elif "files(" in system:
                kind=context.get("asset",{}).get("kind","skill");entry="mcp.json" if kind=="mcp" else "SKILL.md" if kind=="skill" else "manifest.json"
                text=json.dumps({"mcpServers":{"fixed":{"command":"python","args":["still-missing.py"]}}}) if kind=="mcp" else "---\nname: generated\ndescription: 测试技能\n---\n步骤：核对输入，输出测试结果。\n前提：已授权。\n验收：结果可复核。"
                answer=json.dumps({"id":"generated","kind":kind,"name":"Generated","description":"测试技能修订","entry":entry,"files":{entry:text},"author":"","repository":"","license":""},ensure_ascii=False)
            else:answer="模拟诊断：缺失依赖须由用户安装；尚未修复。"
            out={"choices":[{"message":{"content":answer}}]}
        data=json.dumps(out,ensure_ascii=False).encode();self.send_response(200);self.send_header("Content-Type","application/json");self.send_header("Content-Length",str(len(data)));self.end_headers();self.wfile.write(data)

def main():
    ap=argparse.ArgumentParser();ap.add_argument("--exe",required=True);ap.add_argument("--out",required=True);a=ap.parse_args()
    exe=pathlib.Path(a.exe).resolve();root=pathlib.Path(a.out).resolve();root.mkdir(parents=True,exist_ok=False);data=root/"data";data.mkdir();db=data/"registry.db"
    checks=[];counter=0
    def cli(mode,*args,ok=True):
        p=subprocess.run([str(exe),mode,"--db",str(db),*map(str,args)],capture_output=True,text=True,encoding="utf8",timeout=40)
        if ok and p.returncode:raise AssertionError((mode,p.stderr))
        if not ok:assert p.returncode!=0,(mode,p.stdout);return p.stderr
        return json.loads(p.stdout)
    def file(value):
        nonlocal counter
        counter+=1;p=root/f"input-{counter}.json";p.write_text(json.dumps(value,ensure_ascii=False),encoding="utf8");return p
    def check(name,condition=True):
        assert condition,name;checks.append(name)
    def request(id="sample",kind="skill",text=None):
        entry="mcp.json" if kind=="mcp" else "SKILL.md"
        if text is None:text=json.dumps({"mcpServers":{"broken":{"command":"assets-deliberately-missing-runtime","args":["missing.py"]}}}) if kind=="mcp" else "---\nname: sample\ndescription: 测试\n---\n测试索引与受控能力"
        return {"id":id,"kind":kind,"name":"测试能力 "+id,"description":"用于测试检索、版本与维修","entry":entry,"files":{entry:text},"author":"Fixture","repository":"","license":"MIT"}
    def prepare(r):return cli("prepare","--input",file(r))
    def commit(p):return cli("commit","--operation",p["operation_id"],"--digest",p["digest"])
    r=request();plan=prepare(r);check("full SHA-256 preview",len(plan["digest"])==64)
    cli("commit","--operation",plan["operation_id"],"--digest","wrong",ok=False);check("digest mismatch rejected")
    first=commit(plan);check("immutable publication and retry",commit(plan)["id"]==first["id"])
    invalid=request("bad");invalid["files"]["../escape.txt"]="escape";cli("prepare","--input",file(invalid),ok=False);check("path traversal rejected")
    r["files"]["SKILL.md"]+="\nsecond";p2=prepare(r);r["files"]["SKILL.md"]+="\nthird";p3=prepare(r);commit(p2)
    cli("commit","--operation",p3["operation_id"],"--digest",p3["digest"],ok=False);check("concurrent stale preview rejected")
    cli("rollback","--asset",first["id"],"--revision",plan["digest"]);check("rollback to stored bytes")
    vendor=root/"vendor/skill/local";vendor.mkdir(parents=True);(vendor/"SKILL.md").write_text("---\nname: local\ndescription: 测试第二项能力\n---\n测试步骤",encoding="utf8")
    ing=cli("ingest","--root",root/"vendor");check("scanning vendor preserves managed imports",ing["total"]==2)
    mcp=commit(prepare(request("broken","mcp")));check("MCP server declarations registered",len(mcp["provides"])==1)
    for mode in ["keyword","python","rust"]:
        result=cli("search","--mode",mode,"--query","测试");check(mode+" search returns real assets",result["total"]>=2)
    cli("search","--mode","cloud-vector","--query","测试",ok=False);check("unconfigured vector search fails explicitly")
    model=http.server.ThreadingHTTPServer(("127.0.0.1",0),Model);threading.Thread(target=model.serve_forever,daemon=True).start()
    slots=[]
    for slot in ["embedding","judge","rerank","diagnose","repair","dedup","generate","verify","fallback","localembedding"]:
        slots.append({"slot":slot,"enabled":True,"mode":"local-openai","endpoint":f"http://127.0.0.1:{model.server_port}/v1","model":"failing-repair" if slot=="repair" else "mock-"+slot,"timeout_ms":1500})
    (data/"llm-providers.json").write_text(json.dumps({"slots":slots}),encoding="utf8")
    for mode in ["local-vector","cloud-vector"]:
        result=cli("search","--mode",mode,"--query","测试");check(mode+" uses mock embeddings and rerank",result["total"]==3 and any("Reranker" in n for n in result["notes"]))
    def workflow(name,value=None):return cli("workflow","--name",name,"--input",file(value or {}))
    inputs={"r4":{"asset_id":mcp["id"]},"r5":{"asset_id":mcp["id"]},"r6":{"requirement":"比较测试能力"},"r8":{"import":request("quarantine")},"r9":{"asset_id":mcp["id"]},"r10":{"asset_id":first["id"]},"r14":{"requirement":"测试降级"},"r16":{"requirement":"生产一个测试技能"}}
    for i in range(1,17):
        name="r"+str(i);result=workflow(name,inputs.get(name));check(name+" executable workflow",result["status"] in ["completed","needs_review"])
        if name=="r5":check("repair provider outage falls back and creates same-kind draft",result["result"]["draft"]["kind"]=="mcp" and result["result"]["generation_model"]=="mock-judge")
    ids=[first["id"],"skill::skill-local"]
    result=workflow("dedup",{"asset_id":ids[0],"other_asset_id":ids[1]});check("semantic comparison saved",result["status"]=="needs_review")
    result=workflow("merge",{"asset_id":ids[0],"other_asset_id":ids[1]});check("merge produces review draft preserving sources",result["status"]=="needs_review")
    history=cli("history");check("audit history persists",len(history)>=8)
    with socket.socket() as free:free.bind(("127.0.0.1",0));port=free.getsockname()[1]
    env=os.environ.copy();env["AC_VECTOR_TIMEOUT_MS"]="100";process=subprocess.Popen([str(exe),"http","--db",str(db),"--port",str(port)],stdout=subprocess.DEVNULL,stderr=(root/"http.log").open("wb"),env=env)
    def call_http(method,path,payload=None,headers=None):
        body=json.dumps(payload).encode() if payload is not None else None
        req=urllib.request.Request(f"http://127.0.0.1:{port}"+path,data=body,method=method,headers={"Content-Type":"application/json",**(headers or {})})
        try:
            with urllib.request.urlopen(req,timeout=10) as r:return r.status,dict(r.headers),r.read()
        except urllib.error.HTTPError as e:return e.code,dict(e.headers),e.read()
    try:
        for _ in range(100):
            try:
                if call_http("GET","/healthz")[0]==200:break
            except OSError:time.sleep(.1)
        else:raise AssertionError("HTTP startup failed")
        check("browser Origin rejected",call_http("POST","/mcp",{}, {"Origin":"https://example.org"})[0]==400)
        init={"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"fixture","version":"1"}}}
        status,headers,body=call_http("POST","/mcp",init);token=headers.get("Mcp-Session-Id");check("HTTP MCP creates session",status==200 and bool(token))
        req={"jsonrpc":"2.0","id":2,"method":"tools/list"};check("missing session rejected",call_http("POST","/mcp",req)[0]==404)
        headers={"Mcp-Session-Id":token};body=call_http("POST","/mcp",req,headers)[2];tools=json.loads(body)["result"]["tools"];check("MCP schemas expose named inputs",tools[0]["inputSchema"]["properties"]["q"]["type"]=="string")
        req={"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_capability_content","arguments":{"id":first["id"]}}}
        body=json.loads(call_http("POST","/mcp",req,headers)[2]);check("agent can retrieve managed source content",not body["result"].get("isError") and "测试索引" in str(body))
        req["params"]["arguments"]["agent"]="spoof";body=json.loads(call_http("POST","/mcp",req,headers)[2]);check("session identity spoof rejected",body["result"]["isError"])
        late=request("late");late["name"]="later asset";late["description"]="用于绘制时序 组件 与 流程图";commit(prepare(late))
        query={"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search_capabilities","arguments":{"q":"时序图"}}}
        body=json.loads(call_http("POST","/mcp",query,headers)[2]);check("running gateway indexes newly published asset",not body["result"].get("isError") and "managed::skill::late" in str(body))
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:statuses=list(pool.map(lambda _:call_http("GET","/healthz")[0],range(24)))
        check("concurrent clients complete",all(s==200 for s in statuses))
        check("session terminates",call_http("DELETE","/mcp",headers=headers)[0]==200)
    finally:process.terminate();process.wait(timeout=10);model.shutdown()
    source=pathlib.Path(first["provenance"]["source_path"]);source=source if source.is_absolute() else data/source
    source.write_text("tampered",encoding="utf8")
    cli("rollback","--asset",first["id"],"--revision",plan["digest"],ok=False);check("tampered revision rollback refused")
    moved=root.with_name(root.name+"-relocated");shutil.copytree(root,moved);data=moved/"data";db=data/"registry.db"
    cli("ingest","--root",moved/"vendor")
    check("managed source survives package relocation",workflow("r9",{"asset_id":mcp["id"]})["result"]["entry_exists"])
    check("vendor sources refresh after relocation",workflow("r11")["result"]["missing_sources"]==[])
    report={"passed":len(checks),"checks":checks,"cloud_validation":"local mock only; no live cloud credentials or paid requests","directory":str(root)}
    (root/"report.json").write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf8");print(json.dumps(report,ensure_ascii=False,indent=2))

if __name__=="__main__":main()
