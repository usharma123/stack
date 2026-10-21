import harness as h
p=h.RAW/'projects/ledger-a'; p.mkdir(parents=True,exist_ok=True)
(p/'stack.toml').write_text('''[tools]
python = "3.13"
uv = "0.12.23"
[services.postgres]
preset = "postgres"
version = "17"
[services.redis]
preset = "redis"
version = "7"
[tasks.test]
run = "uv run python test_api.py"
services = ["postgres", "redis"]
''')
h.receipt('clean-native-setup',[h.D['binary'],'--json','setup'],cwd=h.RAW,timeout=90,extra={'PATH':'/usr/bin:/bin:/usr/sbin:/sbin'})
h.stack('ledger-naive-compile',p,'compile',timeout=180)
h.stack('ledger-naive-up',p,'up','--ttl','30m','--timeout','90s',timeout=105)
