//! Pod monitoring and lifecycle controls through Runpod's REST API (v2). The Pod list leaves out usage,
//! so each refresh is one list request plus one request per running Pod. No on-Pod agent.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::remote::{Job, Snapshot};

const API: &str = "https://api.runpod.io/v2";
/// Each page holds up to 1000 Pods, so this is only a guard against a looping cursor.
const MAX_PAGES: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Start, Stop, Restart, Terminate }

impl Action {
    pub fn verb(self) -> &'static str {
        match self { Self::Start => "start", Self::Stop => "stop", Self::Restart => "restart", Self::Terminate => "terminate" }
    }
    pub fn title(self) -> &'static str {
        match self { Self::Start => "Start", Self::Stop => "Stop", Self::Restart => "Restart", Self::Terminate => "Delete permanently" }
    }
    pub fn warning(self) -> &'static str {
        match self {
            Self::Start => "Compute billing resumes when the pod starts.",
            Self::Stop => "Interrupts work; container disk data may be lost.",
            Self::Restart => "Interrupts work and restarts the container in place.",
            Self::Terminate => "Permanently deletes this pod and its local volume disk.",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PodDetails {
    /// compute rate in USD per hour; storage is billed separately
    pub hourly_rate: f64,
    #[serde(default)]
    pub actions: Vec<String>,
    pub gpu_count: u32,
    pub gpus: Vec<Usage>,
    pub cpu_percent: Option<f64>,
    pub memory_percent: Option<f64>,
    pub ssh: Option<PodSsh>,
}

/// Deletion is only offered for pods in these states.
fn stopped(state: &str) -> bool {
    matches!(state, "EXITED" | "ERROR")
}

/// Pod ids are plain alphanumerics; anything else never goes into a URL.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl PodDetails {
    /// The API lists what a pod allows; on top of that, only stopped pods can be deleted.
    pub fn allows(&self, action: Action, state: &str) -> bool {
        self.actions.iter().any(|a| a == action.verb()) && (action != Action::Terminate || stopped(state))
    }

    /// What `x` does: stop, or delete once stopped.
    pub fn x_action(&self, state: &str) -> Option<Action> {
        let action = if stopped(state) { Action::Terminate } else { Action::Stop };
        self.allows(action, state).then_some(action)
    }

    /// What `R` does: restart, or start when stopped.
    pub fn restart_action(&self, state: &str) -> Option<Action> {
        [Action::Restart, Action::Start].into_iter().find(|&a| self.allows(a, state))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PodSsh {
    pub direct: Option<SshEndpoint>,
    pub proxy: Option<SshEndpoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SshEndpoint {
    pub host: String,
    pub port: u16,
    pub username: String,
}

impl SshEndpoint {
    /// Host and user come from the API. Only plain names reach ssh, where a user's ProxyCommand or
    /// Match exec could expand them into a shell command (%h, %r), as in CVE-2023-51385.
    pub fn plain(&self) -> bool {
        let ok = |s: &str, extra: &[u8]| {
            !s.is_empty() && !s.starts_with('-')
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b) || extra.contains(&b))
        };
        ok(&self.host, b":") && ok(&self.username, b"")
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.args(["-t", "-o", "ConnectTimeout=10", "-p", &self.port.to_string(), "-l", &self.username, "--", &self.host]);
        cmd
    }

    pub fn connect(&self) -> std::io::Result<()> {
        if !self.plain() {
            return Err(std::io::Error::other("Runpod sent an unexpected SSH host or user; not connecting"));
        }
        // ssh passes on the remote shell's exit code; only 255 is ssh itself failing to connect
        match self.command().status()?.code() {
            Some(255) => Err(std::io::Error::other("SSH couldn't connect; check the pod's SSH setup and your registered key")),
            _ => Ok(()),
        }
    }
}

/// One GPU's utilization, in percent.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub util: Option<f64>,
    pub memory_util: Option<f64>,
}

#[derive(Deserialize)]
struct Page {
    pods: Vec<Pod>,
    pagination: Option<Pagination>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pagination {
    next_cursor: Option<String>,
    has_next_page: bool,
}

#[derive(Deserialize)]
struct Pod {
    id: String,
    name: String,
    status: String,
    #[serde(default)]
    actions: Vec<String>,
    #[serde(default)]
    cost: f64,
    /// present for GPU Pods, absent for CPU Pods
    gpu: Option<Gpu>,
    /// only filled in when fetching a single running Pod
    runtime: Option<Runtime>,
    ssh: Option<PodSsh>,
}

#[derive(Deserialize)]
struct Gpu {
    id: String,
    count: u32,
}

#[derive(Deserialize)]
struct Runtime {
    uptime: Option<u64>,
    gpus: Option<Vec<Usage>>,
    cpu: Option<Util>,
    memory: Option<Util>,
}

#[derive(Deserialize)]
struct Util {
    util: Option<f64>,
}

fn key_from_config(text: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Credentials {
        apikey: Option<String>,
    }
    // Don't include TOML diagnostics: they can contain the line with the secret.
    let config: Credentials = toml::from_str(text).map_err(|_| "couldn't parse ~/.runpod/config.toml".to_string())?;
    config.apikey.filter(|k| !k.trim().is_empty()).ok_or_else(|| "missing apikey in ~/.runpod/config.toml".into())
}

fn api_key() -> Result<String, String> {
    if let Ok(key) = std::env::var("RUNPOD_API_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key);
    }
    if let Some(home) = std::env::var_os("HOME") {
        match std::fs::read_to_string(std::path::PathBuf::from(home).join(".runpod/config.toml")) {
            Ok(text) => return key_from_config(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("couldn't read ~/.runpod/config.toml".into()),
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(out) = Command::new("/usr/bin/security").args(["find-generic-password", "-s", "runpod", "-w"]).output()
        && out.status.success()
        && let Ok(key) = String::from_utf8(out.stdout)
        && !key.trim().is_empty()
    {
        return Ok(key.trim().to_string());
    }
    Err("set RUNPOD_API_KEY, apikey in ~/.runpod/config.toml, or the runpod Keychain entry".into())
}

pub fn fetch() -> Result<Snapshot, String> {
    fetch_from(API, &api_key()?)
}

fn fetch_from(api: &str, key: &str) -> Result<Snapshot, String> {
    let mut pods = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let query: Vec<(&str, &str)> = cursor.iter().map(|c| ("cursor", c.as_str())).collect();
        let page: Page = parse(&get(&format!("{api}/pods"), &query, key)?)?;
        pods.extend(page.pods);
        match page.pagination {
            Some(p) if p.has_next_page && p.next_cursor.is_some() => cursor = p.next_cursor,
            _ => break,
        }
    }
    // A Pod that stops or disappears between the two requests just shows no usage this time.
    for p in pods.iter_mut().filter(|p| p.status == "RUNNING" && plain_id(&p.id)) {
        if let Ok(full) = get(&format!("{api}/pods/{}", p.id), &[], key).and_then(|b| parse::<Pod>(&b)) {
            p.runtime = full.runtime;
            p.ssh = full.ssh.or(p.ssh.take());
        }
    }
    Ok(snapshot(pods))
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|_| "invalid Runpod response".to_string())
}

fn get(url: &str, query: &[(&str, &str)], key: &str) -> Result<Vec<u8>, String> {
    request(url, query, key, None)
}

pub fn act(id: &str, action: Action) -> Result<(), String> {
    act_from(API, &api_key()?, id, action)
}

fn act_from(api: &str, key: &str, id: &str, action: Action) -> Result<(), String> {
    if !plain_id(id) {
        return Err("invalid pod id".into());
    }
    if action == Action::Terminate {
        // Confirm against live state: a stopped row may have started elsewhere since the last poll.
        let pod: Pod = parse(&get(&format!("{api}/pods/{id}"), &[], key)?)?;
        if !stopped(&pod.status) || !pod.actions.iter().any(|a| a == Action::Terminate.verb()) {
            return Err("pod is no longer stopped or deletion is unavailable; refresh with r".into());
        }
    }
    request(&format!("{api}/pods/{id}/action"), &[], key, Some(action)).map(|_| ())
}

/// Requests through curl. `-q` skips ~/.curlrc, and the key goes in through stdin as a header
/// so it never shows up in `ps` or in curl's config parsing.
fn request(url: &str, query: &[(&str, &str)], key: &str, action: Option<Action>) -> Result<Vec<u8>, String> {
    let key = key.trim();
    if key.is_empty() || key.chars().any(char::is_control) {
        return Err("invalid Runpod API key".into());
    }
    let mut cmd = Command::new("curl");
    cmd.args(["-q", "-sS", "--max-time", "15", "--write-out", "\n%{http_code}", "-H", "@-"]);
    if let Some(action) = action {
        cmd.args(["-H", "Content-Type: application/json", "--data", &serde_json::json!({"action": action.verb()}).to_string()]);
    } else {
        cmd.arg("--get");
    }
    for (k, v) in query {
        cmd.args(["--data-urlencode", &format!("{k}={v}")]);
    }
    let mut child = cmd
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "couldn't run curl")?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "Authorization: Bearer {key}");
    }
    let out = child.wait_with_output().map_err(|_| "couldn't run curl")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).replace(key, "[redacted]");
        return Err(format!("Runpod: {}", err.trim().trim_start_matches("curl: ")));
    }
    let split = out.stdout.iter().rposition(|b| *b == b'\n').ok_or("missing Runpod HTTP status")?;
    let (body, status) = out.stdout.split_at(split);
    let status: u16 = std::str::from_utf8(&status[1..]).ok().and_then(|s| s.parse().ok()).ok_or("invalid Runpod HTTP status")?;
    if (200..300).contains(&status) {
        return Ok(body.to_vec());
    }
    let problem: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let detail = problem.get("detail").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty())
        .or_else(|| problem.get("title").and_then(|v| v.as_str()));
    let reason = if matches!(status, 401 | 403) {
        let scope = if action.is_some() { "read and write" } else { "read" };
        format!("Runpod rejected the API key; it needs {scope} access to Pods")
    } else if let Some(detail) = detail {
        detail.to_string()
    } else if status == 409 && action.is_some() {
        "pod state changed; refresh with r before trying again".into()
    } else {
        "request rejected; no explanation returned".into()
    };
    // Only show the API's message, never a raw response that could contain credentials.
    let reason: String = reason.replace(key, "[redacted]").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(400).collect();
    Err(format!("Runpod: {reason} (HTTP {status})"))
}

/// "NVIDIA GeForce RTX 4090" -> "RTX 4090", so it fits the gpu column.
fn short_gpu(id: &str) -> &str {
    id.trim_start_matches("NVIDIA ").trim_start_matches("GeForce ")
}

fn snapshot(pods: Vec<Pod>) -> Snapshot {
    let mut jobs: Vec<Job> = pods
        .into_iter()
        .map(|p| {
            let runtime = p.runtime;
            Job {
                id: p.id,
                name: p.name,
                state: p.status,
                used: runtime.as_ref().and_then(|r| r.uptime),
                limit: None,
                gpu_label: match &p.gpu {
                    Some(g) if g.count > 0 => format!("{}× {}", g.count, short_gpu(&g.id)),
                    _ => "CPU".into(),
                },
                nodes: String::new(),
                start: None,
                end: None,
                workdir: String::new(),
                reason: String::new(),
                partition: String::new(),
                exit: String::new(),
                gpus: Vec::new(),
                pod: Some(PodDetails {
                    hourly_rate: p.cost,
                    actions: p.actions,
                    gpu_count: p.gpu.map_or(0, |g| g.count),
                    cpu_percent: runtime.as_ref().and_then(|r| r.cpu.as_ref()?.util),
                    memory_percent: runtime.as_ref().and_then(|r| r.memory.as_ref()?.util),
                    gpus: runtime.and_then(|r| r.gpus).unwrap_or_default(),
                    ssh: p.ssh,
                }),
            }
        })
        .collect();
    jobs.sort_by(|a, b| b.running().cmp(&a.running()).then_with(|| a.name.cmp(&b.name)).then_with(|| a.id.cmp(&b.id)));
    Snapshot { jobs, outputs: HashMap::new() }
}

/// Pods on the way up pass through PROVISIONING and STARTING; only announce where they land.
fn settled(state: &str) -> bool {
    !matches!(state, "PROVISIONING" | "STARTING")
}

pub fn changes(host: &str, old: &[Job], new: &[Job], config: &crate::config::Config) -> Vec<(String, String)> {
    if !config.notifications {
        return Vec::new();
    }
    let body = |j: &Job| format!("{host} · {} · {}", j.id, j.gpu_label);
    let mut notes = Vec::new();
    for j in new {
        match old.iter().find(|o| o.id == j.id) {
            None if config.notify_on_queue => {
                notes.push((format!("New pod: {} ({})", j.name, j.state.to_lowercase()), body(j)))
            }
            Some(o) if o.state != j.state && settled(&j.state) && (!j.running() || config.notify_on_start) => {
                notes.push((format!("{}: {}", j.name, j.state.to_lowercase()), body(j)))
            }
            _ => {}
        }
    }
    for j in old.iter().filter(|o| !new.iter().any(|n| n.id == o.id)) {
        notes.push((format!("Pod removed: {}", j.name), body(j)));
    }
    notes
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn fixture() -> Snapshot {
        let page: Page = serde_json::from_value(serde_json::json!({"pods": [
            {"id": "stopped", "name": "Stopped", "status": "EXITED", "actions": ["start", "terminate"], "cost": 0.4,
             "gpu": {"id": "NVIDIA A40", "count": 1}, "runtime": null},
            {"id": "running", "name": "Training", "status": "RUNNING", "actions": ["stop", "restart", "terminate"], "cost": 1.2,
             "gpu": {"id": "NVIDIA H200", "count": 2},
             "runtime": {"uptime": 123, "gpus": [{"util": 0, "memoryUtil": null}], "cpu": {"util": 12}, "memory": {"util": 34}}},
            {"id": "starting", "name": "Starting", "status": "STARTING", "actions": ["stop", "terminate"], "cost": 1.2,
             "gpu": {"id": "NVIDIA GeForce RTX 4090", "count": 1}, "runtime": null},
            {"id": "cpu", "name": "CPU worker", "status": "RUNNING", "cost": 0.1, "runtime": {"uptime": 10}}
        ], "pagination": {"nextCursor": null, "hasNextPage": false}}))
        .unwrap();
        snapshot(page.pods)
    }

    #[test]
    fn parses_running_stopped_and_starting_without_inventing_readings() {
        let s = fixture();
        let find = |id: &str| s.jobs.iter().find(|j| j.id == id).unwrap();
        let j = find("running");
        assert_eq!(j.used, Some(123));
        assert_eq!(j.gpu_label, "2× H200");
        let p = j.pod.as_ref().unwrap();
        assert_eq!(p.hourly_rate, 1.2);
        assert_eq!((p.cpu_percent, p.memory_percent), (Some(12.0), Some(34.0)));
        assert_eq!(p.gpus[0].util, Some(0.0));
        assert_eq!(p.gpus[0].memory_util, None);
        assert_eq!(find("starting").state, "STARTING");
        assert_eq!(find("starting").gpu_label, "1× RTX 4090");
        assert_eq!(find("cpu").gpu_label, "CPU");
        assert_eq!(find("cpu").pod.as_ref().unwrap().cpu_percent, None);
        assert!(s.jobs.iter().all(|j| !j.finished()));
    }

    #[test]
    fn announces_new_changed_and_removed_pods_with_hardware() {
        let config = crate::config::Config::default();
        let jobs = fixture().jobs;
        let added = changes("runpod", &[], &jobs, &config);
        assert_eq!(added.len(), jobs.len());
        assert!(added.iter().any(|(t, b)| t.starts_with("New pod: Training") && b.ends_with("2× H200")));
        assert!(added.iter().any(|(t, b)| t.starts_with("New pod: CPU worker") && b.ends_with("CPU")));
        assert!(changes("runpod", &jobs, &jobs, &config).is_empty());

        let mut updated = jobs.clone();
        updated.retain(|j| j.id != "cpu");
        updated.iter_mut().find(|j| j.id == "starting").unwrap().state = "RUNNING".into();
        let notes = changes("runpod", &jobs, &updated, &config);
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|(t, _)| t == "Starting: running"));
        assert!(notes.iter().any(|(t, b)| t == "Pod removed: CPU worker" && b.ends_with("CPU")));

        // passing through STARTING on the way up is not news
        let mut restarting = jobs.clone();
        restarting.iter_mut().find(|j| j.id == "stopped").unwrap().state = "STARTING".into();
        assert!(changes("runpod", &jobs, &restarting, &config).is_empty());

        let disabled = crate::config::Config { notifications: false, ..config.clone() };
        assert!(changes("runpod", &jobs, &updated, &disabled).is_empty());
        let quiet = crate::config::Config { notify_on_queue: false, notify_on_start: false, ..config };
        assert!(changes("runpod", &[], &jobs, &quiet).is_empty());
        assert_eq!(changes("runpod", &jobs, &updated, &quiet).len(), 1, "removals still notify");
    }

    #[test]
    fn errors_never_echo_secrets() {
        assert_eq!(parse::<Page>(b"secret").err().unwrap(), "invalid Runpod response");
        assert!(!key_from_config("apikey = secret").unwrap_err().contains("secret"));
        assert_eq!(key_from_config("apikey = 'test-key'\nother = true").unwrap(), "test-key");
    }

    #[test]
    fn rejects_multiline_keys_before_sending_a_request() {
        for key in ["", " ", "secret\nX-Extra: value", "secret\rvalue"] {
            assert_eq!(fetch_from("http://127.0.0.1:1", key).err().unwrap(), "invalid Runpod API key");
        }
    }

    /// Serves `replies` in order, one connection each, and returns the request lines it saw.
    fn serve(replies: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                assert!(head.to_lowercase().contains(r#"authorization: bearer test-key"\end"#), "{head}");
                let mut request = head.lines().next().unwrap().to_string();
                let length = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                }).unwrap_or(0);
                if length > 0 {
                    assert!(head.to_lowercase().contains("content-type: application/json"));
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    request.push('\n');
                    request.push_str(std::str::from_utf8(&body).unwrap());
                }
                seen.push(request);
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            seen
        });
        (url, server)
    }

    #[test]
    fn actions_post_json_and_report_write_permissions() {
        for action in [Action::Start, Action::Stop, Action::Restart] {
            let (url, server) = serve(vec![(200, "{}".into())]);
            act_from(&url, r#"test-key"\end"#, "pod_123", action).unwrap();
            assert_eq!(server.join().unwrap(), [format!("POST /pods/pod_123/action HTTP/1.1\n{{\"action\":\"{}\"}}", action.verb())]);
        }
        for status in [403, 409, 500] {
            let (url, server) = serve(vec![(status, "{}".into())]);
            let err = act_from(&url, r#"test-key"\end"#, "pod_123", Action::Stop).unwrap_err();
            server.join().unwrap();
            assert!(err.contains(&status.to_string()));
            if status == 403 { assert!(err.contains("write access")); }
        }
        let (url, server) = serve(vec![(400, serde_json::json!({
            "title": "Bad Request", "detail": "Not enough capacity for test-key\"\\end"
        }).to_string())]);
        let err = act_from(&url, r#"test-key"\end"#, "pod_123", Action::Start).unwrap_err();
        server.join().unwrap();
        assert_eq!(err, "Runpod: Not enough capacity for [redacted] (HTTP 400)");
        assert!(act_from("http://127.0.0.1:1", "key", "../pods", Action::Stop).is_err());
        assert!(act_from("http://127.0.0.1:1", "key\nextra", "pod1", Action::Start).is_err());
    }

    #[test]
    fn deletion_rechecks_stopped_state_and_accepts_empty_204() {
        for (state, allowed) in [("EXITED", true), ("ERROR", true), ("RUNNING", true), ("STARTING", true), ("EXITED", false)] {
            let actions = if allowed { vec!["terminate"] } else { vec![] };
            let pod = serde_json::json!({"id":"pod_123", "name":"Pod", "status":state, "actions":actions}).to_string();
            let stopped = stopped(state) && allowed;
            let mut replies = vec![(200, pod)];
            if stopped { replies.push((204, String::new())); }
            let (url, server) = serve(replies);
            let result = act_from(&url, r#"test-key"\end"#, "pod_123", Action::Terminate);
            assert_eq!(result.is_ok(), stopped);
            let seen = server.join().unwrap();
            assert_eq!(seen[0], "GET /pods/pod_123 HTTP/1.1");
            if stopped {
                assert_eq!(seen[1], "POST /pods/pod_123/action HTTP/1.1\n{\"action\":\"terminate\"}");
            } else {
                assert_eq!(seen.len(), 1, "must never delete a pod that has started again");
            }
        }
    }

    #[test]
    fn ssh_uses_structured_fields_and_keeps_host_key_checks() {
        let endpoint: SshEndpoint = serde_json::from_value(serde_json::json!({
            "host": "ssh.runpod.io", "port": 22, "username": "pod-token",
            "command": "ssh -o StrictHostKeyChecking=no ignored; echo unsafe"
        })).unwrap();
        let command = endpoint.command();
        assert_eq!(command.get_program(), "ssh");
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, ["-t", "-o", "ConnectTimeout=10", "-p", "22", "-l", "pod-token", "--", "ssh.runpod.io"]);
        assert!(endpoint.plain());
        assert!(SshEndpoint { host: "2001:db8::1".into(), ..endpoint.clone() }.plain());

        // Names that a ProxyCommand or Match exec could turn into shell commands are never used.
        for (host, username) in [("$(touch x)", "root"), ("-oProxyCommand=x", "root"), ("h;x", "root"),
                                 ("ssh.runpod.io", "a`x`"), ("ssh.runpod.io", "-x"), ("", "root")] {
            let bad = SshEndpoint { host: host.into(), username: username.into(), ..endpoint.clone() };
            assert!(!bad.plain(), "{host} {username}");
            assert!(bad.connect().is_err());
        }
        let mut job = fixture().jobs.into_iter().find(|j| j.id == "running").unwrap();
        let bad = SshEndpoint { host: "x;y".into(), ..endpoint.clone() };
        job.pod.as_mut().unwrap().ssh = Some(PodSsh { proxy: Some(bad.clone()), direct: Some(endpoint) });
        assert_eq!(job.pod_ssh().unwrap().host, "ssh.runpod.io", "falls back to the valid endpoint");
        job.pod.as_mut().unwrap().ssh = Some(PodSsh { proxy: Some(bad), direct: None });
        assert!(job.pod_ssh().is_none());
    }

    #[test]
    fn follows_pages_and_reads_usage_of_running_pods_only() {
        let first = r#"{"pods":[{"id":"run1","name":"A","status":"RUNNING","cost":1.0,"gpu":{"id":"NVIDIA H100","count":1},"runtime":null}],
                       "pagination":{"nextCursor":"a+b/c=","hasNextPage":true}}"#;
        let second = r#"{"pods":[{"id":"off1","name":"B","status":"EXITED","cost":0.5,"runtime":null}],
                        "pagination":{"nextCursor":null,"hasNextPage":false}}"#;
        let detail = r#"{"id":"run1","name":"A","status":"RUNNING","cost":1.0,"gpu":{"id":"NVIDIA H100","count":1},
                        "ssh":{"direct":{"host":"192.0.2.1","port":22001,"username":"root"},"proxy":null},
                        "runtime":{"uptime":60,"gpus":[{"util":87,"memoryUtil":40}],"cpu":{"util":5},"memory":{"util":9}}}"#;
        let (url, server) = serve(vec![(200, first.into()), (200, second.into()), (200, detail.into())]);
        let snap = fetch_from(&url, r#"test-key"\end"#).unwrap();
        let seen = server.join().unwrap();
        assert_eq!(seen, ["GET /pods HTTP/1.1", "GET /pods?cursor=a%2bb%2fc%3d HTTP/1.1", "GET /pods/run1 HTTP/1.1"]);
        let run = snap.jobs.iter().find(|j| j.id == "run1").unwrap();
        assert_eq!(run.used, Some(60));
        assert_eq!(run.pod_ssh().unwrap().port, 22001);
        assert_eq!(run.pod.as_ref().unwrap().gpus[0].util, Some(87.0));
        assert_eq!(snap.jobs.len(), 2);
    }

    #[test]
    fn skips_usage_for_ids_that_are_not_plain() {
        let pod = serde_json::json!({"id": "pod /?#", "name": "Pod", "status": "RUNNING"});
        let list = serde_json::json!({"pods": [pod], "pagination": {"hasNextPage": false, "nextCursor": null}});
        let (url, server) = serve(vec![(200, list.to_string())]);
        let snap = fetch_from(&url, r#"test-key"\end"#).unwrap();
        assert_eq!(server.join().unwrap(), ["GET /pods HTTP/1.1"]);
        assert_eq!(snap.jobs[0].used, None);
    }

    #[test]
    fn reports_http_errors() {
        for status in [401, 429] {
            let (url, server) = serve(vec![(status, r#"{"title":"nope"}"#.into())]);
            let err = fetch_from(&url, r#"test-key"\end"#).err().unwrap();
            server.join().unwrap();
            assert!(err.contains(&status.to_string()), "{err}");
            assert_eq!(err.contains("API key"), status == 401, "{err}");
        }
    }
}
