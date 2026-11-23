//! The background service: a systemd user unit on Linux, a LaunchAgent on macOS. Both run
//! `globalobject agent` as the user, never as root.

use crate::config::Paths;
use anyhow::{bail, Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub const UNIT: &str = "globalobject.service";
pub const LABEL: &str = "org.globalobject.agent";

/// What the service runs, and with which environment.
pub struct Spec {
    pub exe: PathBuf,
    /// Set when the user runs with `GLOBALOBJECT_HOME`, so the service finds the same files.
    pub home: Option<PathBuf>,
    pub log_file: PathBuf,
}

impl Spec {
    fn current() -> Result<Spec> {
        let exe = std::env::current_exe().context("can't find this program's path")?;
        let home = std::env::var_os("GLOBALOBJECT_HOME").map(PathBuf::from);
        let log_file = home_dir()?.join("Library/Logs/globalobject.log");
        Ok(Spec { exe, home, log_file })
    }
}

fn home_dir() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new().context("can't find your home directory")?.home_dir().to_path_buf())
}

pub fn systemd_unit(spec: &Spec) -> String {
    let home = spec.home.as_ref().map(|h| format!("Environment=\"GLOBALOBJECT_HOME={}\"\n", h.display())).unwrap_or_default();
    format!(
        "[Unit]\n\
         Description=GlobalObject: encrypted backup and shared storage slice\n\
         Documentation=https://github.com/globalobject\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart=\"{}\" agent\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         Environment=RUST_LOG=info\n\
         {home}\
         # Backups and serving other members shouldn't slow this machine down.\n\
         Nice=10\n\
         IOSchedulingClass=idle\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        spec.exe.display()
    )
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn launchd_plist(spec: &Spec) -> String {
    let mut env = String::from("    <key>RUST_LOG</key><string>info</string>\n");
    if let Some(h) = &spec.home {
        env.push_str(&format!("    <key>GLOBALOBJECT_HOME</key><string>{}</string>\n", xml(&h.display().to_string())));
    }
    let log = xml(&spec.log_file.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>agent</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        xml(&spec.exe.display().to_string())
    )
}

fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd).args(args).output().with_context(|| format!("couldn't run {cmd}"))?;
    if !out.status.success() {
        bail!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn uid() -> Result<String> {
    Ok(run("id", &["-u"])?.trim().to_string())
}

fn unit_path() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new().context("no home directory")?.config_dir().join("systemd/user").join(UNIT))
}

fn plist_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

/// Install and start the service. Returns a note for the user.
pub fn install(paths: &Paths) -> Result<String> {
    if !paths.is_set_up() {
        bail!("run `globalobject setup` first");
    }
    let spec = Spec::current()?;
    if cfg!(target_os = "macos") {
        let p = plist_path()?;
        write(&p, &launchd_plist(&spec))?;
        let domain = format!("gui/{}", uid()?);
        let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
        run("launchctl", &["bootstrap", &domain, &p.display().to_string()])?;
        Ok(format!("It starts when you log in. Logs: {}", spec.log_file.display()))
    } else if cfg!(target_os = "linux") {
        write(&unit_path()?, &systemd_unit(&spec))?;
        run("systemctl", &["--user", "daemon-reload"]).context("systemd user services aren't available here")?;
        run("systemctl", &["--user", "enable", "--now", UNIT])?;
        // Without lingering, user services stop at logout, which would stop backups and take
        // this machine's slice offline.
        let user = std::env::var("USER").unwrap_or_default();
        Ok(match run("loginctl", &["enable-linger", &user]) {
            Ok(_) => "It keeps running when you log out.".into(),
            Err(_) => format!("It stops when you log out; to keep it running, run: sudo loginctl enable-linger {user}"),
        })
    } else {
        bail!("no service support on this system yet: run `globalobject agent` instead")
    }
}

pub fn uninstall() -> Result<()> {
    if cfg!(target_os = "macos") {
        let _ = run("launchctl", &["bootout", &format!("gui/{}/{LABEL}", uid()?)]);
        let _ = std::fs::remove_file(plist_path()?);
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT]);
        let _ = std::fs::remove_file(unit_path()?);
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    Ok(())
}

pub fn control(action: &str) -> Result<()> {
    if cfg!(target_os = "macos") {
        let target = format!("gui/{}/{LABEL}", uid()?);
        match action {
            "start" => run("launchctl", &["kickstart", &target]),
            "stop" => run("launchctl", &["kill", "SIGTERM", &target]),
            _ => run("launchctl", &["kickstart", "-k", &target]),
        }?;
    } else {
        run("systemctl", &["--user", action, UNIT])?;
    }
    Ok(())
}

/// Whether the service is installed and running.
pub fn is_running() -> bool {
    if cfg!(target_os = "macos") {
        uid().and_then(|u| run("launchctl", &["print", &format!("gui/{u}/{LABEL}")])).is_ok_and(|o| o.contains("state = running"))
    } else {
        Command::new("systemctl").args(["--user", "is-active", "--quiet", UNIT]).status().is_ok_and(|s| s.success())
    }
}

/// Show recent logs, following them if asked. Replaces this process's output with the tool's.
pub fn logs(follow: bool) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        let log = Spec::current()?.log_file;
        let mut c = Command::new("tail");
        c.args(["-n", "200"]);
        if follow {
            c.arg("-f");
        }
        c.arg(log).status()?
    } else {
        let mut c = Command::new("journalctl");
        c.args(["--user", "-u", UNIT, "-n", "200", "--no-pager"]);
        if follow {
            c.arg("-f");
        }
        c.status()?
    };
    if !status.success() {
        bail!("couldn't read the logs");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec { exe: "/home/me/.local/bin/globalobject".into(), home: Some("/srv/go & co".into()), log_file: "/Users/me/Library/Logs/globalobject.log".into() }
    }

    #[test]
    fn unit_text() {
        let u = systemd_unit(&spec());
        assert!(u.contains("ExecStart=\"/home/me/.local/bin/globalobject\" agent\n"), "{u}");
        assert!(u.contains("Environment=\"GLOBALOBJECT_HOME=/srv/go & co\"\n"));
        assert!(u.contains("WantedBy=default.target"));
        let plain = systemd_unit(&Spec { home: None, ..spec() });
        assert!(!plain.contains("GLOBALOBJECT_HOME"));
    }

    #[test]
    fn plist_text() {
        let p = launchd_plist(&spec());
        assert!(p.contains("<string>/home/me/.local/bin/globalobject</string>\n    <string>agent</string>"), "{p}");
        assert!(p.contains("<key>GLOBALOBJECT_HOME</key><string>/srv/go &amp; co</string>"));
        assert!(p.contains(&format!("<key>Label</key><string>{LABEL}</string>")));
        assert!(p.contains("<key>StandardErrorPath</key><string>/Users/me/Library/Logs/globalobject.log</string>"));
    }
}
