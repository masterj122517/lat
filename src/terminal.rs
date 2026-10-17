use crate::{command, discovery::AgentKey};
#[cfg(any(target_os = "macos", test))]
use std::path::Path;
use std::{collections::HashSet, process::Command, time::Duration};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const TIMEOUT: Duration = Duration::from_secs(3);

/// Revalidates the process and focuses its existing terminal; never sends input,
/// launches an agent, or steals its PTY into another process.
pub(crate) fn focus(key: AgentKey) -> Result<(), String> {
    let mut system = System::new();
    let mut current = Pid::from_u32(key.root_pid);
    let mut ancestors = HashSet::new();
    #[cfg(target_os = "linux")]
    let mut window_id = None;
    loop {
        if !ancestors.insert(current.as_u32()) {
            return Err("Cycle in agent process ancestry".into());
        }
        let refresh = ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .without_tasks();
        #[cfg(target_os = "linux")]
        let refresh = if current.as_u32() == key.root_pid {
            refresh.with_environ(UpdateKind::Always)
        } else {
            refresh
        };
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&[current]), true, refresh);
        let Some(process) = system.process(current) else {
            if current.as_u32() == key.root_pid {
                return Err("Agent process is no longer available".into());
            }
            break;
        };
        if current.as_u32() == key.root_pid && process.start_time() != key.start_time {
            return Err("Agent process identity changed; refresh and select it again".into());
        }
        // A failed tmux inventory must not make a tmux child look like a directly
        // focusable GUI terminal. Its inherited GUI identifiers refer to a client.
        if process
            .exe()
            .is_some_and(|path| path.file_name().is_some_and(|name| name == "tmux"))
        {
            return Err(
                "Agent belongs to tmux but its pane is unavailable; refresh before jumping".into(),
            );
        }
        #[cfg(target_os = "linux")]
        if current.as_u32() == key.root_pid {
            window_id = process
                .environ()
                .iter()
                .find_map(|value| value.to_str()?.strip_prefix("WINDOWID=").map(str::to_owned));
        }
        let Some(parent) = process.parent().filter(|parent| parent.as_u32() > 1) else {
            break;
        };
        current = parent;
    }
    let output = command::output(
        Command::new("ps").args(["-p", &key.root_pid.to_string(), "-o", "tty="]),
        TIMEOUT,
    )?;
    let tty = parse_tty(&String::from_utf8_lossy(&output.stdout))?;
    #[cfg(target_os = "macos")]
    {
        // macOS login/launcher ancestors can exit while the terminal stays open.
        // TTY ownership is authoritative even when that ancestry is incomplete.
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .without_tasks(),
        );
        let mut failures = Vec::new();
        for application in [
            MacTerminal::Terminal,
            MacTerminal::ITerm,
            MacTerminal::Ghostty,
        ] {
            if !system.processes().values().any(|process| {
                process.exe().and_then(MacTerminal::from_path).as_ref() == Some(&application)
            }) {
                continue;
            }
            match command::output(
                Command::new("osascript").args(["-e", application.script(), &tty]),
                TIMEOUT,
            ) {
                Ok(_) => return Ok(()),
                Err(error) => failures.push(error),
            }
        }
        Err(format!(
            "Could not focus terminal {tty}: {}. Supported: Terminal, iTerm2, Ghostty with TTY scripting. Check macOS Automation permission",
            if failures.is_empty() {
                "No supported terminal is running".into()
            } else {
                failures.join("; ")
            }
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = tty;
        if std::env::var_os("DISPLAY").is_none() {
            return Err("External-terminal jumps require an X11 DISPLAY and wmctrl; native Wayland focus is not supported".into());
        }
        let output = command::output(Command::new("wmctrl").arg("-lp"), TIMEOUT)?;
        let window = select_x11_window(
            &String::from_utf8_lossy(&output.stdout),
            window_id.as_deref(),
            &ancestors,
        )?;
        command::output(
            Command::new("wmctrl").args(["-i", "-a", &format!("0x{window:x}")]),
            TIMEOUT,
        )
        .map(|_| ())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = tty;
        Err("External-terminal jumps are unsupported on this platform".into())
    }
}

fn parse_tty(value: &str) -> Result<String, String> {
    let value = value.trim();
    let name = value.strip_prefix("/dev/").unwrap_or(value);
    if !(name.starts_with("ttys") || name.starts_with("pts/") || name.starts_with("tty"))
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '/')
    {
        return Err("Agent has no usable controlling terminal".into());
    }
    Ok(format!("/dev/{name}"))
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum MacTerminal {
    Terminal,
    ITerm,
    Ghostty,
}

#[cfg(any(target_os = "macos", test))]
impl MacTerminal {
    fn from_path(path: &Path) -> Option<Self> {
        let path = path.to_str()?;
        if path.ends_with("/Terminal.app/Contents/MacOS/Terminal") {
            Some(Self::Terminal)
        } else if path.ends_with("/iTerm.app/Contents/MacOS/iTerm2") {
            Some(Self::ITerm)
        } else if path.ends_with("/Ghostty.app/Contents/MacOS/ghostty") {
            Some(Self::Ghostty)
        } else {
            None
        }
    }

    #[cfg(target_os = "macos")]
    fn script(&self) -> &'static str {
        match self {
            Self::Terminal => {
                r#"on run argv
set targetTTY to item 1 of argv
tell application id "com.apple.Terminal"
    repeat with w in windows
        repeat with t in tabs of w
            if tty of t is targetTTY then
                set selected tab of w to t
                set miniaturized of w to false
                set frontmost of w to true
                activate
                return
            end if
        end repeat
    end repeat
end tell
error "No Terminal tab owns the agent TTY"
end run"#
            }
            Self::ITerm => {
                r#"on run argv
set targetTTY to item 1 of argv
tell application id "com.googlecode.iterm2"
    repeat with w in windows
        repeat with tabItem in tabs of w
            repeat with s in sessions of tabItem
                if tty of s is targetTTY then
                    select s
                    select tabItem
                    select w
                    activate
                    return
                end if
            end repeat
        end repeat
    end repeat
end tell
error "No iTerm2 session owns the agent TTY"
end run"#
            }
            Self::Ghostty => {
                r#"on run argv
set targetTTY to item 1 of argv
tell application id "com.mitchellh.ghostty"
    repeat with t in terminals
        try
            set terminalTTY to «property Gtty» of t
        on error
            error "This Ghostty version does not expose terminal TTYs; upgrade Ghostty and enable macos-applescript"
        end try
        if terminalTTY is targetTTY then
            focus t
            activate
            return
        end if
    end repeat
end tell
error "No Ghostty terminal owns the agent TTY; enable macos-applescript and use a version exposing TTYs"
end run"#
            }
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn select_x11_window(
    list: &str,
    preferred: Option<&str>,
    ancestors: &HashSet<u32>,
) -> Result<u64, String> {
    let preferred = preferred
        .map(|value| {
            value
                .strip_prefix("0x")
                .map_or_else(|| value.parse::<u64>(), |hex| u64::from_str_radix(hex, 16))
                .map_err(|_| "Invalid terminal WINDOWID")
        })
        .transpose()?;
    let mut matches = HashSet::new();
    for line in list.lines() {
        let mut fields = line.split_whitespace();
        let Some(id) = fields
            .next()
            .and_then(|id| id.strip_prefix("0x"))
            .and_then(|id| u64::from_str_radix(id, 16).ok())
        else {
            continue;
        };
        let _desktop = fields.next();
        let Some(pid) = fields.next().and_then(|pid| pid.parse().ok()) else {
            continue;
        };
        if id != 0 && ancestors.contains(&pid) {
            matches.insert(id);
        }
    }
    if let Some(preferred) = preferred {
        return matches
            .contains(&preferred)
            .then_some(preferred)
            .ok_or_else(|| {
                "Terminal WINDOWID no longer belongs to the agent's process ancestry".into()
            });
    }
    if matches.len() == 1 {
        return Ok(*matches.iter().next().unwrap());
    }
    Err(if matches.is_empty() {
        "No X11 terminal window belongs to the agent"
    } else {
        "Multiple terminal windows belong to this process; no unambiguous WINDOWID"
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_absent_or_malformed_ttys() {
        assert_eq!(parse_tty(" ttys012\n").unwrap(), "/dev/ttys012");
        assert_eq!(parse_tty("pts/4").unwrap(), "/dev/pts/4");
        for tty in ["??", "?", "", "ttys0; echo bad", "/tmp/ttys0", "pts/../1"] {
            assert!(parse_tty(tty).is_err());
        }
    }
    #[test]
    fn only_known_native_terminal_ancestors_are_supported() {
        assert_eq!(
            MacTerminal::from_path(Path::new(
                "/Applications/Ghostty.app/Contents/MacOS/ghostty"
            )),
            Some(MacTerminal::Ghostty)
        );
        assert_eq!(MacTerminal::from_path(Path::new("/tmp/ghostty")), None);
    }
    #[test]
    fn x11_focus_requires_unambiguous_ancestry_and_validates_windowid() {
        let ancestors = HashSet::from([42, 43]);
        let windows = "0x00100001 0 42 host terminal A\n0x00100002 0 42 host terminal B\n0x00100003 0 99 host unrelated";
        assert!(select_x11_window(windows, None, &ancestors).is_err());
        assert_eq!(
            select_x11_window(windows, Some("1048578"), &ancestors).unwrap(),
            0x100002
        );
        assert!(select_x11_window(windows, Some("0x100003"), &ancestors).is_err());
        assert!(select_x11_window(windows, Some("--bad"), &ancestors).is_err());
        assert_eq!(
            select_x11_window("0x00100001 0 43 host terminal", None, &ancestors).unwrap(),
            0x100001
        );
    }
}
