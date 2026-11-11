//! `stack setup --gc-agent`: a per-user launchd agent (macOS) or systemd user unit (Linux)
//! that runs `stack gc --watch`. Stack writes the file and prints the command that loads it;
//! it loads or unloads nothing itself unless asked to with `--enable`.

use crate::error::{io_error, Result, StackError};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LAUNCHD_LABEL: &str = "dev.stack.gc";
pub const SYSTEMD_UNIT: &str = "stack-gc.service";

/// Variables the agent keeps from the shell that wrote it: `PATH` (to find mise, Pitchfork and
/// node for the npm launcher) and the stores Stack and Pitchfork use, when set.
const PASSED: &[&str] = &[
    "PATH",
    "STACK_STATE_DIR",
    "STACK_CACHE_DIR",
    "STACK_DATA_DIR",
    "PITCHFORK_STATE_DIR",
    "MISE_DATA_DIR",
    "MISE_CACHE_DIR",
    "MISE_STATE_DIR",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Launchd,
    Systemd,
}

impl Platform {
    pub fn current() -> Result<Self> {
        match std::env::consts::OS {
            "macos" => Ok(Self::Launchd),
            "linux" => Ok(Self::Systemd),
            other => Err(StackError::new("unsupported_platform", format!("no user service manager Stack knows on {other}"))
                .hint("run `stack gc --watch` under your own supervisor")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Show the file; write nothing.
    Print,
    Write,
    Remove,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub platform: Platform,
    pub path: PathBuf,
    /// The file's content (not for `--remove`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub written: bool,
    pub removed: bool,
    /// The command that loads (or, for `--remove`, unloads) the agent.
    pub command: String,
    /// Whether Stack ran `command` (`--enable`).
    pub ran: bool,
}

/// Where the agent's file lives for the user whose home is `home`.
pub fn path(platform: Platform, home: &Path, xdg_config: Option<&Path>) -> PathBuf {
    match platform {
        Platform::Launchd => home.join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist")),
        Platform::Systemd => xdg_config
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".config"))
            .join("systemd/user")
            .join(SYSTEMD_UNIT),
    }
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// A systemd quoted word: `\` and `"` escaped, `%` doubled (specifier syntax), `$` doubled.
fn systemd_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$"))
}

pub fn render(platform: Platform, program: &Path, env: &[(String, String)], log: &Path) -> String {
    let program = program.to_string_lossy();
    match platform {
        Platform::Launchd => {
            let vars: String = env
                .iter()
                .map(|(k, v)| format!("    <key>{}</key>\n    <string>{}</string>\n", xml(k), xml(v)))
                .collect();
            let log = xml(&log.to_string_lossy());
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `stack setup --gc-agent`: reclaims expired Stack sessions. Remove with `stack setup --gc-agent --remove`. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>gc</string>
    <string>--watch</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{vars}  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
                xml(&program)
            )
        }
        Platform::Systemd => {
            let vars: String = env.iter().map(|(k, v)| format!("Environment={}\n", systemd_quote(&format!("{k}={v}")))).collect();
            format!(
                "# Written by `stack setup --gc-agent`: reclaims expired Stack sessions.\n\
                 # Remove with `stack setup --gc-agent --remove`.\n\
                 [Unit]\n\
                 Description=Stack: reclaim expired sessions (stack gc --watch)\n\
                 \n\
                 [Service]\n\
                 ExecStart={} gc --watch\n\
                 {vars}\
                 Restart=on-failure\n\
                 RestartSec=30\n\
                 \n\
                 [Install]\n\
                 WantedBy=default.target\n",
                systemd_quote(&program)
            )
        }
    }
}

/// The command that loads (`load`) or unloads the agent at `path`.
pub fn command(platform: Platform, path: &Path, load: bool) -> String {
    let quoted = shell_word(&path.to_string_lossy());
    match (platform, load) {
        (Platform::Launchd, true) => format!("launchctl bootstrap gui/$(id -u) {quoted}"),
        (Platform::Launchd, false) => format!("launchctl bootout gui/$(id -u)/{LAUNCHD_LABEL}"),
        (Platform::Systemd, true) => format!("systemctl --user daemon-reload && systemctl --user enable --now {SYSTEMD_UNIT}"),
        (Platform::Systemd, false) => format!("systemctl --user disable --now {SYSTEMD_UNIT}"),
    }
}

fn shell_word(s: &str) -> String {
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+".contains(&b)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The `stack` a supervisor should run: the one on `PATH` (an npm install's launcher keeps its
/// path across upgrades), else this executable.
fn program() -> Result<PathBuf> {
    if let Some(found) = crate::session::which_in(std::env::var("PATH").ok().as_deref(), "stack") {
        return Ok(found);
    }
    std::env::current_exe().map_err(|e| io_error("locating the stack executable", e))
}

fn run_shell(command: &str) -> Result<()> {
    let status = Command::new("sh")
        .args(["-c", command])
        .status()
        .map_err(|e| io_error(format!("running `{command}`"), e))?;
    if status.success() {
        Ok(())
    } else {
        Err(StackError::new("agent_command_failed", format!("`{command}` failed ({status})"))
            .hint("the file is in place; run the command yourself to see why"))
    }
}

/// Print, write or remove the agent for the current platform under `home`. `enable` also runs
/// the load (or, for `Remove`, unload) command.
pub fn run(action: Action, enable: bool, home: &Path) -> Result<Report> {
    let platform = Platform::current()?;
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute());
    let path = path(platform, home, xdg.as_deref());
    if action == Action::Remove {
        let command = command(platform, &path, false);
        let mut ran = false;
        if enable && path.exists() {
            run_shell(&command)?;
            ran = true;
        }
        let removed = match fs::remove_file(&path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(io_error(path.display(), e)),
        };
        return Ok(Report { platform, path, content: None, written: false, removed, command, ran });
    }
    let env: Vec<(String, String)> =
        PASSED.iter().filter_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| (k.to_string(), v))).collect();
    let log = match platform {
        Platform::Launchd => home.join("Library/Logs/stack-gc.log"),
        // journald keeps a user unit's output.
        Platform::Systemd => PathBuf::from("journal"),
    };
    let content = render(platform, &program()?, &env, &log);
    let command = command(platform, &path, true);
    if action == Action::Print {
        return Ok(Report { platform, path, content: Some(content), written: false, removed: false, command, ran: false });
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| io_error(dir.display(), e))?;
    }
    fs::write(&path, &content).map_err(|e| io_error(path.display(), e))?;
    let ran = if enable {
        run_shell(&command)?;
        true
    } else {
        false
    };
    Ok(Report { platform, path, content: Some(content), written: true, removed: false, command, ran })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_agents_escape_values_and_run_gc_watch() {
        let env = vec![("PATH".to_string(), "/a&b:/usr/bin".to_string())];
        let plist = render(Platform::Launchd, Path::new("/opt/st<ack>/stack"), &env, Path::new("/h/Library/Logs/stack-gc.log"));
        assert!(plist.contains("<string>/opt/st&lt;ack&gt;/stack</string>\n    <string>gc</string>\n    <string>--watch</string>"), "{plist}");
        assert!(plist.contains("<string>/a&amp;b:/usr/bin</string>"), "{plist}");
        assert!(plist.contains(&format!("<string>{LAUNCHD_LABEL}</string>")));
    }

    #[test]
    fn systemd_units_quote_specifiers_and_paths() {
        let env = vec![("PATH".to_string(), "/x 1:/y%z".to_string())];
        let unit = render(Platform::Systemd, Path::new("/home/u/.npm global/bin/stack"), &env, Path::new("journal"));
        assert!(unit.contains("ExecStart=\"/home/u/.npm global/bin/stack\" gc --watch\n"), "{unit}");
        assert!(unit.contains("Environment=\"PATH=/x 1:/y%%z\"\n"), "{unit}");
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn paths_and_commands_per_platform() {
        let home = Path::new("/h");
        assert_eq!(path(Platform::Launchd, home, None), Path::new("/h/Library/LaunchAgents/dev.stack.gc.plist"));
        assert_eq!(path(Platform::Systemd, home, None), Path::new("/h/.config/systemd/user/stack-gc.service"));
        assert_eq!(path(Platform::Systemd, home, Some(Path::new("/c"))), Path::new("/c/systemd/user/stack-gc.service"));
        assert!(command(Platform::Launchd, Path::new("/h/a b.plist"), true).ends_with("'/h/a b.plist'"));
        assert!(command(Platform::Systemd, home, false).contains("disable --now"));
    }
}
