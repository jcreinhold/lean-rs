"""Seal the private application's fixed, locally built deployment under the caller's held guards."""
import hashlib, json, os, subprocess
from pathlib import Path
a=Path(__file__).resolve().parent
q=a.parent
rec=json.loads((a/'prepared.json').read_text())
project=a/'lean'; release=q/'release'
bundle=q/'sdk/crates/lean-rs-host/shims'
dep=bundle/'lean-rs-interop-shims'; host=bundle/'lean-rs-host-shims'
out=lambda p:p/'.lake/build/lib'
image=lambda p,name:out(p)/('lib'+name+'.dylib')
di=image(dep,'lean__rs__interop__shims_LeanRsInterop')
hi=image(host,'lean__rs__host__shims_LeanRsHostShims')
ci=image(project,'lean__rs__interop__consumer_LeanRsInteropConsumer')
shims=[out(dep)/'lean',out(host)/'lean']
assert Path(rec['consumer_image'])==ci and Path(rec['sysroot'])==release
assert list(map(Path,rec['shim_paths']))==shims
assert list(map(Path,rec['roots']))==sorted([project,dep,host])
assert list(map(Path,rec['shim_roots']))==sorted([dep,host])
filepins={}; directories=[]
def pin(path):
    assert path==path.resolve() and path.is_file(),path
    stat=path.stat()
    with path.open('rb') as stream: digest=hashlib.file_digest(stream,'sha256').hexdigest()
    filepins[str(path)]=[stat.st_dev,stat.st_ino,digest]
def tree(path):
    assert path==path.resolve() and path.is_dir(),path
    directories.append(str(path))
    for child in path.iterdir():
        assert not child.is_symlink(),child
        tree(child) if child.is_dir() else pin(child)
for manifest in ['consumer','shim']:
    path=Path(rec[manifest+'_manifest'])
    assert path.read_text()==rec[manifest+'_json']
    pin(path)
    v=json.loads(path.read_text())
    p,m,i=(('lean_rs_interop_consumer','LeanRsInteropConsumer',ci) if manifest=='consumer' else ('lean_rs_host_shims','LeanRsHostShims',hi))
    assert (v['package'],v['module'],v['primary_dylib'])==(p,m,str(i))
    assert v['dependencies']==[{'name':'lean_rs_interop_shims','dylib_path':str(di),'export_symbols_for_dependents':True,'initializer':{'package':'lean_rs_interop_shims','module':'LeanRsInterop'}}]
    assert v['build_toolchain']['sysroot']==str(release) and v['toolchain_fingerprint']['lean_version']=='4.36.0'
    assert v['toolchain_fingerprint']['header_sha256']=='dbccdab3a0036f41494f22762adce633875e34cf8f6f7cfa2186175b0e964aa4'
for f, syms in [(di,['_initialize_lean__rs__interop__shims_LeanRsInterop']), (hi,['_initialize_lean__rs__host__shims_LeanRsHostShims']), (ci,['_initialize_lean__rs__interop__consumer_LeanRsInteropConsumer','_lean_rs_interop_consumer_worker_metadata'])]:
    pin(f)
    exports={line.split()[-1] for line in subprocess.check_output(['/usr/bin/nm','-gU',f],text=True).splitlines() if line.strip()}
    assert set(syms)<=exports
    deps=[line.split(' (compatibility')[0].strip() for line in subprocess.check_output(['/usr/bin/otool','-L',f],text=True).splitlines()[2:]]
    assert all(p.startswith('/usr/lib/') or p in ('@rpath/libLake_shared.dylib','@rpath/libleanshared.dylib') for p in deps),deps
for root in (dep,host,project): pin(root/'lake-manifest.json')
newdir=json.loads((project/'lake-manifest.json').read_text())['packages'][0]['dir']
assert (project/newdir).resolve()==dep
# Both built packages must actually select the same source package.
assert (host/json.loads((host/'lake-manifest.json').read_text())['packages'][0]['dir']).resolve()==dep
for f in [release/'include/lean/lean.h',release/'bin/lean']: pin(f)
assert filepins[str(release/'include/lean/lean.h')][2]=='dbccdab3a0036f41494f22762adce633875e34cf8f6f7cfa2186175b0e964aa4'
for r in [out(project)/'lean',*shims,release/'lib/lean']: tree(r)
# Verify original transitive import selection and refuse local shadows of builtin namespaces.
for prefix in ['Init','Lean','Lake','Std']:
    for r in [out(project)/'lean',*shims]:
        assert not (r/prefix).exists() and not (r/(prefix+'.olean')).exists()
for root,module in [(out(project)/'lean','LeanRsInteropConsumer/Callback'),(shims[0],'LeanRsInterop')]:
    assert (root/(module+'.olean')).is_file()
assert not (out(project)/'lean'/'LeanRsInterop').exists() and not (out(project)/'lean'/'LeanRsInterop.olean').exists()
rec.update(files=filepins,directories=sorted(directories),interop_image=str(di),host_image=str(hi),
           import_roots=list(map(str,[out(project)/'lean',project/newdir/'.lake/build/lib/lean',*shims])))
rec['ordered_directory_ids']=[(Path(root).stat().st_dev,Path(root).stat().st_ino) for root in rec['import_roots']]
(a/'deployment.json').write_text(json.dumps(rec,separators=(',',':'),sort_keys=True))
print('sealed native images, registration order and checked exports:',ci,di,hi,'files',len(filepins),'directories',len(directories),flush=True)
