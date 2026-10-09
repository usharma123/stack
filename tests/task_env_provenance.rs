#![cfg(unix)]
//! A task's `[env]` values with real mise: `stack run` runs mise against a copy of the
//! generated configuration, and a value evaluated against the configuration that declares it
//! (`{{config_source}}`) must be the one planning read from the project's own file.
//!
//! Opt-in, since it needs a real provider: `STACK_TEST_MISE=/path/to/mise cargo test --test
//! task_env_provenance`. Without it the test reports itself skipped and passes.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const MANIFEST: &str = r#"[[use]]
bundle = 'path:../bundle'
[env]
SOURCE_ASSET = '{{ config_source | dirname }}/../../../message.txt'
ROOT_ASSET = '{{ config_root }}/message.txt'
CHAINED = '{{ env.SOURCE_ASSET }}'
BRACES = '{% raw %}{{ not_a_template }}{% endraw %}'
[tasks.probe]
run = '''
printf 'source=%s\nroot=%s\nchained=%s\nbraces=%s\ntemplate=%s\n' "$SOURCE_ASSET" "$ROOT_ASSET" "$CHAINED" "$BRACES" '{{ env.SOURCE_ASSET }}'
cat "$SOURCE_ASSET" "{{ env.CHAINED }}"
hello
'''
"#;

const SHOW: &str = r#"printf 'source=%s\nroot=%s\nchained=%s\nbraces=%s\n' "$SOURCE_ASSET" "$ROOT_ASSET" "$CHAINED" "$BRACES""#;

fn stack(work: &Path, mise: &Path, args: &[&str]) -> (i32, Value) {
    let dir = |name: &str| work.join(name).to_string_lossy().into_owned();
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", mise.parent().unwrap().display());
    let out = Command::new(env!("CARGO_BIN_EXE_stack"))
        .args(["-C", &dir("app"), "--json"])
        .args(args)
        .env_clear()
        .envs([
            ("HOME", dir("home")),
            ("PATH", path),
            ("STACK_STATE_DIR", dir("state")),
            ("STACK_CACHE_DIR", dir("cache")),
            ("MISE_DATA_DIR", dir("mise/data")),
            ("MISE_CACHE_DIR", dir("mise/cache")),
            ("MISE_STATE_DIR", dir("mise/state")),
            ("XDG_CONFIG_HOME", dir("home/.config")),
            ("XDG_STATE_HOME", dir("home/.local/state")),
            ("NO_COLOR", "1".into()),
        ])
        .current_dir(work)
        .output()
        .unwrap();
    let json = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
    (out.status.code().unwrap_or(-1), json)
}

fn lines(output: &Value) -> Vec<String> {
    output["data"]["stdout"].as_str().unwrap().lines().map(String::from).collect()
}

#[test]
fn a_task_sees_the_env_values_planning_read_from_the_project_configuration() {
    let Some(mise) = std::env::var_os("STACK_TEST_MISE") else {
        eprintln!("skipped: set STACK_TEST_MISE to a real mise binary");
        return;
    };
    let mise = Path::new(&mise).canonicalize().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().canonicalize().unwrap();
    for sub in ["app", "bundle/bin", "home", "state", "cache", "mise/data", "mise/cache", "mise/state"] {
        fs::create_dir_all(work.join(sub)).unwrap();
    }
    fs::write(work.join("bundle/bundle.toml"), "[bundle]\nname = 'probe'\n[paths]\nbin = ['bin']\n").unwrap();
    let hello = work.join("bundle/bin/hello");
    fs::write(&hello, "#!/bin/sh\necho hello-from-bundle-path\n").unwrap();
    fs::set_permissions(&hello, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(work.join("app/message.txt"), "harmless-project-message\n").unwrap();
    fs::write(work.join("app/stack.toml"), MANIFEST).unwrap();

    let (code, compiled) = stack(&work, &mise, &["compile"]);
    assert_eq!(code, 0, "{compiled}");
    // What planning reads from the project's own configuration, as any command receives it.
    let (code, planned) = stack(&work, &mise, &["exec", "--", "sh", "-c", SHOW]);
    assert_eq!((code, &planned["data"]["exit_code"]), (0, &Value::from(0)), "{planned}");
    let planned = lines(&planned);
    let generated = work.join("app/.config/mise/conf.d/stack.toml");
    let asset = format!("{}/../../../message.txt", generated.parent().unwrap().display());
    assert_eq!(
        planned,
        [
            format!("source={asset}"),
            format!("root={}/app/message.txt", work.display()),
            format!("chained={asset}"),
            "braces={{ not_a_template }}".to_string(),
        ]
    );

    let (code, ran) = stack(&work, &mise, &["run", "probe"]);
    assert_eq!((code, &ran["data"]["exit_code"]), (0, &Value::from(0)), "{ran}");
    let ran = lines(&ran);
    // The same values in the task's shell and in its `{{env.X}}` templates; the asset is read
    // twice from the project, and the bundle's `_.path` entry still puts `hello` on PATH.
    assert_eq!(ran[..4], planned[..], "{ran:?}");
    assert_eq!(
        ran[4..],
        [
            format!("template={asset}"),
            "harmless-project-message".to_string(),
            "harmless-project-message".to_string(),
            "hello-from-bundle-path".to_string(),
        ],
        "{ran:?}"
    );
    let copies: Vec<_> = fs::read_dir(work.join("cache/task-config")).unwrap().collect();
    assert!(copies.is_empty(), "{copies:?}");
}
