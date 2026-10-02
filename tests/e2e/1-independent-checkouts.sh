# 1. Two independent checkouts of the same bundles get separate ports and verified instances.
export PATH=/opt/stack:$PATH
git config --global user.email a@b; git config --global user.name a; git config --global init.defaultBranch main
rm -rf /srv/pybase /srv/obs; for b in pybase obs; do cp -r /examples/bundles/$b /srv/$b && cd /srv/$b && git init -q && git add -A && git commit -qm v1 && git tag v1; done
mkapp() { rm -rf ~/$1 && cp -r /examples/app ~/$1 && cd ~/$1 && rm -f stack.lock && git init -q && sed -i 's|path:../bundles/pybase|git+file:///srv/pybase?ref=v1|; s|path:../bundles/obs|git+file:///srv/obs?ref=v1|' stack.toml && stack compile >/dev/null && git add -A && git commit -qm init; }
mkapp appA; mkapp appB
echo "### S1: two independent repos, no overrides"
for a in appA appB; do cd ~/$a; s=$(date +%s); stack up --json > /tmp/up-$a.json; echo "$a up rc=$? $(( $(date +%s)-s ))s $(jq -c '[.data.checks[] | {service, port, identity}]' /tmp/up-$a.json)"; done
for a in appA appB; do cd ~/$a; echo "$a tests: $(stack exec --require-all -- bash -c 'uv sync -q && uv run pytest -q 2>&1 | tail -1')"; done
for a in appA appB; do cd ~/$a; echo "$a reaches: $(stack exec -- bash -c 'psql "$DATABASE_URL" -Atc "show data_directory"')"; done
