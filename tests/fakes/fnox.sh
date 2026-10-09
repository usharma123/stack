#!/bin/sh
echo "$*|${FNOX_NON_INTERACTIVE-unset}|$(pwd -P)" >>"$REVIEW_FIXTURE/fnox.log"
echo '  × fnox.toml line 3: DEPLOY_KEY = "leak-sentinel-stderr-0004"' >&2
case " $* " in *" --describe "*) step=describe ;; *) step=keys ;; esac
case "$(cat "$REVIEW_FIXTURE/fnox-$step-mode" 2>/dev/null)" in
  garbage) echo 'leak-sentinel-garbage-0006 is not the protocol'; exit 0 ;;
  config) echo '{"schema":1,"error":{"kind":"config","message":"line 3: DEPLOY_KEY = leak-sentinel-config-0005"}}'; exit 1 ;;
  oversized) head -c 70000 /dev/zero | tr '\0' x; exit 0 ;;
  exit) exit 3 ;;
  file) cat "$REVIEW_FIXTURE/fnox-$step.json"; exit "$(cat "$REVIEW_FIXTURE/fnox-$step-exit" 2>/dev/null || echo 0)" ;;
esac
if test "$step" = describe; then
  echo '{"schema":1,"fnox_version":"1.39.0","profile":["default"],"keys":[{"key":"DEPLOY_KEY","kind":"secret","env":true,"as_file":false,"injectable":{"exec":true,"shell":true}},{"key":"SENTRY_DSN","kind":"secret","env":true,"as_file":false,"injectable":{"exec":true,"shell":true}},{"key":"SHORT_KEY","kind":"secret","env":true,"as_file":false,"injectable":{"exec":true,"shell":true}},{"key":"HIDDEN","kind":"secret","env":false,"as_file":false,"injectable":{"exec":false,"shell":false}}],"dynamic_leases":[],"daemon_enabled":false}'
else
  echo '{"schema":1,"fnox_version":"1.39.0","scope":"exec","profile":["default"],"set":{"DEPLOY_KEY":"leak-sentinel-deploy-0001","SENTRY_DSN":"leak-sentinel-sentry-0002","SHORT_KEY":"short77","DEP":"leak-sentinel-dependency-0003"},"files":{},"remove":["HIDDEN","PGHOST","DATABASE_URL","PATH","MISE_SHELL","__MISE_DIFF","STACK_PROJECT"],"missing":[],"leases":[]}'
fi
