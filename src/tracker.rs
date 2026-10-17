use crate::{
    attention,
    discovery::{AgentKey, Discovery},
    notification::Notifier,
    terminal,
    tmux::{self, Client, Pane, Tmux},
};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    Running,
    OutputActive,
    Quiet,
}
impl Activity {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::OutputActive => "output active",
            Self::Quiet => "quiet",
        }
    }
}
pub(crate) fn activity(timestamp: Option<u64>, now: u64) -> Activity {
    match timestamp {
        Some(0) => Activity::Quiet,
        Some(time) if now.saturating_sub(time) <= 3 => Activity::OutputActive,
        Some(_) => Activity::Quiet,
        None => Activity::Running,
    }
}
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub key: AgentKey,
    pub cwd: Option<PathBuf>,
    pub state: String,
    pub pane: Option<Pane>,
    pub activity: Activity,
    pub attention: Option<&'static str>,
}
impl Row {
    pub(crate) fn location(&self) -> String {
        self.pane
            .as_ref()
            .map(|p| {
                format!(
                    "{}:{}.{}, {}",
                    p.session_name, p.window_index, p.pane_index, p.pane_id
                )
            })
            .unwrap_or_else(|| "outside tmux".into())
    }
    pub(crate) fn project(&self) -> String {
        self.cwd
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                self.cwd
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "unavailable".into())
            })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Preview {
    NoPane,
    Text(Arc<String>),
    Error(String),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub rows: Vec<Row>,
    pub error: Option<String>,
    pub notification_error: Option<String>,
    pub preview: Option<(AgentKey, Preview)>,
}
#[derive(Clone, Debug)]
pub(crate) struct Navigation {
    pub key: AgentKey,
    pub pane: Option<Pane>,
    pub client: Option<String>,
}
#[derive(Clone, Debug)]
pub(crate) enum NavResult {
    Ready {
        target: String,
        client: Option<String>,
    },
    Choose(Vec<Client>),
    Focused,
    Error(String),
}
#[derive(Default)]
struct Exchange {
    stop: bool,
    refresh: bool,
    selection_changed: bool,
    selected: Option<AgentKey>,
    navigation: Option<Navigation>,
    result: Option<NavResult>,
    latest: Option<Arc<Snapshot>>,
}
pub(crate) struct Tracker {
    shared: Arc<(Mutex<Exchange>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
impl Tracker {
    pub(crate) fn new() -> Self {
        let shared = Arc::new((Mutex::new(Exchange::default()), Condvar::new()));
        let worker_shared = shared.clone();
        let worker = thread::spawn(move || collect(worker_shared));
        Self {
            shared,
            worker: Some(worker),
        }
    }
    pub(crate) fn take(&self) -> (Option<Arc<Snapshot>>, Option<NavResult>) {
        let mut x = self.shared.0.lock();
        (x.latest.take(), x.result.take())
    }
    pub(crate) fn select(&self, key: Option<AgentKey>) {
        let mut x = self.shared.0.lock();
        if x.selected != key {
            x.selected = key;
            x.selection_changed = true;
            self.shared.1.notify_one();
        }
    }
    pub(crate) fn refresh(&self) {
        let mut x = self.shared.0.lock();
        x.refresh = true;
        self.shared.1.notify_one();
    }
    pub(crate) fn navigate(&self, request: Navigation) {
        let mut x = self.shared.0.lock();
        x.navigation = Some(request);
        self.shared.1.notify_one();
    }
}
impl Drop for Tracker {
    fn drop(&mut self) {
        {
            let mut x = self.shared.0.lock();
            x.stop = true;
            self.shared.1.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[derive(PartialEq, Eq)]
struct CaptureKey {
    pane_pid: u32,
    timestamp: Option<u64>,
    width: u32,
    height: u32,
}

/// One alert per observed waiting episode; read failures do not rearm it.
#[derive(Default)]
struct AttentionState {
    pane: Option<(String, u32)>,
    waiting: bool,
}
impl AttentionState {
    fn observe(&mut self, pane: &Pane, waiting: bool) -> bool {
        let same_pane = self
            .pane
            .as_ref()
            .is_some_and(|(id, pid)| id == &pane.pane_id && *pid == pane.pane_pid);
        let alert = waiting && (!same_pane || !self.waiting);
        if !same_pane {
            self.pane = Some((pane.pane_id.clone(), pane.pane_pid));
        }
        self.waiting = waiting;
        alert
    }
}
fn collect(shared: Arc<(Mutex<Exchange>, Condvar)>) {
    let mut discovery = Discovery::new();
    let tmux = Tmux::new();
    let origin = std::env::var("TMUX").ok();
    let session = origin.as_deref().and_then(tmux::origin_session);
    let origin_pane = std::env::var("TMUX_PANE").ok();
    let mut snapshot = Snapshot::default();
    let mut cache: HashMap<String, (CaptureKey, Preview)> = HashMap::new();
    let mut screens: HashMap<String, (CaptureKey, Preview)> = HashMap::new();
    let mut attention_states: HashMap<AgentKey, AttentionState> = HashMap::new();
    let mut notifier = Notifier::new();
    let mut next_scan = Instant::now();
    loop {
        let (scan, refresh, selected, navigation) = {
            let mut x = shared.0.lock();
            while !x.stop
                && !x.refresh
                && !x.selection_changed
                && x.navigation.is_none()
                && Instant::now() < next_scan
            {
                let duration = next_scan.saturating_duration_since(Instant::now());
                shared.1.wait_for(&mut x, duration);
            }
            if x.stop {
                break;
            }
            let refresh = std::mem::take(&mut x.refresh);
            x.selection_changed = false;
            let navigation = x.navigation.take();
            (
                refresh || navigation.is_some() || Instant::now() >= next_scan,
                refresh,
                x.selected,
                navigation,
            )
        };
        if scan {
            let agents = discovery.scan();
            let (panes, error) = match tmux.inventory() {
                Ok(p) => (p, None),
                Err(e) => (Vec::new(), Some(e.to_string())),
            };
            let wall_time = now();
            snapshot.rows.clear();
            snapshot.rows.extend(agents.into_iter().map(|a| {
                let pane =
                    tmux::map_agent(&a.pids, discovery.parents(), &panes, session.as_deref());
                let activity = activity(pane.as_ref().and_then(|p| p.last_output), wall_time);
                let cwd = a.cwd.or_else(|| {
                    pane.as_ref()
                        .filter(|p| !p.path.as_os_str().is_empty())
                        .map(|p| p.path.clone())
                });
                Row {
                    key: a.key,
                    cwd,
                    state: a.state,
                    pane,
                    activity,
                    attention: None,
                }
            }));
            snapshot
                .rows
                .sort_by_cached_key(|r| (r.key.product, r.project(), r.location(), r.key.root_pid));
            snapshot.error = error;
            cache.retain(|id, _| panes.iter().any(|p| &p.pane_id == id && !p.dead));
            screens.retain(|id, _| panes.iter().any(|p| &p.pane_id == id && !p.dead));
            attention_states.retain(|key, _| snapshot.rows.iter().any(|r| &r.key == key));
            let mut alerted_panes = std::collections::HashSet::new();
            for row in &mut snapshot.rows {
                let Some(pane) = &row.pane else {
                    if snapshot.error.is_none() {
                        attention_states.remove(&row.key);
                    }
                    continue;
                };
                let capture_key = CaptureKey {
                    pane_pid: pane.pane_pid,
                    timestamp: pane.last_output,
                    width: pane.width,
                    height: pane.height,
                };
                if refresh
                    || pane.last_output.is_none()
                    || screens.get(&pane.pane_id).is_none_or(|(old, preview)| {
                        *old != capture_key || matches!(preview, Preview::Error(_))
                    })
                {
                    let screen = match tmux.screen(pane) {
                        Ok(text) => Preview::Text(Arc::new(text)),
                        Err(error) => Preview::Error(error.to_string()),
                    };
                    screens.insert(pane.pane_id.clone(), (capture_key, screen));
                }
                match &screens[&pane.pane_id].1 {
                    Preview::Text(screen) => {
                        row.attention = attention::detect(row.key.product, screen);
                        let alert = attention_states
                            .entry(row.key)
                            .or_default()
                            .observe(pane, row.attention.is_some());
                        if alert && alerted_panes.insert(&pane.pane_id) {
                            let title = format!("lat: {} needs input", row.key.product);
                            let body = format!(
                                "{} · {}\n{} (pane-level detection)",
                                row.project(),
                                row.location(),
                                row.attention.unwrap()
                            );
                            snapshot.notification_error = notifier.send(&title, &body).err();
                        }
                    }
                    Preview::Error(error) => {
                        snapshot.error.get_or_insert_with(|| {
                            format!("Could not inspect agent input prompts: {error}")
                        });
                    }
                    Preview::NoPane => unreachable!(),
                }
            }
            next_scan = Instant::now() + Duration::from_secs(1);
        }
        snapshot.preview = selected.map(|key| {
            let preview = match snapshot
                .rows
                .iter()
                .find(|r| r.key == key)
                .and_then(|r| r.pane.as_ref())
            {
                None => Preview::NoPane,
                Some(pane) => {
                    let capture_key = CaptureKey {
                        pane_pid: pane.pane_pid,
                        timestamp: pane.last_output,
                        width: pane.width,
                        height: pane.height,
                    };
                    let needs_capture = refresh
                        || (scan && pane.last_output.is_none())
                        || cache
                            .get(&pane.pane_id)
                            .is_none_or(|(old, _)| *old != capture_key);
                    if needs_capture {
                        let preview = match tmux.capture(pane) {
                            Ok(text) => Preview::Text(Arc::new(text)),
                            Err(e) => Preview::Error(e.to_string()),
                        };
                        cache.insert(pane.pane_id.clone(), (capture_key, preview));
                    }
                    cache[&pane.pane_id].1.clone()
                }
            };
            (key, preview)
        });
        let result = navigation.map(|request| {
            validate_navigation(
                &tmux,
                &snapshot,
                &request,
                origin.is_some(),
                session.as_deref(),
                origin_pane.as_deref(),
            )
        });
        let mut x = shared.0.lock();
        x.latest = Some(Arc::new(snapshot.clone()));
        if let Some(result) = result {
            x.result = Some(result);
        }
    }
}
fn valid_target(snapshot: &Snapshot, request: &Navigation) -> bool {
    snapshot.rows.iter().any(|row| {
        row.key == request.key
            && match (row.pane.as_ref(), request.pane.as_ref()) {
                (Some(pane), Some(expected)) => {
                    pane.pane_pid == expected.pane_pid && pane.target() == expected.target()
                }
                (None, None) => true,
                _ => false,
            }
    })
}
fn validate_navigation(
    tmux: &Tmux,
    snapshot: &Snapshot,
    request: &Navigation,
    inside: bool,
    session: Option<&str>,
    pane: Option<&str>,
) -> NavResult {
    if !valid_target(snapshot, request) {
        return NavResult::Error("Agent or tmux pane is no longer available".into());
    }
    let Some(target_pane) = &request.pane else {
        return match terminal::focus(request.key) {
            Ok(()) => NavResult::Focused,
            Err(error) => NavResult::Error(error),
        };
    };
    if !inside {
        return NavResult::Ready {
            target: target_pane.target(),
            client: None,
        };
    }
    let (Some(session), Some(pane)) = (session, pane) else {
        return NavResult::Error("No attached client is viewing the tracker pane".into());
    };
    let clients = match tmux.clients() {
        Ok(clients) => clients
            .into_iter()
            .filter(|c| c.session_id == session && c.pane_id == pane)
            .collect::<Vec<_>>(),
        Err(e) => return NavResult::Error(e.to_string()),
    };
    if let Some(chosen) = &request.client {
        return if clients.iter().any(|c| &c.name == chosen) {
            NavResult::Ready {
                target: target_pane.target(),
                client: Some(chosen.clone()),
            }
        } else {
            NavResult::Error("No attached client is viewing the tracker pane".into())
        };
    }
    match clients.as_slice() {
        [] => NavResult::Error("No attached client is viewing the tracker pane".into()),
        [client] => NavResult::Ready {
            target: target_pane.target(),
            client: Some(client.name.clone()),
        },
        _ => NavResult::Choose(clients),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pane() -> Pane {
        Pane {
            session_id: "$1".into(),
            window_id: "@2".into(),
            pane_id: "%3".into(),
            pane_pid: 41,
            window_index: 0,
            pane_index: 0,
            height: 24,
            width: 80,
            dead: false,
            last_output: Some(5),
            session_name: "s".into(),
            window_name: "w".into(),
            path: "/work".into(),
        }
    }

    #[test]
    fn alerts_once_per_wait_and_rearms_after_input_or_pane_replacement() {
        let mut state = AttentionState::default();
        let mut pane = pane();
        assert!(!state.observe(&pane, false));
        assert!(state.observe(&pane, true));
        pane.last_output = Some(100);
        pane.width = 120;
        assert!(!state.observe(&pane, true));
        assert!(!state.observe(&pane, false));
        assert!(state.observe(&pane, true));
        pane.pane_pid += 1;
        assert!(state.observe(&pane, true));
        assert!(!state.observe(&pane, true));
        pane.pane_id = "%99".into();
        assert!(state.observe(&pane, true));
    }

    #[test]
    fn pane_activity_requires_evidence() {
        assert_eq!(activity(Some(98), 100), Activity::OutputActive);
        assert_eq!(activity(Some(96), 100), Activity::Quiet);
        assert_eq!(activity(Some(0), 100), Activity::Quiet);
        assert_eq!(activity(None, 100), Activity::Running);
        assert_eq!(activity(Some(110), 100), Activity::OutputActive);
    }
    #[test]
    fn jump_rejects_disappearance_reused_pid_and_replaced_pane() {
        let key = AgentKey {
            product: crate::discovery::Product::Codex,
            root_pid: 42,
            start_time: 10,
        };
        let pane = pane();
        let mut request = Navigation {
            key,
            pane: Some(pane.clone()),
            client: None,
        };
        let mut snapshot = Snapshot {
            rows: vec![Row {
                key,
                cwd: None,
                state: "Run".into(),
                pane: Some(pane),
                activity: Activity::Quiet,
                attention: None,
            }],
            ..Snapshot::default()
        };
        assert!(valid_target(&snapshot, &request));
        snapshot.rows[0].key.start_time = 11;
        assert!(!valid_target(&snapshot, &request));
        snapshot.rows[0].key = key;
        snapshot.rows[0].pane.as_mut().unwrap().pane_pid = 100;
        assert!(!valid_target(&snapshot, &request));
        snapshot.rows[0].pane.as_mut().unwrap().pane_pid = 41;
        snapshot.rows[0].pane.as_mut().unwrap().pane_id = "%4".into();
        assert!(!valid_target(&snapshot, &request));
        let original_pane = snapshot.rows[0].pane.take();
        assert!(!valid_target(&snapshot, &request));
        request.pane = None;
        assert!(valid_target(&snapshot, &request));
        snapshot.rows[0].key.start_time += 1;
        assert!(!valid_target(&snapshot, &request));
        snapshot.rows[0].key = key;
        snapshot.rows[0].pane = original_pane;
        assert!(!valid_target(&snapshot, &request));
        snapshot.rows.clear();
        assert!(!valid_target(&snapshot, &request));
    }
}
