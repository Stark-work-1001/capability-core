"""Export every registered asset with provenance evidence; never invent authors.

Read-only input DB. --refresh-upstreams queries public GitHub metadata/trees only.
Writes reports to the explicitly selected output directory, never the source DB.
"""
import argparse, concurrent.futures, datetime, hashlib, json, pathlib, re, sqlite3, urllib.request

REPOS = ["K-Dense-AI/scientific-agent-skills", "kenryu42/cc-safety-net", "omdsh-dev/dsh-mnemon",
         "bowenliang123/dsh-context", "Aa728848/dsh-chatgpt-subscription", "Aa728848/dsh-llm-verifier",
         "wzyn20051216/solidworks-automation-skill", "moonbitlang/skills", "ahujasid/mcp-for-blender"]
PKGS = {"cc-safety-net":REPOS[1], "dsh-mnemon":REPOS[2], "dsh-context":REPOS[3],
        "dsh-chatgpt-subscription":REPOS[4], "dsh-llm-verifier":REPOS[5], "solidworks-automation":REPOS[6],
        "moonbit-skills":REPOS[7], "mcp-for-blender-main":REPOS[8]}

def api(url):
    with urllib.request.urlopen(urllib.request.Request(url,headers={"User-Agent":"Capability-Core-Provenance-Research","Accept":"application/vnd.github+json"}),timeout=30) as r:
        return json.load(r)

def refresh(repo, cache):
    try:
        meta=api("https://api.github.com/repos/"+repo)
        tree=api("https://api.github.com/repos/"+repo+"/git/trees/HEAD?recursive=1")
        data={"repository":meta["html_url"],"owner":meta["owner"]["login"],"description":meta.get("description"),
              "license":(meta.get("license") or {}).get("spdx_id"),"archived":meta.get("archived"),
              "tree_sha":tree.get("sha"),"truncated":tree.get("truncated"),"checked_at":datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "files":{f["sha"]:f["path"] for f in tree.get("tree",[]) if f["type"]=="blob"}}
        (cache/(repo.replace("/","--")+".json")).write_text(json.dumps(data,ensure_ascii=False,indent=2),encoding="utf8")
        return repo,True
    except Exception as e:
        return repo,str(e)

def blob_hash(b):
    return hashlib.sha1(b"blob "+str(len(b)).encode()+b"\0"+b).hexdigest()

def main():
    parser=argparse.ArgumentParser();parser.add_argument("--db",required=True);parser.add_argument("--out",required=True)
    parser.add_argument("--refresh-upstreams",action="store_true");args=parser.parse_args()
    out=pathlib.Path(args.out).resolve();out.mkdir(parents=True,exist_ok=True);cache=out/"upstreams";cache.mkdir(exist_ok=True)
    if args.refresh_upstreams:
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            print(json.dumps(list(pool.map(lambda r:refresh(r,cache),REPOS)),ensure_ascii=False),flush=True)
    upstreams={}
    for f in cache.glob("*.json"):
        v=json.loads(f.read_text(encoding="utf8"));upstreams[f.stem.replace("--","/")]=v
    conn=sqlite3.connect(pathlib.Path(args.db).resolve().as_uri()+"?mode=ro",uri=True)
    rows={}
    for (raw,) in conn.execute("SELECT manifest FROM assets ORDER BY kind,name"):
        m=json.loads(raw);prov=m.get("provenance",{});source=pathlib.Path(prov.get("source_path",""))
        if not source.is_absolute():source=pathlib.Path(args.db).resolve().parent/source
        row={"asset_id":m["id"],"kind":m["kind"],"name":m["name"],"description":m.get("description",""),
             "author":prov.get("author"),"author_source":prov.get("author_source"),"repository":None,"license":None,
             "verified_online":False,"content_verified":False,"evidence":[],"runtime_refs":prov.get("runtime_refs",[])}
        try: content=source.read_bytes();text=content.decode("utf-8-sig")
        except (OSError,UnicodeError) as e:row["read_error"]=str(e);rows[m["id"]]=row;continue
        row["source_content_sha256"]=hashlib.sha256(content).hexdigest()
        front=text.split("---",2)[1] if text.startswith("---") and text.count("---")>=2 else ""
        author=re.search(r"(?m)^\s*(?:skill-author|author):\s*([^\r\n]+)",front)
        if not row["author"] and author:
            row["author"]=author.group(1).strip().strip("\"'");row["author_source"]="frontmatter declaration"
        license_match=re.search(r"(?m)^license:\s*([^\r\n]+)",front)
        if license_match:row["license"]=license_match.group(1).strip().strip("\"'");row["license_scope"]="local frontmatter declaration; may describe underlying library"
        pkg=None
        for parent in source.parents:
            if parent.parent.name in ["skill","plugin","mcp"] and parent.parent.parent.name=="vendor":pkg=parent;break
        if pkg:
            row["package_label"]=pkg.name
            for f in [pkg/"package.json",pkg/".claude-plugin/plugin.json",pkg/".codex-plugin/plugin.json",pkg/".qoder-plugin/plugin.json"]:
                if not f.is_file():continue
                try:v=json.loads(f.read_text(encoding="utf-8-sig"))
                except (OSError,ValueError):continue
                for key in ["author","repository","license"]:
                    if not row.get(key) and v.get(key):
                        value=v[key];row[key]=value.get("name") if key=="author" and isinstance(value,dict) else value.get("url") if key=="repository" and isinstance(value,dict) else value
                        row["evidence"].append({"field":key,"source":str(f)})
        hashes={blob_hash(content),blob_hash(content.replace(b"\r\n",b"\n"))}
        match=None
        for repo,up in upstreams.items():
            for h in hashes:
                if h in up["files"]:match=(repo,up,up["files"][h]);break
            if match:break
        repo=match[0] if match else PKGS.get(pkg.name if pkg else "")
        if not repo and row.get("author")=="K-Dense Inc.":repo=REPOS[0]
        if repo in upstreams:
            up=upstreams[repo];row["repository"]=up["repository"];row["repository_owner"]=up["owner"]
            row["verified_online"]=True;row["content_verified"]=match is not None
            row["upstream_license"]=up["license"];row["upstream_checked_at"]=up["checked_at"];row["upstream_archived"]=up["archived"]
            row["evidence"].append({"source":up["repository"],"tree_sha":up["tree_sha"],"match":"normalized Git blob" if match else "local declaration/package association; version unverified"})
            if match:row["upstream_file"]=up["repository"]+"/blob/"+up["tree_sha"]+"/"+match[2]
        row["metadata_source"]="local declaration + evidence list";rows[m["id"]]=row
    report={"generated_at":datetime.datetime.now(datetime.timezone.utc).isoformat(),"assets":rows}
    (out/"asset-research.json").write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf8")
    summary={"total":len(rows),"declared_authors":sum(bool(r.get("author")) for r in rows.values()),"repository_known":sum(bool(r.get("repository")) for r in rows.values()),"online_repository_checked":sum(r["verified_online"] for r in rows.values()),"normalized_content_verified":sum(r["content_verified"] for r in rows.values())}
    (out/"summary.json").write_text(json.dumps(summary,indent=2),encoding="utf8");print(json.dumps(summary),flush=True)

if __name__=="__main__":main()
