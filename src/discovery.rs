use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

const OMP_WORKERS: &[&str] = &[
    "__omp_worker_blob_broker",
    "__omp_worker_computer",
    "__omp_worker_daemon_broker",
    "__omp_worker_ida_host",
    "__omp_worker_lsp_mux",
    "__omp_worker_parent_watchdog",
    "__omp_worker_stats_activity",
    "__omp_worker_terminal_output",
    "__omp_worker_tiny_inference",
    "__omp_worker_text_predict",
    "__omp_worker_stats_sync",
    "__omp_worker_tab",
    "__omp_worker_js_eval",
    "__omp_worker_js_eval_process",
    "__omp_worker_stt",
    "__omp_worker_tts",
    "__omp_worker_mnemopi_embed",
];

const OMP_SCRIPT_SUFFIX: &[&str] = &["@oh-my-pi", "pi-coding-agent", "dist", "cli.js"];
const CODEX_SCRIPT_SUFFIX: &[&str] = &["@openai", "codex", "bin", "codex.js"];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Product {
    Omp,
    Codex,
    OpenCode,
}

impl std::fmt::Display for Product {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Omp => "omp",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AgentKey {
    pub(crate) product: Product,
    pub(crate) root_pid: u32,
    pub(crate) start_time: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DiscoveredAgent {
    pub(crate) key: AgentKey,
    pub(crate) pids: Vec<u32>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) state: String,
}

pub(crate) struct Discovery {
    system: System,
    entrypoints: Entrypoints,
    canonical_paths: HashMap<PathBuf, Option<PathBuf>>,
    parents: HashMap<u32, u32>,
    roots: Vec<Pid>,
}

impl Discovery {
    pub(crate) fn new() -> Self {
        let mut canonical_paths = HashMap::new();
        let entrypoints = Entrypoints::from_path(&mut canonical_paths);
        Self {
            system: System::new(),
            entrypoints,
            canonical_paths,
            parents: HashMap::new(),
            roots: Vec::new(),
        }
    }

    pub(crate) fn scan(&mut self) -> Vec<DiscoveredAgent> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::Always)
                .without_tasks(),
        );

        let processes: Vec<_> = self
            .system
            .processes()
            .values()
            .map(ProcessMetadata::from_process)
            .collect();
        self.parents.clear();
        self.parents.extend(
            processes
                .iter()
                .filter_map(|process| process.parent.map(|parent| (process.pid, parent))),
        );

        let mut agents =
            identify_processes(&processes, &self.entrypoints, &mut self.canonical_paths);
        self.roots.clear();
        self.roots
            .extend(agents.iter().map(|agent| Pid::from_u32(agent.key.root_pid)));
        drop(processes);
        if self.roots.is_empty() {
            return agents;
        }

        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&self.roots),
            false,
            ProcessRefreshKind::nothing()
                .with_cwd(UpdateKind::Always)
                .without_tasks(),
        );

        agents.retain_mut(|agent| {
            let Some(process) = self.system.process(Pid::from_u32(agent.key.root_pid)) else {
                return false;
            };
            let refreshed = ProcessMetadata::from_process(process);
            if !process.exists()
                || is_dead(refreshed.status)
                || refreshed.start_time != agent.key.start_time
                || classify(&refreshed, &self.entrypoints, &mut self.canonical_paths)
                    .map(|candidate| candidate.product)
                    != Some(agent.key.product)
            {
                return false;
            }
            agent.cwd = process.cwd().map(Path::to_path_buf);
            true
        });

        agents
    }

    pub(crate) fn parents(&self) -> &HashMap<u32, u32> {
        &self.parents
    }
}

#[derive(Default)]
struct Entrypoints {
    omp: Option<PathBuf>,
    codex: Option<PathBuf>,
    opencode: Option<PathBuf>,
}

impl Entrypoints {
    fn from_path(cache: &mut HashMap<PathBuf, Option<PathBuf>>) -> Self {
        Self {
            omp: find_path_entry("omp", cache),
            codex: find_path_entry("codex", cache),
            opencode: find_path_entry("opencode", cache),
        }
    }

    fn for_product(&self, product: Product) -> Option<&PathBuf> {
        match product {
            Product::Omp => self.omp.as_ref(),
            Product::Codex => self.codex.as_ref(),
            Product::OpenCode => self.opencode.as_ref(),
        }
    }

    fn product_for_path(&self, path: &Path) -> Option<Product> {
        [Product::Omp, Product::Codex, Product::OpenCode]
            .into_iter()
            .find(|product| {
                self.for_product(*product)
                    .is_some_and(|entry| entry == path)
            })
    }
}

struct ProcessMetadata<'a> {
    pid: u32,
    parent: Option<u32>,
    exe: Option<&'a Path>,
    argv: &'a [OsString],
    start_time: u64,
    status: ProcessStatus,
}

impl<'a> ProcessMetadata<'a> {
    fn from_process(process: &'a sysinfo::Process) -> Self {
        Self {
            pid: process.pid().as_u32(),
            parent: process.parent().map(|parent| parent.as_u32()),
            exe: process.exe(),
            argv: process.cmd(),
            start_time: process.start_time(),
            status: process.status(),
        }
    }
}

fn is_dead(status: ProcessStatus) -> bool {
    matches!(status, ProcessStatus::Dead | ProcessStatus::Zombie)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CandidateKind {
    Native,
    Runtime,
}

struct Candidate<'a> {
    product: Product,
    application_args: &'a [OsString],
    kind: CandidateKind,
}

struct Identified<'a> {
    key: AgentKey,
    pids: Vec<u32>,
    state: String,
    application_args: &'a [OsString],
    kind: CandidateKind,
    parent: Option<u32>,
}

fn identify_processes<'a>(
    processes: &[ProcessMetadata<'a>],
    entrypoints: &Entrypoints,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Vec<DiscoveredAgent> {
    let mut identified: Vec<Identified<'a>> = processes
        .iter()
        .filter(|process| !is_dead(process.status))
        .filter_map(|process| {
            classify(process, entrypoints, cache).map(|candidate| Identified {
                key: AgentKey {
                    product: candidate.product,
                    root_pid: process.pid,
                    start_time: process.start_time,
                },
                pids: vec![process.pid],
                state: format!("{:?}", process.status),
                application_args: candidate.application_args,
                kind: candidate.kind,
                parent: process.parent,
            })
        })
        .collect();
    identified.sort_by_key(|agent| agent.key.root_pid);

    let mut absorbed = vec![false; identified.len()];
    for launcher_index in 0..identified.len() {
        let (launcher_pid, application_args) = {
            let launcher = &identified[launcher_index];
            if launcher.key.product != Product::Codex || launcher.kind != CandidateKind::Runtime {
                continue;
            }
            (launcher.key.root_pid, launcher.application_args)
        };
        if let Some(child_index) = identified.iter().enumerate().find_map(|(index, child)| {
            (index != launcher_index
                && !absorbed[index]
                && child.key.product == Product::Codex
                && child.kind == CandidateKind::Native
                && child.parent == Some(launcher_pid)
                && child.application_args == application_args)
                .then_some(index)
        }) {
            let child_pid = identified[child_index].key.root_pid;
            identified[launcher_index].pids.push(child_pid);
            absorbed[child_index] = true;
        }
    }

    identified
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !absorbed[*index])
        .map(|(_, agent)| DiscoveredAgent {
            key: agent.key,
            pids: agent.pids,
            cwd: None,
            state: agent.state,
        })
        .collect()
}

fn classify<'a>(
    process: &ProcessMetadata<'a>,
    entrypoints: &Entrypoints,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<Candidate<'a>> {
    let executable = process.exe;
    if let Some(executable) = executable {
        if let Some(product) = native_product(executable, entrypoints, cache) {
            return candidate(
                product,
                CandidateKind::Native,
                process.argv.get(1..).unwrap_or_default(),
            );
        }
        if is_runtime(executable) {
            return runtime_candidate(process.argv, false, entrypoints, cache);
        }
        return None;
    }

    let argv0 = Path::new(process.argv.first()?);
    if !argv0.is_absolute() {
        return None;
    }
    if let Some(product) =
        canonical_path(argv0, cache).and_then(|path| entrypoints.product_for_path(path))
    {
        return candidate(
            product,
            CandidateKind::Native,
            process.argv.get(1..).unwrap_or_default(),
        );
    }
    if is_runtime(argv0) {
        return runtime_candidate(process.argv, true, entrypoints, cache);
    }
    None
}

fn native_product(
    executable: &Path,
    entrypoints: &Entrypoints,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<Product> {
    product_from_name(executable.file_name()?).or_else(|| {
        canonical_path(executable, cache).and_then(|path| entrypoints.product_for_path(path))
    })
}

fn runtime_candidate<'a>(
    argv: &'a [OsString],
    require_absolute_script: bool,
    entrypoints: &Entrypoints,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<Candidate<'a>> {
    let mut script_index = 1;
    if argv
        .get(script_index)
        .is_some_and(|argument| argument == "--")
    {
        script_index += 1;
    }
    let script = Path::new(argv.get(script_index)?);
    if require_absolute_script && !script.is_absolute() {
        return None;
    }
    let product = script_product(script, entrypoints, cache)?;
    candidate(
        product,
        CandidateKind::Runtime,
        argv.get(script_index + 1..).unwrap_or_default(),
    )
}

fn candidate<'a>(
    product: Product,
    kind: CandidateKind,
    application_args: &'a [OsString],
) -> Option<Candidate<'a>> {
    let first = application_args.first().and_then(|value| value.to_str());
    let excluded = match product {
        Product::Omp => first.is_some_and(|argument| OMP_WORKERS.contains(&argument)),
        Product::Codex => first.is_some_and(|argument| {
            matches!(
                argument,
                "exec-server"
                    | "app-server"
                    | "responses-api-proxy"
                    | "stdio-to-uds"
                    | "tcp-tunnel"
            )
        }),
        Product::OpenCode => first.is_some_and(|argument| matches!(argument, "serve" | "acp")),
    };
    (!excluded).then(|| Candidate {
        product,
        application_args,
        kind,
    })
}

fn script_product(
    script: &Path,
    entrypoints: &Entrypoints,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<Product> {
    if script.file_name().is_some_and(|name| name == "omp") {
        return Some(Product::Omp);
    }
    canonical_path(script, cache)
        .and_then(|path| entrypoints.product_for_path(path))
        .or_else(|| has_path_suffix(script, OMP_SCRIPT_SUFFIX).then_some(Product::Omp))
        .or_else(|| has_path_suffix(script, CODEX_SCRIPT_SUFFIX).then_some(Product::Codex))
}

fn product_from_name(name: &OsStr) -> Option<Product> {
    match name.to_str()? {
        "omp" => Some(Product::Omp),
        "codex" => Some(Product::Codex),
        "opencode" => Some(Product::OpenCode),
        _ => None,
    }
}

fn is_runtime(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(OsStr::to_str),
        Some("bun" | "node")
    )
}

fn has_path_suffix(path: &Path, suffix: &[&str]) -> bool {
    path.components()
        .rev()
        .zip(suffix.iter().rev())
        .all(|(component, expected)| component.as_os_str() == *expected)
        && path.components().count() >= suffix.len()
}

fn find_path_entry(name: &str, cache: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(name);
        canonical_path(&candidate, cache).cloned()
    })
}

fn canonical_path<'a>(
    path: &Path,
    cache: &'a mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<&'a PathBuf> {
    if !cache.contains_key(path) {
        cache.insert(path.to_path_buf(), path.canonicalize().ok());
    }
    cache.get(path).and_then(Option::as_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Entrypoints {
        Entrypoints {
            omp: Some(PathBuf::from("/bin/omp-entry")),
            codex: Some(PathBuf::from("/bin/codex-entry")),
            opencode: Some(PathBuf::from("/bin/opencode-entry")),
        }
    }

    #[derive(Clone)]
    struct RawProcess {
        pid: u32,
        parent: Option<u32>,
        exe: Option<PathBuf>,
        argv: Vec<OsString>,
        start_time: u64,
        status: ProcessStatus,
    }

    impl RawProcess {
        fn metadata(&self) -> ProcessMetadata<'_> {
            ProcessMetadata {
                pid: self.pid,
                parent: self.parent,
                exe: self.exe.as_deref(),
                argv: &self.argv,
                start_time: self.start_time,
                status: self.status,
            }
        }
    }

    fn process(pid: u32, parent: Option<u32>, exe: Option<&str>, argv: &[&str]) -> RawProcess {
        RawProcess {
            pid,
            parent,
            exe: exe.map(PathBuf::from),
            argv: argv.iter().map(OsString::from).collect(),
            start_time: pid as u64 + 100,
            status: ProcessStatus::Run,
        }
    }

    fn identify(processes: &[RawProcess]) -> Vec<DiscoveredAgent> {
        let metadata: Vec<_> = processes.iter().map(RawProcess::metadata).collect();
        identify_processes(&metadata, &paths(), &mut HashMap::new())
    }

    #[test]
    fn omp_bun_root_and_worker_are_not_double_counted() {
        let agents = identify(&[
            process(
                10,
                Some(1),
                Some("/opt/homebrew/bin/bun"),
                &["bun", "/any/@oh-my-pi/pi-coding-agent/dist/cli.js", "task"],
            ),
            process(
                11,
                Some(10),
                Some("/opt/homebrew/bin/bun"),
                &[
                    "bun",
                    "/any/@oh-my-pi/pi-coding-agent/dist/cli.js",
                    "__omp_worker_daemon_broker",
                ],
            ),
        ]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].key.product, Product::Omp);
        assert_eq!(agents[0].key.root_pid, 10);
    }

    #[test]
    fn independent_roots_with_same_cwd_remain_distinct() {
        let agents = identify(&[
            process(10, None, Some("/bin/omp"), &["omp", "task"]),
            process(11, None, Some("/bin/omp"), &["omp", "task"]),
        ]);
        assert_eq!(agents.len(), 2);
        assert_ne!(agents[0].key, agents[1].key);
    }

    #[test]
    fn stopped_agents_remain_visible_but_dead_agents_do_not() {
        let mut stopped = process(10, None, Some("/bin/omp"), &["omp"]);
        stopped.status = ProcessStatus::Stop;
        let mut dead = process(11, None, Some("/bin/codex"), &["codex", "exec"]);
        dead.status = ProcessStatus::Zombie;
        let agents = identify(&[stopped, dead]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].key.product, Product::Omp);
        assert_eq!(agents[0].state, "Stop");
    }

    #[test]
    fn product_words_and_similar_binaries_do_not_match() {
        let agents = identify(&[
            process(
                10,
                None,
                Some("/usr/bin/node"),
                &["node", "-e", "console.log('codex')"],
            ),
            process(
                11,
                None,
                Some("/bin/codex-helper"),
                &["codex-helper", "omp"],
            ),
            process(
                12,
                None,
                Some("/usr/bin/node"),
                &["node", "/work/codex-prompt.js", "codex"],
            ),
        ]);
        assert!(agents.is_empty());
    }

    #[test]
    fn codex_launcher_and_direct_native_child_coalesce_only_with_same_arguments() {
        let agents = identify(&[
            process(
                10,
                None,
                Some("/usr/bin/node"),
                &["node", "/any/@openai/codex/bin/codex.js", "exec", "hello"],
            ),
            process(
                11,
                Some(10),
                Some("/bin/codex"),
                &["codex", "exec", "hello"],
            ),
            process(
                12,
                Some(10),
                Some("/bin/codex"),
                &["codex", "exec", "other"],
            ),
        ]);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].key.root_pid, 10);
        assert_eq!(agents[0].pids, vec![10, 11]);
        assert_eq!(agents[1].key.root_pid, 12);
    }

    #[test]
    fn infrastructure_exclusions_are_exact_first_application_argument() {
        let agents = identify(&[
            process(1, None, Some("/bin/codex"), &["codex", "exec"]),
            process(2, None, Some("/bin/codex"), &["codex", "exec-server"]),
            process(3, None, Some("/bin/opencode"), &["opencode", "serve"]),
            process(4, None, Some("/bin/opencode"), &["opencode", "serve now"]),
            process(5, None, Some("/bin/omp"), &["omp", "prompt", "exec-server"]),
            process(
                6,
                None,
                Some("/bin/omp"),
                &["omp", "__omp_worker_daemon_broker"],
            ),
            process(7, None, Some("/bin/opencode"), &["opencode", "task"]),
        ]);
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.key.root_pid)
                .collect::<Vec<_>>(),
            vec![1, 4, 5, 7]
        );
    }

    #[test]
    fn inaccessible_metadata_requires_absolute_strict_paths() {
        let mut entrypoints = paths();
        entrypoints.codex = None;
        let mut cache = HashMap::new();
        let relative = process(
            1,
            None,
            None,
            &["node", "/any/@openai/codex/bin/codex.js", "exec"],
        );
        let absolute_non_script = process(2, None, None, &["/usr/bin/node", "codex", "exec"]);
        let absolute_script = process(
            3,
            None,
            None,
            &["/usr/bin/node", "/any/@openai/codex/bin/codex.js", "exec"],
        );
        assert!(classify(&relative.metadata(), &entrypoints, &mut cache).is_none());
        assert!(classify(&absolute_non_script.metadata(), &entrypoints, &mut cache).is_none());
        assert_eq!(
            classify(&absolute_script.metadata(), &entrypoints, &mut cache)
                .unwrap()
                .product,
            Product::Codex
        );
    }

    #[test]
    fn inaccessible_metadata_accepts_only_an_absolute_canonical_entrypoint() {
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let entrypoints = Entrypoints {
            codex: Some(executable.clone()),
            ..Entrypoints::default()
        };
        let matching = RawProcess {
            pid: 1,
            parent: None,
            exe: None,
            argv: vec![executable.into_os_string(), OsString::from("exec")],
            start_time: 1,
            status: ProcessStatus::Run,
        };
        let non_matching = RawProcess {
            argv: vec![OsString::from("codex"), OsString::from("exec")],
            ..matching.clone()
        };
        let mut cache = HashMap::new();
        assert_eq!(
            classify(&matching.metadata(), &entrypoints, &mut cache)
                .unwrap()
                .product,
            Product::Codex
        );
        assert!(classify(&non_matching.metadata(), &entrypoints, &mut cache).is_none());
    }

    #[test]
    fn runtime_allows_only_the_optional_double_dash_before_its_script() {
        let process = process(
            1,
            None,
            Some("/usr/bin/bun"),
            &[
                "bun",
                "--",
                "/any/@oh-my-pi/pi-coding-agent/dist/cli.js",
                "task",
            ],
        );
        assert_eq!(
            classify(&process.metadata(), &paths(), &mut HashMap::new())
                .unwrap()
                .product,
            Product::Omp
        );
    }

    #[test]
    fn script_suffixes_match_path_components_not_substrings() {
        let accepted = process(
            1,
            None,
            Some("/usr/bin/bun"),
            &["bun", "/x/@oh-my-pi/pi-coding-agent/dist/cli.js", "task"],
        );
        let rejected = process(
            2,
            None,
            Some("/usr/bin/bun"),
            &[
                "bun",
                "/x/not-@oh-my-pi/pi-coding-agent/dist/cli.js",
                "task",
            ],
        );
        let mut cache = HashMap::new();
        assert_eq!(
            classify(&accepted.metadata(), &paths(), &mut cache)
                .unwrap()
                .product,
            Product::Omp
        );
        assert!(classify(&rejected.metadata(), &paths(), &mut cache).is_none());
    }
}
