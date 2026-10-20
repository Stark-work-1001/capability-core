"""Create the offline runtime's content manifest from a verified extracted tree."""
import hashlib,json,pathlib,sys
root=pathlib.Path(sys.argv[1]).resolve()
files={}
for p in sorted(root.rglob('*')):
    if p.is_file():
        if p.is_symlink() or not p.resolve().is_relative_to(root):raise RuntimeError('Runtime contains a link')
        files[p.relative_to(root).as_posix()]=hashlib.sha256(p.read_bytes()).hexdigest()
(root.parent/'webview2-manifest.json').write_text(json.dumps({'version':'155.0.4283.62','files':files},indent=2),encoding='utf8')
print('Runtime manifest:',len(files),'files')
