//! `globalobject setup`: join a pool (or come back to one) in a few questions.

use crate::{
    config::{default_keep, format_size, parse_size, BackupConfig, Config, Credentials, DashboardConfig, Paths, SliceConfig, DEFAULT_DASHBOARD_PORT},
    service, Client,
};
use anyhow::{bail, Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password};
use globalobject_net::NetMode;
use std::{io::IsTerminal, path::PathBuf};

#[derive(clap::Args, Debug, Default)]
pub struct SetupArgs {
    /// The join link you were sent, like https://cp.example.org/join/1a2b3c...
    #[arg(long)]
    pub join: Option<String>,
    /// The control plane's address, when using a bare invite code or --restore.
    #[arg(long, env = "GLOBALOBJECT_CONTROL_PLANE")]
    pub server: Option<String>,
    /// A bare invite code, with --server.
    #[arg(long)]
    pub invite: Option<String>,
    /// Use an existing account, with its recovery phrase: for a new or reinstalled machine.
    #[arg(long)]
    pub restore: bool,
    /// Recovery phrase to use instead of making a new one. Prefer typing it when asked.
    #[arg(long, env = "GLOBALOBJECT_PHRASE", hide_env_values = true)]
    pub phrase: Option<String>,
    #[arg(long, env = "GLOBALOBJECT_PASSPHRASE", hide_env_values = true)]
    pub passphrase: Option<String>,
    /// Disk to share with the pool, like 500GB.
    #[arg(long)]
    pub slice: Option<String>,
    /// Where the shared slice lives.
    #[arg(long)]
    pub slice_dir: Option<PathBuf>,
    /// A folder to back up. Repeat for several.
    #[arg(long = "path")]
    pub paths: Vec<PathBuf>,
    /// Hours between backups. 0 turns scheduled backups off.
    #[arg(long)]
    pub every_hours: Option<u32>,
    /// Port for the dashboard on this machine.
    #[arg(long)]
    pub dashboard_port: Option<u16>,
    /// Accept every default without asking. For scripts.
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Don't install the background service (run `globalobject agent` yourself).
    #[arg(long)]
    pub no_service: bool,
    /// Loopback only, with no relays. For local test grids.
    #[arg(long, env = "GLOBALOBJECT_LOCAL")]
    pub local: bool,
}

/// `https://host/join/<token>` into the control plane address and the invite.
pub fn parse_join(link: &str) -> Result<(String, String)> {
    let link = link.trim();
    let Some((server, token)) = link.rsplit_once("/join/") else {
        bail!("'{link}' doesn't look like a join link (https://.../join/<code>)");
    };
    let token = token.trim_end_matches('/');
    if server.is_empty() || token.is_empty() || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("'{link}' doesn't look like a join link (https://.../join/<code>)");
    }
    Ok((server.to_string(), token.to_string()))
}

/// A new 12-word recovery phrase from the BIP39 English list: words that are easy to write
/// down and type back. Any phrase works for an existing account; this only makes new ones.
pub fn new_phrase() -> String {
    bip39::Mnemonic::generate(12).expect("12 is a valid word count").to_string()
}

/// Which word positions to ask for back, and whether the answers match.
pub fn check_words(phrase: &str, answers: &[(usize, String)]) -> bool {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    answers.iter().all(|(i, a)| words.get(*i).is_some_and(|w| w.eq_ignore_ascii_case(a.trim())))
}

fn default_paths() -> Vec<PathBuf> {
    let Some(dirs) = directories::UserDirs::new() else { return vec![] };
    [dirs.document_dir(), dirs.picture_dir(), dirs.desktop_dir()].into_iter().flatten().filter(|p| p.is_dir()).map(PathBuf::from).collect()
}

/// A quarter of the free space where the slice will live, rounded down to 10 GB.
pub fn suggest_slice(dir: &std::path::Path) -> Option<u64> {
    let mut probe = dir.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    let free = fs4::available_space(&probe).ok()?;
    let step = 10_000_000_000;
    Some((free / 4 / step * step).max(step.min(free / 2)))
}

pub async fn run(a: SetupArgs) -> Result<()> {
    run_with(&Paths::detect()?, a).await
}

pub async fn run_with(paths: &Paths, a: SetupArgs) -> Result<()> {
    if paths.is_set_up() {
        let (cfg, _) = paths.load()?;
        println!("This machine is already set up with {}.", cfg.control_plane);
        println!("See `globalobject status`, or remove {} to start again.", paths.config_dir.display());
        return Ok(());
    }
    let ask = !a.yes;
    if ask && !std::io::stdin().is_terminal() {
        bail!("setup needs a terminal to ask questions. Run it in one, or pass --yes with the options you want");
    }
    let theme = ColorfulTheme::default();
    println!("GlobalObject: encrypted backup, stored on other members' machines in exchange for some of your spare disk.\n");

    // Where, and with which invite.
    let (server, invite) = match (&a.join, &a.server, &a.invite) {
        (Some(link), _, _) => {
            let (s, t) = parse_join(link)?;
            (s, Some(t))
        }
        (None, Some(s), inv) => (s.trim_end_matches('/').to_string(), inv.clone()),
        (None, None, _) if ask => {
            let link: String = Input::with_theme(&theme).with_prompt("Paste your join link").interact_text()?;
            let (s, t) = parse_join(&link)?;
            (s, Some(t))
        }
        _ => bail!("pass --join <link>, or --server with --invite or --restore"),
    };
    if !a.restore && invite.is_none() {
        bail!("an invite is needed to join. Coming back to an existing account? Use --restore");
    }

    // The recovery phrase.
    let passphrase = a.passphrase.clone().unwrap_or_default();
    let phrase = if a.restore {
        match &a.phrase {
            Some(p) => p.clone(),
            None if ask => Password::with_theme(&theme).with_prompt("Recovery phrase").interact()?,
            None => bail!("--restore needs the recovery phrase (GLOBALOBJECT_PHRASE)"),
        }
    } else {
        match &a.phrase {
            Some(p) => p.clone(),
            None => {
                let p = new_phrase();
                println!("Your recovery phrase:\n");
                for (i, chunk) in p.split_whitespace().collect::<Vec<_>>().chunks(4).enumerate() {
                    let line: Vec<String> = chunk.iter().enumerate().map(|(j, w)| format!("{:>2}. {w:<10}", i * 4 + j + 1)).collect();
                    println!("    {}", line.join(" "));
                }
                println!("\nWrite these 12 words down and keep them somewhere safe. They are the only way to");
                println!("restore your backups on another machine. Nobody, including the operator, can recover");
                println!("them for you.\n");
                if ask {
                    Confirm::with_theme(&theme).with_prompt("I've written them down").default(true).wait_for_newline(true).interact()?;
                    // Clear the words off the screen before asking for some of them back.
                    print!("\x1b[2J\x1b[H");
                    let picks = [2usize, 6, 10];
                    loop {
                        let answers: Vec<(usize, String)> = picks
                            .iter()
                            .map(|&i| Input::<String>::with_theme(&theme).with_prompt(format!("Word {}", i + 1)).interact_text().map(|w| (i, w)))
                            .collect::<Result<_, _>>()?;
                        if check_words(&p, &answers) {
                            break;
                        }
                        println!("That doesn't match. Here it is again: {p}");
                    }
                }
                p
            }
        }
    };

    // The slice.
    let slice_dir = a.slice_dir.clone().unwrap_or_else(|| paths.default_slice_dir());
    let suggested = suggest_slice(&slice_dir).unwrap_or(50_000_000_000);
    let slice = match &a.slice {
        Some(s) => parse_size(s)?,
        None if ask => {
            println!("\nYou share some disk with the pool; in return you get backup space, about a quarter of");
            println!("what you share with the usual 3 copies. Your data on others' machines is encrypted, and");
            println!("so is theirs on yours.");
            loop {
                let s: String = Input::with_theme(&theme)
                    .with_prompt(format!("Disk to share (in {})", slice_dir.display()))
                    .default(format_size(suggested))
                    .interact_text()?;
                match parse_size(&s) {
                    Ok(n) if n >= 1_000_000_000 => break n,
                    Ok(_) => println!("Share at least 1 GB."),
                    Err(e) => println!("{e}"),
                }
            }
        }
        None => suggested,
    };

    // What to back up, and how often.
    let backup_paths = if !a.paths.is_empty() {
        a.paths.clone()
    } else if ask {
        let defaults = default_paths();
        let shown = defaults.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
        let s: String = Input::with_theme(&theme).with_prompt("Folders to back up (comma-separated)").default(shown).allow_empty(true).interact_text()?;
        s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(PathBuf::from).collect()
    } else {
        default_paths()
    };
    for p in &backup_paths {
        if !p.exists() {
            bail!("{} doesn't exist", p.display());
        }
    }
    let every_hours = match a.every_hours {
        Some(h) => h,
        None if ask => Input::with_theme(&theme).with_prompt("Back up every how many hours").default(24u32).interact_text()?,
        None => 24,
    };

    // Join (or find the account), then write everything down.
    let net = if a.local { NetMode::Local } else { NetMode::Internet };
    let state = paths.state_dir();
    println!("\nConnecting to {server}...");
    let client = if a.restore {
        Client::recover(&server, &phrase, &passphrase, &state, net).await?
    } else {
        Client::register(&server, invite.as_deref().unwrap_or_default(), &phrase, &passphrase, &state, net).await.context("joining failed")?
    };
    let (account, pool) = (client.id(), client.pool());
    client.shutdown().await;

    let cfg = Config {
        control_plane: server.clone(),
        local: a.local,
        backup: BackupConfig { paths: backup_paths, every_hours, keep: default_keep() },
        slice: SliceConfig { dir: slice_dir, size: slice, upload_limit: 0 },
        dashboard: DashboardConfig { port: a.dashboard_port.unwrap_or(DEFAULT_DASHBOARD_PORT) },
    };
    std::fs::create_dir_all(&cfg.slice.dir).with_context(|| format!("create {}", cfg.slice.dir.display()))?;
    paths.save_credentials(&Credentials { phrase, passphrase, dashboard_token: globalobject_crypto::random_hex(16) })?;
    paths.save_config(&cfg)?;
    println!("Account {account} in pool {pool}. Settings are in {}.", paths.config_file().display());

    if a.no_service {
        println!("\nStart it with `globalobject agent` (it runs the storage slice, backups and the dashboard).");
    } else {
        match service::install(paths) {
            Ok(note) => {
                println!("\nThe background service is installed and running. {note}");
                println!("It shares your slice, backs up {} every {every_hours} hours, and serves a dashboard:", describe(&cfg.backup.paths));
                println!("  globalobject dashboard     open it in your browser");
            }
            Err(e) => {
                println!("\nCouldn't install the background service: {e:#}");
                println!("Run `globalobject agent` yourself instead, or try `globalobject service install` later.");
            }
        }
    }
    println!("  globalobject status        check everything is healthy");
    println!("  globalobject backup        back up now");
    Ok(())
}

fn describe(paths: &[PathBuf]) -> String {
    match paths.len() {
        0 => "nothing yet".into(),
        1 => paths[0].display().to_string(),
        n => format!("{} folders", n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_links() {
        assert_eq!(parse_join("https://cp.example.org/join/0a1b2c").unwrap(), ("https://cp.example.org".into(), "0a1b2c".into()));
        assert_eq!(parse_join(" http://127.0.0.1:7400/join/ff/ ").unwrap(), ("http://127.0.0.1:7400".into(), "ff".into()));
        assert!(parse_join("https://cp.example.org/").is_err());
        assert!(parse_join("https://cp.example.org/join/<script>").is_err());
    }

    #[test]
    fn phrases() {
        let p = new_phrase();
        let words: Vec<&str> = p.split_whitespace().collect();
        assert_eq!(words.len(), 12);
        assert_ne!(p, new_phrase());
        assert!(check_words(&p, &[(2, words[2].to_uppercase()), (6, format!(" {} ", words[6]))]));
        assert!(!check_words(&p, &[(2, "notaword".into())]));
        assert!(!check_words(&p, &[(40, words[0].into())]));
    }
}
