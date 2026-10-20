import concurrent.futures,hashlib,pathlib,sys
root=pathlib.Path(sys.argv[1]).resolve()
if not (root/'CANDIDATE.json').is_file():raise RuntimeError('Not a review candidate')
def digest(p):
    h=hashlib.sha256()
    with p.open('rb') as f:
        while chunk:=f.read(1024*1024):h.update(chunk)
    return h.hexdigest()+'  '+str(p.relative_to(root))
static={'assets-desktop.exe','assets-serve.exe','vendor','runtime','docs','README.txt','CANDIDATE.json'}
files=sorted(p for p in root.rglob('*') if p.is_file() and p.relative_to(root).parts[0] in static and not p.name.endswith('.sock'))
with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:lines=list(pool.map(digest,files))
temporary=root/'SHA256SUMS.tmp'
temporary.write_text('\n'.join(lines)+'\n',encoding='utf8')
temporary.replace(root/'SHA256SUMS.txt')
print('Candidate checksums:',len(lines),'files')
