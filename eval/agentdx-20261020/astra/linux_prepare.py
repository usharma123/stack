import harness as h,json,tarfile
name='agentdx-astra-'+h.ISO.name
r=json.loads((h.RAW/'resources.json').read_text());r['containers'].append(name);(h.RAW/'resources.json').write_text(json.dumps(r,indent=2))
# Copy only pinned tracked source, no reports or local modifications.
h.receipt('linux-source-archive',['git','archive','--format=tar','--output='+str(h.RAW/'source.tar'),'8b3402fa1db8f11917b5ba33a333c9093545d9ce'])
h.receipt('linux-container-create',['docker','run','-d','--name',name,'--label','agentdx.owner='+h.ISO.name,'rust:1.93','sleep','7200'])
h.receipt('linux-source-copy',['docker','cp',str(h.RAW/'source.tar'),name+':/tmp/source.tar'])
h.receipt('linux-build',['docker','exec',name,'bash','-lc','mkdir -p /work/src; tar -xf /tmp/source.tar -C /work/src; cd /work/src; cargo build --locked'],timeout=180)
