#![cfg(unix)]
//! A task's `[env]` values with real mise: `stack run` runs mise against a copy of the
//! generated configuration, and a value evaluated against the configuration that declares it
//! (`{{config_source}}`) must be the one planning read from the project's own file. An
//! explicit value keeps its precedence over the one a tool sets (`JAVA_HOME`, `GOROOT`).
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
    // A `mise` wrapper (see `observe`) comes first when the test installs one.
    let path = format!("{}:{}:/usr/bin:/bin:/usr/sbin:/sbin", dir("observer"), mise.parent().unwrap().display());
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

/// Install a `mise` that keeps a copy of the configuration each `mise run` is given in
/// `<work>/seen/`, then runs the real one offline (the tools are local directories).
fn observe(work: &Path, mise: &Path) {
    fs::create_dir_all(work.join("observer")).unwrap();
    fs::create_dir_all(work.join("seen")).unwrap();
    let wrapper = work.join("observer/mise");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = run ] && [ -n \"$MISE_GLOBAL_CONFIG_FILE\" ]; then cp \"$MISE_GLOBAL_CONFIG_FILE\" \"{seen}/$$.toml\"; fi\nMISE_OFFLINE=1 exec \"{mise}\" \"$@\"\n",
        seen = work.join("seen").display(),
        mise = mise.display(),
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
}

const TOOL_SHOW: &str = r#"printf 'java=%s\ngo=%s\ndollar=%s\n' "$JAVA_HOME" "$GOROOT" "$DOLLAR"; cat "$JAVA_HOME/marker.txt" "$GOROOT/marker.txt""#;

#[test]
fn explicit_env_values_keep_their_precedence_over_a_tools_environment_in_a_task() {
    let Some(mise) = std::env::var_os("STACK_TEST_MISE") else {
        eprintln!("skipped: set STACK_TEST_MISE to a real mise binary");
        return;
    };
    let mise = Path::new(&mise).canonicalize().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().canonicalize().unwrap();
    for sub in ["app", "home", "state", "cache", "mise/data", "mise/cache", "mise/state"] {
        fs::create_dir_all(work.join(sub)).unwrap();
    }
    // Two local tool directories mise's Java and Go backends export as JAVA_HOME and GOROOT,
    // and the explicit directories `[env]` names instead; each holds a marker naming itself.
    for name in ["java-install", "go-install", "explicit-java", "explicit-go"] {
        fs::create_dir_all(work.join(name).join("bin")).unwrap();
        fs::write(work.join(name).join("marker.txt"), format!("{name}\n")).unwrap();
    }
    let at = |name: &str| work.join(name).display().to_string();
    let manifest = format!(
        r#"[tools]
java = "path:{java}"
go = "path:{go}"
[env]
JAVA_HOME = "{explicit_java}"
GOROOT = "{explicit_go}"
DOLLAR = 'cost $$HOME {{% raw %}}{{{{ env.HOME }}}}{{% endraw %}}'
[tasks.probe]
run = """
{show}
printf 'template-java=%s\ntemplate-go=%s\n' '{{{{ env.JAVA_HOME }}}}' '{{{{ env.GOROOT }}}}'
"""
"#,
        java = at("java-install"),
        go = at("go-install"),
        explicit_java = at("explicit-java"),
        explicit_go = at("explicit-go"),
        show = TOOL_SHOW,
    );
    fs::write(work.join("app/stack.toml"), manifest).unwrap();
    observe(&work, &mise);

    let (code, compiled) = stack(&work, &mise, &["compile"]);
    assert_eq!(code, 0, "{compiled}");
    let planned_lines = [
        format!("java={}", at("explicit-java")),
        format!("go={}", at("explicit-go")),
        "dollar=cost $HOME {{ env.HOME }}".to_string(),
        "explicit-java".to_string(),
        "explicit-go".to_string(),
    ];
    let (code, planned) = stack(&work, &mise, &["exec", "--", "sh", "-c", TOOL_SHOW]);
    assert_eq!((code, &planned["data"]["exit_code"]), (0, &Value::from(0)), "{planned}");
    assert_eq!(lines(&planned), planned_lines);
    let templates = [format!("template-java={}", at("explicit-java")), format!("template-go={}", at("explicit-go"))];
    let expected: Vec<String> = planned_lines.iter().chain(&templates).cloned().collect();

    // Control: mise running the task from the project's own configuration.
    let control = ["exec", "--", "mise", "run", "--skip-deps", "--no-timings", "probe", "--"];
    let (code, original) = stack(&work, &mise, &control);
    assert_eq!((code, &original["data"]["exit_code"]), (0, &Value::from(0)), "{original}");
    assert_eq!(lines(&original), expected, "{original}");

    let (code, ran) = stack(&work, &mise, &["run", "probe"]);
    assert_eq!((code, &ran["data"]["exit_code"]), (0, &Value::from(0)), "{ran}");
    assert_eq!(lines(&ran), expected, "{ran}");

    // The copy `mise run` read names each variable; it holds none of the planned values.
    let seen: Vec<String> = fs::read_dir(work.join("seen"))
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .filter(|config| config.contains("Copied by stack"))
        .collect();
    assert_eq!(seen.len(), 1, "{seen:?}");
    for value in [at("explicit-java"), at("explicit-go"), "cost".to_string()] {
        assert!(!seen[0].contains(&value), "{value} written into the copy: {}", seen[0]);
    }
    let copies: Vec<_> = fs::read_dir(work.join("cache/task-config")).unwrap().collect();
    assert!(copies.is_empty(), "{copies:?}");
}
