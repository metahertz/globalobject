//! `globalobject agent`: the background service. It runs this machine's storage slice, backs
//! up on schedule, and serves the dashboard. It also leaves a small status file, so
//! `globalobject status` can report on it without talking to it.

use crate::{
    config::{Config, Credentials, Paths},
    dashboard, Client,
};
use anyhow::{Context, Result};
use globalobject_net::NetMode;
use globalobject_node::{AccountAuth, Node, NodeConfig};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

/// How often the agent checks whether a backup is due, and refreshes its status file.
const TICK: Duration = Duration::from_secs(30);
/// Wait this long after starting before a first or overdue backup, so the slice is online
/// and the machine has settled after boot.
const START_DELAY: Duration = Duration::from_secs(60);

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JobRecord {
    pub kind: String,
    pub started: f64,
    pub finished: Option<f64>,
    pub ok: Option<bool>,
    pub message: String,
}

/// Written to `state/agent.json` every tick.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentStatus {
    pub pid: u32,
    pub version: String,
    pub started: f64,
    pub updated: f64,
    pub node: Option<i64>,
    pub slice_used: u64,
    pub slice_size: u64,
    pub running: Option<JobRecord>,
    pub last_backup: Option<JobRecord>,
    pub next_backup: Option<f64>,
    pub dashboard: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub at: f64,
    pub level: &'static str,
    pub message: String,
}

pub struct Agent {
    pub paths: Paths,
    pub cfg: RwLock<Config>,
    pub creds: Credentials,
    pub client: Client,
    pub node: Arc<Node>,
    status: Mutex<AgentStatus>,
    events: Mutex<VecDeque<Event>>,
    /// One backup, restore or prune at a time: they share the repository.
    job: tokio::sync::Mutex<()>,
}

impl Agent {
    pub fn status(&self) -> AgentStatus {
        let mut s = self.status.lock().unwrap().clone();
        s.updated = now();
        s.slice_used = self.node.used_bytes();
        s.slice_size = self.cfg.read().unwrap().slice.size;
        s
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().iter().rev().cloned().collect()
    }

    pub fn log(&self, level: &'static str, message: impl Into<String>) {
        let message = message.into();
        match level {
            "error" => tracing::error!("{message}"),
            "warn" => tracing::warn!("{message}"),
            _ => tracing::info!("{message}"),
        }
        let mut ev = self.events.lock().unwrap();
        ev.push_back(Event { at: now(), level, message });
        while ev.len() > 200 {
            ev.pop_front();
        }
    }

    fn write_status(&self) {
        let s = self.status();
        let path = self.paths.state_dir().join("agent.json");
        if let Ok(text) = serde_json::to_vec_pretty(&s) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    fn next_backup(&self) -> Option<f64> {
        let every = self.cfg.read().unwrap().backup.every_hours;
        if every == 0 {
            return None;
        }
        let s = self.status.lock().unwrap();
        let last_ok = s.last_backup.as_ref().filter(|r| r.ok == Some(true)).and_then(|r| r.finished);
        let earliest = s.started + START_DELAY.as_secs_f64();
        Some(match (last_ok, &s.last_backup) {
            (Some(t), _) => (t + every as f64 * 3600.0).max(earliest),
            // After a failure, try again in an hour rather than at the next full interval.
            (None, Some(r)) => (r.finished.unwrap_or(r.started) + 3600.0).max(earliest),
            (None, None) => earliest,
        })
    }

    /// Back up the configured folders now, then apply retention. Errors are recorded, not
    /// returned: they show on the dashboard and in `status`.
    pub async fn backup_now(self: &Arc<Self>) {
        let _job = self.job.lock().await;
        let paths = self.cfg.read().unwrap().backup.paths.clone();
        let keep = self.cfg.read().unwrap().backup.keep.clone();
        let mut rec = JobRecord { kind: "backup".into(), started: now(), ..Default::default() };
        self.status.lock().unwrap().running = Some(rec.clone());
        self.log("info", format!("backup started: {}", paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")));
        let result = async {
            let stats = self.client.backup_paths(&paths).await?;
            let mut msg = format!(
                "snapshot {}: {} files, {} read, {} new",
                &stats.snapshot[..8.min(stats.snapshot.len())],
                stats.files,
                crate::config::format_size(stats.bytes_in),
                crate::config::format_size(stats.bytes_added)
            );
            let gone = self.client.forget(&keep).await?;
            if !gone.is_empty() {
                let p = self.client.prune(false).await?;
                msg.push_str(&format!("; {} old snapshots removed, {} freed", gone.len(), crate::config::format_size(p.bytes_removed)));
            }
            anyhow::Ok(msg)
        }
        .await;
        rec.finished = Some(now());
        match result {
            Ok(msg) => {
                rec.ok = Some(true);
                self.log("info", format!("backup finished: {msg}"));
                rec.message = msg;
            }
            Err(e) => {
                rec.ok = Some(false);
                rec.message = format!("{e:#}");
                self.log("error", format!("backup failed: {e:#}"));
            }
        }
        {
            let mut s = self.status.lock().unwrap();
            s.running = None;
            s.last_backup = Some(rec.clone());
        }
        record_backup(&self.paths, &rec);
        self.write_status();
    }

    pub async fn restore(self: &Arc<Self>, snapshot: String, dest: PathBuf) {
        let _job = self.job.lock().await;
        let rec = JobRecord { kind: "restore".into(), started: now(), message: dest.display().to_string(), ..Default::default() };
        self.status.lock().unwrap().running = Some(rec);
        self.log("info", format!("restore of {snapshot} to {} started", dest.display()));
        let r = async {
            std::fs::create_dir_all(&dest)?;
            self.client.restore(&snapshot, &dest).await
        }
        .await;
        match r {
            Ok(()) => self.log("info", format!("restored {snapshot} to {}", dest.display())),
            Err(e) => self.log("error", format!("restore of {snapshot} failed: {e:#}")),
        }
        self.status.lock().unwrap().running = None;
    }

    /// Apply a new slice size: save it, and tell the control plane. Shrinking drains pieces
    /// to other members before the space is released.
    pub async fn set_slice(&self, size: u64) -> Result<()> {
        self.client.set_pledge(self.node.id, size).await?;
        let mut cfg = self.cfg.write().unwrap();
        cfg.slice.size = size;
        self.paths.save_config(&cfg)?;
        drop(cfg);
        self.log("info", format!("slice set to {}", crate::config::format_size(size)));
        Ok(())
    }

    /// Pick up edits to `config.toml` made outside the agent, by hand or by `globalobject
    /// paths` and `globalobject slice`.
    async fn reload_if_changed(&self) {
        let Ok(text) = std::fs::read_to_string(self.paths.config_file()) else { return };
        let new: Config = match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                self.log("warn", format!("config.toml has an error, keeping the old settings: {e}"));
                return;
            }
        };
        let old = self.cfg.read().unwrap().clone();
        if new == old {
            return;
        }
        if new.slice.size != old.slice.size {
            if let Err(e) = self.client.set_pledge(self.node.id, new.slice.size).await {
                self.log("error", format!("couldn't change the slice size: {e:#}"));
                return;
            }
        }
        *self.cfg.write().unwrap() = new;
        self.log("info", "settings reloaded from config.toml");
    }

    pub fn update_config(&self, f: impl FnOnce(&mut Config)) -> Result<()> {
        let mut cfg = self.cfg.write().unwrap();
        f(&mut cfg);
        self.paths.save_config(&cfg)?;
        let n = cfg.backup.paths.len();
        drop(cfg);
        let next = self.next_backup();
        self.status.lock().unwrap().next_backup = next;
        self.log("info", format!("settings saved ({n} folders)"));
        Ok(())
    }
}

pub async fn run(paths: Paths) -> Result<()> {
    let (cfg, creds) = paths.load()?;
    let net = if cfg.local { NetMode::Local } else { NetMode::Internet };
    let client = Client::open(&cfg.control_plane, &creds.phrase, &creds.passphrase, &paths.state_dir(), net).await?;
    std::fs::create_dir_all(&cfg.slice.dir)?;
    let node = globalobject_node::start(NodeConfig {
        control_plane: cfg.control_plane.clone(),
        owner: AccountAuth { id: client.id(), seed: client.signing().seed() },
        store: cfg.slice.dir.clone(),
        pledged: cfg.slice.size,
        real_capacity: cfg.slice.size,
        // The control plane works out the site from this machine's address.
        site: String::new(),
        asn: String::new(),
        link_up_bps: cfg.slice.upload_limit,
        heartbeat: Duration::from_secs(15),
        net,
        probe_bytes: 4 << 20,
        gc_every: Duration::from_secs(600),
    })
    .await
    .context("starting the storage slice")?;
    std::fs::write(paths.state_dir().join("node"), node.id.to_string())?;
    // The size may have changed in config.toml since last time.
    if let Err(e) = client.set_pledge(node.id, cfg.slice.size).await {
        tracing::warn!("couldn't update the slice size: {e:#}");
    }

    let last_backup = read_backup(&paths);
    let port = cfg.dashboard.port;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.with_context(|| format!("dashboard port {port} is in use"))?;
    let dashboard_url = format!("http://{}", listener.local_addr()?);
    let agent = Arc::new(Agent {
        status: Mutex::new(AgentStatus {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            started: now(),
            node: Some(node.id),
            last_backup,
            dashboard: dashboard_url.clone(),
            ..Default::default()
        }),
        paths,
        cfg: RwLock::new(cfg),
        creds,
        client,
        node,
        events: Mutex::new(VecDeque::new()),
        job: tokio::sync::Mutex::new(()),
    });
    agent.log("info", format!("started: node {} sharing {}", agent.node.id, crate::config::format_size(agent.status().slice_size)));

    let dash = tokio::spawn(dashboard::serve(agent.clone(), listener));
    agent.log("info", format!("dashboard on {dashboard_url} (open it with `globalobject dashboard`)"));

    let sched = {
        let agent = agent.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            loop {
                tick.tick().await;
                agent.reload_if_changed().await;
                // A backup run with `globalobject backup` counts for the schedule too.
                if let Some(r) = read_backup(&agent.paths) {
                    let mut st = agent.status.lock().unwrap();
                    if st.last_backup.as_ref().is_none_or(|old| r.started > old.started) {
                        st.last_backup = Some(r);
                    }
                }
                let next = agent.next_backup();
                agent.status.lock().unwrap().next_backup = next;
                agent.write_status();
                // In its own task, so this loop keeps the status file fresh during long backups.
                if next.is_some_and(|t| t <= now()) && agent.status.lock().unwrap().running.is_none() {
                    let a = agent.clone();
                    tokio::spawn(async move { a.backup_now().await });
                }
            }
        })
    };

    shutdown_signal().await;
    agent.log("info", "stopping");
    sched.abort();
    dash.abort();
    agent.client.shutdown().await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

/// Record a backup's outcome where `status`, the dashboard and the agent's schedule see it,
/// whichever process ran it.
pub fn record_backup(paths: &Paths, rec: &JobRecord) {
    let _ = std::fs::write(paths.state_dir().join("last-backup.json"), serde_json::to_vec_pretty(rec).unwrap_or_default());
}

pub fn read_backup(paths: &Paths) -> Option<JobRecord> {
    serde_json::from_slice(&std::fs::read(paths.state_dir().join("last-backup.json")).ok()?).ok()
}

/// The agent's last status file, if it has written one recently.
pub fn read_status(paths: &Paths) -> Option<AgentStatus> {
    let s: AgentStatus = serde_json::from_slice(&std::fs::read(paths.state_dir().join("agent.json")).ok()?).ok()?;
    Some(s)
}
